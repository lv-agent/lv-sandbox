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
    #[error("malformed header block")]
    MalformedHeaderBlock,
}

/// 在 HTTP/1.1 头部块(到 `\r\n\r\n` 为止,不含终止空行)里:
/// 找 `authorization: bearer <sentinel>`(大小写不敏感),改写成 `Authorization: Bearer <real_token>`。
/// 返回改写后的头部块(原样保留其余行与字节序,仅替换该行)。
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
    // request line(行 0)不动;从行 1 找 Authorization。
    let mut found = false;
    for line in lines.iter_mut().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("authorization") {
                let v = value.trim();
                let want = format!("Bearer {sentinel}");
                if v.eq_ignore_ascii_case(&want) {
                    *line = format!("Authorization: Bearer {real_token}");
                    found = true;
                    break;
                } else {
                    return Err(SwapError::SentinelMismatch);
                }
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
    fn case_insensitive_header_name_and_value() {
        let b = block("POST /g HTTP/1.1", Some("bearer sent-XYZ"));
        let out = rewrite_authorization(&b, "sent-XYZ", "real").unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Authorization: Bearer real"));
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
}
