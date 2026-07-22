//! cr-12 G2: 头部改写核心。纯函数,便于单测。
//!
//! 故意不做完整 HTTP 解析:helper 每请求新拨一条连接 + 发 `Connection: close`,
//! 故 swap-proxy 可一请求一连接,只改写头部块到 `\r\n\r\n` 为止的两行:
//! `Authorization`(sentinel→real)+ `Host`(→ upstream host)。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SwapError {
    #[error("missing Authorization header")]
    MissingAuthorization,
    #[error("sentinel mismatch")]
    SentinelMismatch,
    #[error("multiple Authorization headers")]
    MultipleAuthorization,
    #[error("malformed header block")]
    MalformedHeaderBlock,
}

/// 在 HTTP/1.1 头部块(到 `\r\n\r\n` 为止,不含终止空行)里改写两行:
///
/// 1. **Authorization**:`authorization: bearer <sentinel>`(scheme 大小写不敏感、
///    token **大小写敏感**)→ `Authorization: Bearer <real_token>`。
///    - cr-12 G2 review I-3:sentinel 是公开占位值(design §5),比较**不是常量时间** — by design。
///    - cr-12 G2 review I-4:>1 个 Authorization 头 → `MultipleAuthorization`(注入风险,拒绝转发)。
///
/// 2. **Host**:`host: <whatever helper sent>` → `Host: <upstream_host>`。
///    - live 验证(真 github)发现:helper 发的 `Host` = swap-proxy 地址(`localhost:<port>`)。
///      若原样转发给 github,github 按 `Host` 虚拟路由 → 301 重定向 → helper 不跟重定向 → 失败。
///      G2 本地集成测试没暴露此问题(本地 git-http-backend CGI 不查 Host)。
///      故转发前必须把 Host 重写为真 upstream host(`cfg.upstream` 的 host 部分)。
///
/// 返回改写后的头部块(原样保留 request line + 其余行与字节序)。
pub fn rewrite_request_headers(
    header_block: &[u8],
    sentinel: &str,
    real_token: &str,
    upstream_host: &str,
) -> Result<Vec<u8>, SwapError> {
    let block = std::str::from_utf8(header_block).map_err(|_| SwapError::MalformedHeaderBlock)?;
    // 首行是 request line;其余每行一个 header。
    let mut lines: Vec<String> = block.split("\r\n").map(|s| s.to_string()).collect();
    if lines.is_empty() {
        return Err(SwapError::MalformedHeaderBlock);
    }

    // I-4: 先数 Authorization 头个数(跳过 request line)。>1 → 拒绝(注入风险)。
    let auth_count = lines
        .iter()
        .skip(1)
        .filter(|line| {
            line.split_once(':')
                .map(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
                .unwrap_or(false)
        })
        .count();
    if auth_count > 1 {
        return Err(SwapError::MultipleAuthorization);
    }

    // 单遍:同时处理 Authorization(sentinel→real)与 Host(→upstream_host)。
    // auth_count ≤ 1 已保证,故至多一行 Authorization(无 break 需求;Host 可能在其后)。
    let mut auth_found = false;
    for line in lines.iter_mut().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("authorization") {
                let v = value.trim();
                // I-3: scheme 大小写不敏感,token(case-sensitive)严格相等。
                match v.split_once(char::is_whitespace) {
                    Some((scheme, token)) => {
                        let token = token.trim();
                        if scheme.eq_ignore_ascii_case("bearer") && token == sentinel {
                            *line = format!("Authorization: Bearer {real_token}");
                            auth_found = true;
                        } else {
                            return Err(SwapError::SentinelMismatch);
                        }
                    }
                    None => return Err(SwapError::SentinelMismatch),
                }
            } else if name.eq_ignore_ascii_case("host") {
                *line = format!("Host: {upstream_host}");
            }
        }
    }
    if !auth_found {
        return Err(SwapError::MissingAuthorization);
    }
    Ok(lines.join("\r\n").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用 upstream host(Host 重写后的期望值)。
    const UP_HOST: &str = "github.example";

    fn block(req_line: &str, auth: Option<&str>) -> Vec<u8> {
        let mut lines: Vec<String> = vec![req_line.to_string()];
        if let Some(a) = auth {
            lines.push(format!("Authorization: {a}"));
        }
        lines.push("Host: x".to_string());
        lines.join("\r\n").into_bytes()
    }

    /// 两个 Authorization 头的块(用于 I-4 测试)。
    fn block_two_auth(req_line: &str, auth1: &str, auth2: &str) -> Vec<u8> {
        [
            req_line.to_string(),
            format!("Authorization: {auth1}"),
            format!("Authorization: {auth2}"),
            "Host: x".to_string(),
        ]
        .join("\r\n")
        .into_bytes()
    }

    #[test]
    fn rewrites_matching_sentinel() {
        let b = block("GET /info/refs HTTP/1.1", Some("Bearer sent-XYZ"));
        let out = rewrite_request_headers(&b, "sent-XYZ", "real-TOKEN", UP_HOST).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Authorization: Bearer real-TOKEN"), "{s}");
        assert!(!s.contains("sent-XYZ"), "sentinel must not survive: {s}");
        assert!(s.contains("GET /info/refs HTTP/1.1"), "request line preserved");
        // Host 被重写为 upstream host(不再是 helper 发的 "x")。
        assert!(s.contains("Host: github.example"), "Host must be rewritten: {s}");
        assert!(!s.contains("Host: x"), "original Host must not survive: {s}");
    }

    #[test]
    fn case_insensitive_scheme_but_exact_token() {
        // scheme `bearer`(小写)+ 精确 token → 通过。
        let b = block("POST /g HTTP/1.1", Some("bearer sent-XYZ"));
        let out = rewrite_request_headers(&b, "sent-XYZ", "real", UP_HOST).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Authorization: Bearer real"));
    }

    #[test]
    fn token_case_sensitive() {
        // I-3:token 大小写敏感。SENT-XYZ != sent-XYZ → SentinelMismatch。
        let b = block("GET / HTTP/1.1", Some("Bearer SENT-XYZ"));
        assert!(matches!(
            rewrite_request_headers(&b, "sent-XYZ", "real", UP_HOST),
            Err(SwapError::SentinelMismatch)
        ));
    }

    #[test]
    fn mismatched_sentinel_errors() {
        let b = block("GET / HTTP/1.1", Some("Bearer wrong"));
        assert!(matches!(
            rewrite_request_headers(&b, "sent-XYZ", "real", UP_HOST),
            Err(SwapError::SentinelMismatch)
        ));
    }

    #[test]
    fn missing_authorization_errors() {
        let b = block("GET / HTTP/1.1", None);
        assert!(matches!(
            rewrite_request_headers(&b, "sent-XYZ", "real", UP_HOST),
            Err(SwapError::MissingAuthorization)
        ));
    }

    #[test]
    fn multiple_authorization_rejected() {
        // I-4:两个 Authorization 头(哪怕第一个匹配 sentinel)→ 拒绝,不转发。
        let b = block_two_auth(
            "GET /info/refs HTTP/1.1",
            "Bearer sent-XYZ",
            "Bearer attacker-injected",
        );
        assert!(matches!(
            rewrite_request_headers(&b, "sent-XYZ", "real", UP_HOST),
            Err(SwapError::MultipleAuthorization)
        ));
    }

    #[test]
    fn host_header_rewritten_to_upstream() {
        // live 验证(真 github):Host 必须重写为 upstream host,否则 github 301/404。
        // 覆盖 Host 在 Authorization 之后、不同大小写名("host")两种形态。
        let raw = "GET /info/refs HTTP/1.1\r\n\
                   Authorization: Bearer sent-XYZ\r\n\
                   host: localhost:9999\r\n";
        let out = rewrite_request_headers(raw.as_bytes(), "sent-XYZ", "real", "github.com").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Host: github.com"), "Host rewritten to upstream: {s}");
        assert!(!s.contains("localhost:9999"), "original host:port must not survive: {s}");
    }
}
