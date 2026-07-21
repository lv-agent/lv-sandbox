//! cr-12 G2: swap-proxy 配置(全 env)。reference 实现 = 单 sentinel → 单 real token。
//!
//! env 契约(operator 配齐两侧):
//! - `FIXUS_SWAP_LISTEN`        : 监听地址,默认 `127.0.0.1:8443`。
//! - `FIXUS_SWAP_SENTINEL`      : 期望的 sentinel(必填;与牢侧 `FIXUS_GIT_SENTINEL` 同值)。
//! - `FIXUS_SWAP_TOKEN`         : 真 token(必填;只在本进程内)。
//! - `FIXUS_SWAP_UPSTREAM`      : 真 upstream `host:port`,默认 `github.com:443`。
//! - `FIXUS_SWAP_CERT_PEM`      : 入站 TLS 证书 PEM 内容(必填;**不再**做自签回退 = fail-closed)。
//! - `FIXUS_SWAP_KEY_PEM`       : 入站 TLS 私钥 PEM 内容(必填)。
//! - `FIXUS_SWAP_UPSTREAM_CA_PEM`: upstream CA PEM 内容(缺则 webpki-roots)。

/// 不 derive `Debug`:`sentinel` / `real_token` / `key_pem` 是敏感值,
/// 一个无心的 `tracing::info!(?cfg)` 或 panic backtrace 会把真凭据泄进日志(design §5 不变量 2)。
#[derive(Clone)]
pub struct SwapConfig {
    pub listen: String,
    pub sentinel: String,
    pub real_token: String,
    pub upstream: String,
    // I-5: cert/key 必填(生产 fail-closed;测试用 tests/common 生成 + 经 env 注入)。
    pub cert_pem: String,
    pub key_pem: String,
    pub upstream_ca_pem: Option<String>,
}

impl SwapConfig {
    pub fn from_env() -> Result<Self, String> {
        let sentinel = std::env::var("FIXUS_SWAP_SENTINEL")
            .map_err(|_| "FIXUS_SWAP_SENTINEL required".to_string())?;
        let real_token = std::env::var("FIXUS_SWAP_TOKEN")
            .map_err(|_| "FIXUS_SWAP_TOKEN required".to_string())?;

        // M-3: CR/LF 卫生。sentinel/token 出现在改写后的 Authorization 头里,
        // 含换行 = 头注入(往上游塞任意行)。operator 配错 → fail-fast。
        for (name, val) in [
            ("FIXUS_SWAP_SENTINEL", &sentinel),
            ("FIXUS_SWAP_TOKEN", &real_token),
        ] {
            if val.contains('\r') || val.contains('\n') {
                return Err(format!("{name} must not contain CR or LF (header injection)"));
            }
        }

        // I-5: cert/key 必填(无自签回退)。production 必须由 operator 提供证。
        let cert_pem = std::env::var("FIXUS_SWAP_CERT_PEM")
            .map_err(|_| "FIXUS_SWAP_CERT_PEM required (no self-signed fallback)".to_string())?;
        if cert_pem.is_empty() {
            return Err("FIXUS_SWAP_CERT_PEM must not be empty".to_string());
        }
        let key_pem = std::env::var("FIXUS_SWAP_KEY_PEM")
            .map_err(|_| "FIXUS_SWAP_KEY_PEM required (no self-signed fallback)".to_string())?;
        if key_pem.is_empty() {
            return Err("FIXUS_SWAP_KEY_PEM must not be empty".to_string());
        }

        Ok(Self {
            listen: std::env::var("FIXUS_SWAP_LISTEN")
                .unwrap_or_else(|_| "127.0.0.1:8443".to_string()),
            sentinel,
            real_token,
            upstream: std::env::var("FIXUS_SWAP_UPSTREAM")
                .unwrap_or_else(|_| "github.com:443".to_string()),
            cert_pem,
            key_pem,
            upstream_ca_pem: std::env::var("FIXUS_SWAP_UPSTREAM_CA_PEM")
                .ok()
                .filter(|s| !s.is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程 env 读写的测试互斥(与 sandbox-core profile.rs 的 GIT_ENV_LOCK 同模式)。
    /// cargo 默认多线程跑测试;动 env 必须序列化,否则互相踩。
    static SWAP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 全清 swap 相关 env,测试起手干净。
    fn clear_env() {
        for k in [
            "FIXUS_SWAP_SENTINEL",
            "FIXUS_SWAP_TOKEN",
            "FIXUS_SWAP_CERT_PEM",
            "FIXUS_SWAP_KEY_PEM",
            "FIXUS_SWAP_UPSTREAM_CA_PEM",
        ] {
            std::env::remove_var(k);
        }
    }

    /// 配齐必填 env。
    fn set_required(sentinel: &str, token: &str) {
        std::env::set_var("FIXUS_SWAP_SENTINEL", sentinel);
        std::env::set_var("FIXUS_SWAP_TOKEN", token);
        std::env::set_var("FIXUS_SWAP_CERT_PEM", "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n");
        std::env::set_var("FIXUS_SWAP_KEY_PEM", "-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----\n");
    }

    #[test]
    fn from_env_requires_sentinel_and_token() {
        let _g = SWAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        clear_env();
        // 两者皆缺 → Err
        assert!(SwapConfig::from_env().is_err(), "both absent → Err");

        // 只 sentinel → Err(token 缺)
        std::env::set_var("FIXUS_SWAP_SENTINEL", "s");
        assert!(SwapConfig::from_env().is_err(), "token absent → Err");

        // 只 token → Err(sentinel 缺)
        clear_env();
        std::env::set_var("FIXUS_SWAP_TOKEN", "t");
        assert!(SwapConfig::from_env().is_err(), "sentinel absent → Err");

        // sentinel + token 但无 cert/key → Err(I-5:cert/key 必填)
        clear_env();
        std::env::set_var("FIXUS_SWAP_SENTINEL", "s");
        std::env::set_var("FIXUS_SWAP_TOKEN", "t");
        assert!(
            SwapConfig::from_env().is_err(),
            "cert/key absent → Err (no self-signed fallback)"
        );

        // 全配齐 → Ok 且默认值填齐
        clear_env();
        set_required("s", "t");
        let cfg = SwapConfig::from_env().expect("all required set → Ok");
        assert_eq!(cfg.sentinel, "s");
        assert_eq!(cfg.real_token, "t");
        assert_eq!(cfg.listen, "127.0.0.1:8443");
        assert_eq!(cfg.upstream, "github.com:443");

        clear_env();
    }

    #[test]
    fn from_env_rejects_crlf_in_sentinel_and_token() {
        // M-3:CR/LF 在 sentinel/token 里 = 头注入 → 拒绝。
        let _g = SWAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        clear_env();
        set_required("sent\r\nX-Evil: yes", "real");
        assert!(
            SwapConfig::from_env().is_err(),
            "CR/LF in sentinel → Err"
        );

        clear_env();
        set_required("sent", "real\nX-Evil: yes");
        assert!(
            SwapConfig::from_env().is_err(),
            "CR/LF in token → Err"
        );

        clear_env();
    }
}
