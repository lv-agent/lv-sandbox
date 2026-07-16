//! sandbox-broker — fixus broker bridge(cr-087)。
//! 消费 tool-invoke-{region} → translate → lv-sandbox HTTP → 产 tool-result-{region}。

mod error;
mod idem;
mod lv_client;
mod session_map;
mod translate;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

use logdb_client::broker::{BrokerProducer, GroupConsumer};
use logdb_broker_proto::pb::consume_response::Payload;

use idem::IdempotentCache;
use lv_client::LvClient;
use session_map::SessionMap;

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
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    sandbox_url: String,
    #[arg(long)]
    sandbox_api_key: Option<String>,
    #[arg(long, default_value = "shell")]
    profile: String,
    #[arg(long, default_value = "3600")]
    session_timeout_secs: u64,
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

    let http: Arc<dyn lv_client::SandboxHttp> = LvClient::new(
        cli.sandbox_url.clone(),
        cli.sandbox_api_key.clone(),
        Duration::from_secs(300),
    ).arc();
    let sessions = Arc::new(SessionMap::new(cli.profile.clone(), cli.session_timeout_secs));
    let cache = Arc::new(IdempotentCache::new());

    let stream = format!("tool-invoke-{}", cli.region);
    let result_stream = format!("tool-result-{}", cli.region);
    let consumer_id = format!("sandbox-broker-{}", uuid::Uuid::new_v4().simple());

    tracing::info!("sandbox-broker starting: broker={} stream={} group={} consumer={} sandbox={}",
        cli.broker_addr, stream, cli.group, consumer_id, cli.sandbox_url);

    loop {
        let result_producer = match BrokerProducer::connect(format!("http://{}", cli.broker_addr)).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("result producer connect failed: {}; retry in 1s", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        match run_consumer(&cli, &stream, &result_stream, &consumer_id, http.clone(), sessions.clone(), cache.clone(), result_producer).await {
            Ok(()) => tracing::info!("consumer loop ended normally"),
            Err(e) => {
                tracing::error!("consumer error: {}; retry in 1s", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_consumer(
    cli: &Cli,
    stream: &str,
    result_stream: &str,
    consumer_id: &str,
    http: Arc<dyn lv_client::SandboxHttp>,
    sessions: Arc<SessionMap>,
    cache: Arc<IdempotentCache>,
    result_producer: BrokerProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("http://{}", cli.broker_addr);
    let mut consumer = GroupConsumer::join(addr, &cli.namespace, stream, &cli.group, consumer_id).await?;
    tracing::info!("joined group {} ({}), shards: {:?}", cli.group, consumer_id, consumer.assigned_shards());
    let mut frames = consumer.consume_frames().await?;

    let semaphore = Arc::new(Semaphore::new(cli.concurrency.max(1)));
    let producer = Arc::new(tokio::sync::Mutex::new(result_producer));
    let (commit_tx, mut commit_rx) = tokio::sync::mpsc::unbounded_channel::<(u32, u64)>();

    let mut consecutive_errors: u32 = 0;
    while let Some(item) = frames.next().await {
        while let Ok((shard_id, seq)) = commit_rx.try_recv() {
            let _ = consumer.commit_shard(shard_id, seq).await;
        }
        let frame = match item {
            Ok(f) => { consecutive_errors = 0; f }
            Err(e) => {
                consecutive_errors += 1;
                tracing::error!("consume error (consecutive={}): {}", consecutive_errors, e);
                if consecutive_errors >= 3 {
                    return Err(format!("{} consecutive consume errors, rejoining", consecutive_errors).into());
                }
                continue;
            }
        };
        match frame.payload {
            Some(Payload::Record(rec)) => {
                if rec.event_type != "tool_invoked" { continue; }
                let payload: serde_json::Value = serde_json::from_slice(&rec.content).unwrap_or_default();
                let tool_name = payload["tool_name"].as_str().unwrap_or("?").to_string();
                let idempotency_key = payload["idempotency_key"].as_str().unwrap_or("?").to_string();
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("?").to_string();
                let step_id = rec.metadata.get("step_id").cloned().unwrap_or_default();
                let task_id = rec.metadata.get("task_id").cloned().unwrap_or_default();
                let shard_id = rec.shard_id;
                let seq = rec.seq;

                // cr-12:fixus 的 effective_policy JSON 声明 net 能力时,本 task 选 git egress profile
                // (Task 1 在 sandbox-core 加了 "git" profile)。fs host-path scope 仍不翻译(session jail,
                // 比 stub 严)。effective_policy 由 fixus 经 serde_json::to_string 写成 JSON 字符串。
                // 不返回对解析值的引用(Value 是临时量,跨闭包持有会悬垂),折成 bool 再 then_some。
                let profile_override = rec.metadata.get("effective_policy")
                    .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                    .is_some_and(|p| p.get("net").is_some())
                    .then_some("git");
                if profile_override.is_some() {
                    tracing::debug!(task = %task_id, "effective_policy net hint → git profile");
                }

                // 幂等命中:直接产缓存结果 + commit
                if let Some(cached) = cache.get(&idempotency_key).await {
                    tracing::info!("cache hit: {}", idempotency_key);
                    produce_result(&producer, &cli.namespace, result_stream, &step_id, &task_id, &tool_call_id, &tool_name, &cached).await;
                    let _ = consumer.commit_shard(shard_id, seq).await;
                    continue;
                }

                let input = payload.get("input").cloned().unwrap_or(serde_json::Value::Null);
                let timeout_ms = payload.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(120_000);
                let timeout_secs = (timeout_ms / 1000).clamp(1, 600);
                let session_id_src = payload["session_id"].as_str().unwrap_or(&task_id).to_string();

                let sem = semaphore.clone();
                let prod = producer.clone();
                let tx = commit_tx.clone();
                let ns = cli.namespace.clone();
                let rs = result_stream.to_string();
                let cache_clone = cache.clone();
                let http_clone = http.clone();
                let sessions_clone = sessions.clone();

                tokio::spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    let t0 = std::time::Instant::now();
                    let (success, output, error) = match translate::execute(
                        &tool_name, &input, timeout_secs, &session_id_src, &http_clone, &sessions_clone,
                        profile_override,
                    ).await {
                        Ok(o) => (o.success, o.output, o.error),
                        Err(e) => (false, serde_json::Value::Null, Some(format!("{}", e))),
                    };
                    let dur = t0.elapsed().as_millis() as u64;
                    let r = idem::ToolResult { success, output, error, duration_ms: dur };
                    tracing::info!("executed {} task={} success={} duration_ms={}", tool_name, task_id, r.success, dur);
                    cache_clone.put(idempotency_key.clone(), r.clone()).await;
                    produce_result(&prod, &ns, &rs, &step_id, &task_id, &tool_call_id, &tool_name, &r).await;
                    let _ = tx.send((shard_id, seq));
                });
            }
            Some(Payload::CaughtUp(_)) | Some(Payload::Rebalance(_)) | Some(Payload::Assignment(_)) => {}
            None => {}
        }
    }
    drop(commit_tx);
    while let Some((shard_id, seq)) = commit_rx.recv().await {
        let _ = consumer.commit_shard(shard_id, seq).await;
    }
    tracing::info!("all tasks done, leaving group");
    consumer.leave().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn produce_result(
    producer: &Arc<tokio::sync::Mutex<BrokerProducer>>,
    namespace: &str,
    result_stream: &str,
    step_id: &str,
    task_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    r: &idem::ToolResult,
) {
    let result_payload = serde_json::json!({
        "step_id": step_id, "task_id": task_id, "tool_call_id": tool_call_id, "tool_name": tool_name,
        "success": r.success, "output": r.output, "error": r.error, "duration_ms": r.duration_ms,
    });
    let content = serde_json::to_vec(&result_payload).unwrap_or_default();
    let mut meta = HashMap::new();
    meta.insert("step_id".into(), step_id.to_string());
    meta.insert("task_id".into(), task_id.to_string());
    meta.insert("event_type".into(), "tool_result".into());
    let mut p = producer.lock().await;
    if let Err(e) = p.produce_full(namespace, result_stream, "tool_result", &content, Some(step_id), 0, "application/json", &meta).await {
        tracing::error!("broker produce result failed: {}", e);
    }
}

/// cr-087 live integration:验证 LvClient(reqwest)+ translate 对真实 lv-sandbox server。
/// 需 server 在 :8080。`cargo test -p sandbox-broker live -- --ignored`(无 server 自动 skip)。
#[cfg(test)]
mod live {
    use super::*;
    use crate::translate;

    /// 探活:命中 lv-sandbox 专属路由 `/api/v1/profiles`(200 无鉴权 / 401 鉴权开)才算"对的 server 在"。
    /// 不用 `/health`——它 auth-exempt 且 shape 通用,任何 squat 在 :8080 的服务(如本机 mygate)
    /// 都会回 200,导致测试误闯入并 404 失败而非 skip。404 / 连接失败 → 判定不可用 → skip。
    /// 测试目标 server base URL。默认 :8080(lv-sandbox 默认);可用
    /// `SANDBOX_BRIDGE_TEST_URL` 覆盖(本地 :8080 被 squat 时跑别的端口)。
    fn base() -> String {
        std::env::var("SANDBOX_BRIDGE_TEST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
    }

    async fn sandbox_up() -> bool {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build().unwrap()
            .get(format!("{}/api/v1/profiles", base())).send().await
            .map(|r| r.status().is_success() || r.status().as_u16() == 401)
            .unwrap_or(false)
    }

    async fn lv() -> Arc<dyn lv_client::SandboxHttp> {
        LvClient::new(base(), None, std::time::Duration::from_secs(60)).arc()
    }

    #[tokio::test]
    #[ignore = "需 live lv-sandbox sandbox-server (:8080)"]
    async fn bash_exec_against_live_server() {
        if !sandbox_up().await {
            eprintln!("skip: lv-sandbox server not reachable on :8080");
            return;
        }
        let http = lv().await;
        let sessions = SessionMap::new("shell".into(), 300);
        let r = translate::execute(
            "fixus_bash",
            &serde_json::json!({"command": "echo cr087live"}),
            10, "live-test-task", &http, &sessions, None,
        ).await.expect("bash exec should succeed against live server");
        assert!(r.success, "bash should exit 0; error={:?}", r.error);
        let stdout = r.output["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("cr087live"), "stdout should contain marker: {}", stdout);
    }

    #[tokio::test]
    #[ignore = "需 live lv-sandbox sandbox-server (:8080)"]
    async fn write_then_read_roundtrip() {
        if !sandbox_up().await {
            eprintln!("skip: lv-sandbox server not reachable on :8080");
            return;
        }
        let http = lv().await;
        let sessions = SessionMap::new("shell".into(), 300);
        let task = "live-rw-task";
        // write
        let w = translate::execute("fixus_write",
            &serde_json::json!({"file_path": "cr087.txt", "content": "hello live"}), 0, task, &http, &sessions, None).await.unwrap();
        assert!(w.success, "write failed: {:?}", w.error);
        // read back (same task → same session → sees the file)
        let r = translate::execute("fixus_read",
            &serde_json::json!({"file_path": "cr087.txt"}), 0, task, &http, &sessions, None).await.unwrap();
        assert!(r.success, "read failed: {:?}", r.error);
        let content = r.output["content"].as_str().unwrap_or("");
        assert_eq!(content, "hello live", "read-back content mismatch");
    }
}
