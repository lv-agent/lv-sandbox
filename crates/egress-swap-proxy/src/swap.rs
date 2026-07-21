//! cr-12 G2: 头部改写核心。纯函数,便于单测。
//!
//! 故意不做完整 HTTP 解析:helper 每请求新拨一条连接 + 发 `Connection: close`,
//! 故 swap-proxy 可一请求一连接,只改写头部块到 `\r\n\r\n` 为止的 `Authorization` 行。

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

/// 在 HTTP/1.1 头部块(到 `\r\n\r\n` 为止,不含终止空行)里:
/// 找 `authorization: bearer <sentinel>`(scheme 大小写不敏感、token **大小写敏感**),
/// 改写成 `Authorization: Bearer <real_token>`。返回改写后的头部块(原样保留其余行与字节序)。
///
/// cr-12 G2 review I-3:sentinel 是公开占位值(design §5),故比较**不是常量时间** — by design。
/// scheme(`bearer`)大小写不敏感(RFC 7235),token 部分大小写敏感(operator 两侧配同值,精确比较;
/// 顺带避免大小写折叠压缩 sentinel 字符集的有效熵)。
///
/// cr-12 G2 review I-4:出现 >1 个 Authorization 头 → `MultipleAuthorization`(注入风险,拒绝转发)。
pub fn rewrite_authorization(
    header_block: &[u8],
    sentinel: &str,
    real_token: &str,
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

    // request line(行 0)不动;从行 1 找 Authorization。
    let mut found = false;
    for line in lines.iter_mut().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("authorization") {
                let v = value.trim();
                // I-3: scheme 大小写不敏感,token(case-sensitive)严格相等。
                //   拆 "scheme SP token";无空格 / 无 token → SentinelMismatch。
                match v.split_once(char::is_whitespace) {
                    Some((scheme, token)) => {
                        let token = token.trim();
                        if scheme.eq_ignore_ascii_case("bearer") && token == sentinel {
                            *line = format!("Authorization: Bearer {real_token}");
                            found = true;
                        } else {
                            return Err(SwapError::SentinelMismatch);
                        }
                    }
                    None => return Err(SwapError::SentinelMismatch),
                }
                break;
            }
        }
    }
    if !found {
        return Err(SwapError::MissingAuthorization);
    }
    Ok(lines.join("\r\n").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = rewrite_authorization(&b, "sent-XYZ", "real-TOKEN").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Authorization: Bearer real-TOKEN"), "{s}");
        assert!(!s.contains("sent-XYZ"), "sentinel must not survive: {s}");
        assert!(s.contains("GET /info/refs HTTP/1.1"), "request line preserved");
        assert!(s.contains("Host: x"), "other headers preserved");
    }

    #[test]
    fn case_insensitive_scheme_but_exact_token() {
        // scheme `bearer`(小写)+ 精确 token → 通过。
        let b = block("POST /g HTTP/1.1", Some("bearer sent-XYZ"));
        let out = rewrite_authorization(&b, "sent-XYZ", "real").unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Authorization: Bearer real"));
    }

    #[test]
    fn token_case_sensitive() {
        // I-3:token 大小写敏感。SENT-XYZ != sent-XYZ → SentinelMismatch。
        let b = block("GET / HTTP/1.1", Some("Bearer SENT-XYZ"));
        assert!(matches!(
            rewrite_authorization(&b, "sent-XYZ", "real"),
            Err(SwapError::SentinelMismatch)
        ));
    }

    #[test]
    fn mismatched_sentinel_errors() {
        let b = block("GET / HTTP/1.1", Some("Bearer wrong"));
        assert!(matches!(
            rewrite_authorization(&b, "sent-XYZ", "real"),
            Err(SwapError::SentinelMismatch)
        ));
    }

    #[test]
    fn missing_authorization_errors() {
        let b = block("GET / HTTP/1.1", None);
        assert!(matches!(
            rewrite_authorization(&b, "sent-XYZ", "real"),
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
            rewrite_authorization(&b, "sent-XYZ", "real"),
            Err(SwapError::MultipleAuthorization)
        ));
    }
}
