//! sandbox-broker — fixus broker bridge(cr-087)。
//!
//! 以 broker GroupConsumer 身份消费 fixus `tool-invoke-{region}`,翻成 lv-sandbox
//! HTTP(session exec + 文件 API),再产 `tool-result-{region}`。fixus 零改动。

mod error;
mod lv_client;

use clap::Parser;

#[derive(Parser)]
#[command(name = "sandbox-broker", version)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:5100")]
    broker_addr: String,
    #[arg(long, default_value = "default")]
    namespace: String,
    #[arg(long, default_value = "default")]
    region: String,
    #[arg(long, default_value = "sandboxes")]
    group: String,
    /// lv-sandbox server HTTP base
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    sandbox_url: String,
    /// 可选 Bearer api_key(server 侧 cr-023)
    #[arg(long)]
    sandbox_api_key: Option<String>,
    /// session 绑定 profile
    #[arg(long, default_value = "shell")]
    profile: String,
    /// session 生命周期 + TTL 回收阈值(秒)
    #[arg(long, default_value = "3600")]
    session_timeout_secs: u64,
    /// 客户端背压 semaphore
    #[arg(long, default_value = "4")]
    concurrency: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sandbox_broker=info".into()),
        )
        .init();
    let cli = Cli::parse();
    tracing::info!(
        "sandbox-broker stub starting: broker={} region={} group={} sandbox={} profile={}",
        cli.broker_addr, cli.region, cli.group, cli.sandbox_url, cli.profile
    );
}
