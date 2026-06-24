//! 输出脱敏：stdout/stderr 返回前剔除常见密钥/token 模式（cr-018+#78）。
//!
//! 防止任务输出（误读 `~/.aws/credentials`、环境变量、配置文件等）把真实凭证
//! 带进 agent 上下文。默认规则覆盖：Bearer token、AWS 访问密钥、GitHub token、私钥。

use once_cell::sync::Lazy;
use regex::Regex;

/// 内置脱敏规则（顺序敏感：多行/长模式优先匹配）
static RULES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    vec![
        // 私钥（PEM，多行）
        (
            Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
            "[REDACTED PRIVATE KEY]",
        ),
        // Bearer token
        (
            Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]+").unwrap(),
            "bearer [REDACTED]",
        ),
        // AWS 访问密钥（AKIA + 16 位大写字母数字）
        (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "AKIA[REDACTED]"),
        // GitHub token（ghp_/ghs_/gho_/ghr_/ghu_ + 36 位）
        (Regex::new(r"gh[psoru]_[A-Za-z0-9]{36}").unwrap(), "[REDACTED]"),
    ]
});

/// 脱敏输入字符串：把常见密钥/token 模式替换为 `[REDACTED]`。
pub fn redact(input: &str) -> String {
    let mut s = input.to_string();
    for (re, rep) in RULES.iter() {
        s = re.replace_all(&s, *rep).to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_redacted() {
        let r = redact("Authorization: Bearer eyJhbGc.iOiJKV1QiLCJ.abc123");
        assert!(r.contains("[REDACTED]"), "Bearer should be redacted: {r}");
        assert!(!r.contains("eyJhbGc"), "token content should not leak: {r}");
    }

    #[test]
    fn aws_access_key_redacted() {
        let r = redact("aws_access_key_id = AKIAIOSFODNN7EXAMPLE");
        assert!(r.contains("[REDACTED]"), "AKIA should be redacted: {r}");
        assert!(!r.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn github_token_redacted() {
        let r = redact("token = ghp_1234567890abcdefghijklmnopqrstuvwxyz");
        assert!(r.contains("[REDACTED]"), "gh token should be redacted: {r}");
        assert!(!r.contains("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn private_key_redacted() {
        let s = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let r = redact(s);
        assert!(r.contains("REDACTED"), "private key should be redacted: {r}");
        assert!(!r.contains("MIIE"));
    }

    #[test]
    fn no_secrets_passed_through() {
        let r = redact("hello world\nnormal output 42");
        assert_eq!(r, "hello world\nnormal output 42");
    }
}
