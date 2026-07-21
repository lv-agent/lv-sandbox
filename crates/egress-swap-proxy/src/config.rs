//! cr-12 G2: swap-proxy 配置(全 env)。reference 实现 = 单 sentinel → 单 real token。
//!
//! env 契约(operator 配齐两侧):
//! - `FIXUS_SWAP_LISTEN`        : 监听地址,默认 `127.0.0.1:8443`。
//! - `FIXUS_SWAP_SENTINEL`      : 期望的 sentinel(必填;与牢侧 `FIXUS_GIT_SENTINEL` 同值)。
//! - `FIXUS_SWAP_TOKEN`         : 真 token(必填;只在本进程内)。
//! - `FIXUS_SWAP_UPSTREAM`      : 真 upstream `host:port`,默认 `github.com:443`。
//! - `FIXUS_SWAP_CERT_PEM`      : 入站 TLS 证书 PEM 内容(缺则运行期自签,仅测试用)。
//! - `FIXUS_SWAP_KEY_PEM`       : 入站 TLS 私钥 PEM 内容。
//! - `FIXUS_SWAP_UPSTREAM_CA_PEM`: upstream CA PEM 内容(缺则 webpki-roots)。

#[derive(Debug, Clone)]
pub struct SwapConfig {
    pub listen: String,
    pub sentinel: String,
    pub real_token: String,
    pub upstream: String,
    pub cert_pem: Option<String>,
    pub key_pem: Option<String>,
    pub upstream_ca_pem: Option<String>,
}

impl SwapConfig {
    pub fn from_env() -> Result<Self, String> {
        let sentinel = std::env::var("FIXUS_SWAP_SENTINEL")
            .map_err(|_| "FIXUS_SWAP_SENTINEL required".to_string())?;
        let real_token = std::env::var("FIXUS_SWAP_TOKEN")
            .map_err(|_| "FIXUS_SWAP_TOKEN required".to_string())?;
        Ok(Self {
            listen: std::env::var("FIXUS_SWAP_LISTEN")
                .unwrap_or_else(|_| "127.0.0.1:8443".to_string()),
            sentinel,
            real_token,
            upstream: std::env::var("FIXUS_SWAP_UPSTREAM")
                .unwrap_or_else(|_| "github.com:443".to_string()),
            cert_pem: std::env::var("FIXUS_SWAP_CERT_PEM")
                .ok()
                .filter(|s| !s.is_empty()),
            key_pem: std::env::var("FIXUS_SWAP_KEY_PEM")
                .ok()
                .filter(|s| !s.is_empty()),
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

    #[test]
    fn from_env_requires_sentinel_and_token() {
        let _g = SWAP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // 两者皆缺 → Err
        std::env::remove_var("FIXUS_SWAP_SENTINEL");
        std::env::remove_var("FIXUS_SWAP_TOKEN");
        assert!(
            SwapConfig::from_env().is_err(),
            "both absent → Err"
        );

        // 只 sentinel → Err(token 缺)
        std::env::set_var("FIXUS_SWAP_SENTINEL", "s");
        std::env::remove_var("FIXUS_SWAP_TOKEN");
        assert!(
            SwapConfig::from_env().is_err(),
            "token absent → Err"
        );

        // 只 token → Err(sentinel 缺)
        std::env::remove_var("FIXUS_SWAP_SENTINEL");
        std::env::set_var("FIXUS_SWAP_TOKEN", "t");
        assert!(
            SwapConfig::from_env().is_err(),
            "sentinel absent → Err"
        );

        // 两者皆设 → Ok 且默认值填齐
        std::env::set_var("FIXUS_SWAP_SENTINEL", "s");
        std::env::set_var("FIXUS_SWAP_TOKEN", "t");
        let cfg = SwapConfig::from_env().expect("both set → Ok");
        assert_eq!(cfg.sentinel, "s");
        assert_eq!(cfg.real_token, "t");
        assert_eq!(cfg.listen, "127.0.0.1:8443");
        assert_eq!(cfg.upstream, "github.com:443");

        // 清理(不污染同进程其它测试)
        std::env::remove_var("FIXUS_SWAP_SENTINEL");
        std::env::remove_var("FIXUS_SWAP_TOKEN");
    }
}
