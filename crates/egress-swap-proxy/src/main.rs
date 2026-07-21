//! cr-12 G2 reference swap-proxy binary: 牢外 sentinel→real 兑换。
//!
//! 牢内 helper(`git-remote-fixus`)→ UDS→SOCKS5h(allowlist 收口到本代理)
//! → TLS → 本代理。本代理 TLS 终结(信任边界),识别 `Authorization: Bearer <sentinel>`,
//! 改写成 `Authorization: Bearer <real_token>`,再 TLS 连真上游(github.com:443)转发。
//!
//! Thin binary:解析 env(`SwapConfig::from_env`) + 构造 TLS 配置 + 绑端口 +
//! `egress_swap_proxy::server::serve(listener, ...)`。
//! accept 循环 / `handle_conn` / TLS 配置构造在 `src/server.rs`(lib),让
//! Task 4 的 E2E 测试可直接驱动 `serve` 跑完整 TLS relay 路径,无需 spawn 子进程。

use std::sync::Arc;

use egress_swap_proxy::{config, server};
use tokio::net::TcpListener;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = match config::SwapConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!("egress-swap-proxy config error: {e}");
            std::process::exit(2);
        }
    };

    let server_cfg = match server::build_server_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            error!("egress-swap-proxy TLS server config error: {e}");
            std::process::exit(2);
        }
    };
    let upstream_cfg = Arc::new(server::build_upstream_client_config(&cfg)?);

    let listener = TcpListener::bind(&cfg.listen).await?;
    info!(
        listen = %cfg.listen,
        upstream = %cfg.upstream,
        "egress-swap-proxy starting (sentinel→real swap)"
    );

    let cfg = Arc::new(cfg);
    server::serve(listener, Arc::new(server_cfg), upstream_cfg, cfg).await?;
    Ok(())
}
