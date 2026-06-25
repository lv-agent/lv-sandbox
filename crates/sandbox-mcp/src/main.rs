//! sandbox-mcp 入口：MCP/stdio 网关。
//!
//! 以子进程方式被 AI Agent（Claude Code、Hermes-Agent）启动，
//! 通过 stdin/stdout 收发 JSON-RPC，转发请求到独立运行的 sandbox-server。
//!
//! 环境变量：
//! - `SANDBOX_SERVER_URL`：sandbox-server 地址（默认 `http://127.0.0.1:8080`）
//! - `SANDBOX_API_KEY`：cr-023,server 开启鉴权时须配同值 key（默认不配 = 不带）
//! - `RUST_LOG`：日志级别（默认 `info`）

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use tracing_subscriber::EnvFilter;

use sandbox_mcp::{SandboxHttpClient, SandboxMcpServer};

#[tokio::main]
async fn main() -> Result<()> {
    // 日志输出到 stderr（stdout 保留给 MCP JSON-RPC 协议）
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let base_url = std::env::var("SANDBOX_SERVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    // cr-023: server.api_key 有值(鉴权开)时,网关须带同值 key,否则被 401。
    let api_key = std::env::var("SANDBOX_API_KEY").ok().filter(|s| !s.is_empty());
    tracing::info!(base_url = %base_url, api_key_set = api_key.is_some(), "sandbox-mcp gateway starting, connecting to sandbox-server");

    let http = SandboxHttpClient::new(&base_url)?.with_api_key(api_key);
    let server = SandboxMcpServer::new(http);

    // 通过 stdio 提供 MCP 服务
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
