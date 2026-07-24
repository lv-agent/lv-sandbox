//! cr-12 全栈 E2E(policy→profile→执行→clone)。`#[ignore]`:需外部 bin + 预构建。
//! 见 docs/superpowers/specs/2026-07-24-cr12-end-to-end-assembly-design.md。
//!
//! 跑法:
//!   cd <lv-sandbox> && cargo build --workspace
//!   FIXUS_E2E_TOOLS_BANK_BIN=<lv-fixus>/target/debug/tools-bank \
//!   FIXUS_E2E_BROKER_BIN=<lv-logdb>/target/debug/logdb-broker \
//!   cargo test -p git-remote-fixus --test cr12_e2e_full_stack -- --ignored --nocapture

mod common;

use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use common::{gen_cert, spawn_cgi_tls_server};
use egress_swap_proxy::{config::SwapConfig, server};
use tokio::net::TcpListener;

const SENTINEL: &str = "jail-sentinel-E2E";
const REAL_TOKEN: &str = "real-token-E2E";

/// 子进程 RAII:drop 时 kill+reap,panic 也清。
struct Proc {
    child: Child,
    name: &'static str,
}
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        eprintln!("[teardown] killed {}", self.name);
    }
}

/// 取必需 env 指针;缺 → 指引并 panic(外层 `#[ignore]`,手动跑)。
fn require_bin(env_name: &str) -> String {
    match std::env::var(env_name) {
        Ok(v) if !v.is_empty() => v,
        _ => panic!(
            "set {env_name} (+ cargo build --workspace & --bin tools-bank & -p logdb-broker) \
             to run this #[ignore] full-stack test"
        ),
    }
}

/// workspace target/debug(sibling bin 都在此)。
fn target_debug() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_BIN_EXE_git-remote-fixus"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// 轮询 TCP 端口就绪(最多 ~5s)。
fn wait_port(addr: &str) {
    for _ in 0..100 {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("port not ready: {addr}");
}

fn write_tmp(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

/// 起 logdb-broker(embedded logdbd)+ 返回 Proc。bind 5100(= sandbox-broker/tools-bank 默认 --broker-addr)。
///
/// 注:`embedded: true` 下 broker 把 `logdbd_addr` 当 `SocketAddr` parse(非 URL),
/// 故写成裸 `127.0.0.1:50051`;`data_dir` 显式指向 tempdir 避免 `./data` 落到 workspace。
fn spawn_broker(dir: &std::path::Path) -> Proc {
    let bin = require_bin("FIXUS_E2E_BROKER_BIN");
    let data_dir = dir.join("logdb-data");
    let cfg = write_tmp(
        dir,
        "broker.yaml",
        &format!(
            "bind_addr: \"127.0.0.1:5100\"\n\
             logdbd_addr: \"127.0.0.1:50051\"\n\
             embedded: true\n\
             num_shards: 4\n\
             session_timeout_ms: 10000\n\
             data_dir: \"{}\"\n",
            data_dir.display()
        ),
    );
    let child = Command::new(&bin)
        .env("LOGDB_BROKER_CONFIG", &cfg)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn logdb-broker: {e}"));
    Proc {
        child,
        name: "logdb-broker",
    }
}

fn spawn_tools_bank() -> Proc {
    let bin = require_bin("FIXUS_E2E_TOOLS_BANK_BIN");
    let child = Command::new(&bin)
        .args([
            "--broker-addr",
            "127.0.0.1:5100",
            "--region",
            "default",
            "--port",
            "3001",
        ])
        .env("LOGDBD_NAMESPACE", "default")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn tools-bank: {e}"));
    Proc {
        child,
        name: "tools-bank",
    }
}

fn spawn_sandbox_server(
    dir: &std::path::Path,
    swap_port: u16,
    inbound_cert_pem: &str,
) -> Proc {
    let bin = target_debug().join("sandbox-server");
    let base_dir = dir.join("sandboxes");
    let cfg = write_tmp(
        dir,
        "sandbox-server.yaml",
        &format!(
            "server:\n  listen_addr: \"127.0.0.1:8080\"\n  log_level: \"info\"\n  log_format: \"text\"\n\
             sandbox:\n  base_dir: \"{base}\"\n  fail_closed: false\n  default_profile: \"shell\"\n\
             profiles:\n  shell:\n    rlimit:\n      nproc: 8192\n      nofile: 256\n",
            base = base_dir.display()
        ),
    );
    let cert_file = write_tmp(dir, "git-ca.pem", inbound_cert_pem);
    let mut path = std::ffi::OsString::from(target_debug());
    path.push(":");
    if let Ok(p) = std::env::var("PATH") {
        path.push(p);
    }
    let child = Command::new(&bin)
        .args(["--config", cfg.to_str().unwrap()])
        .env("PATH", &path)
        .env("FIXUS_GIT_SENTINEL", SENTINEL)
        .env("FIXUS_GIT_EGRESS_HOST", "localhost")
        .env("FIXUS_GIT_EGRESS_PORT", swap_port.to_string())
        .env("FIXUS_GIT_CA_FILE", &cert_file)
        .env("RUST_LOG", "info")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn sandbox-server: {e}"));
    Proc {
        child,
        name: "sandbox-server",
    }
}

fn spawn_sandbox_broker() -> Proc {
    let bin = target_debug().join("sandbox-broker");
    let mut cmd = Command::new(&bin);
    cmd.args([
        "--broker-addr",
        "127.0.0.1:5100",
        "--sandbox-url",
        "http://127.0.0.1:8080",
        "--region",
        "default",
        "--group",
        "sandboxes",
    ])
    .env("LOGDBD_NAMESPACE", "default")
    .env("RUST_LOG", "warn")
    .env_remove("HTTP_PROXY")
    .env_remove("HTTPS_PROXY")
    .env_remove("http_proxy")
    .env_remove("https_proxy")
    .env_remove("ALL_PROXY")
    .env_remove("all_proxy")
    .env("NO_PROXY", "*")
    .env("no_proxy", "*")
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit());
    Proc {
        child: cmd
            .spawn()
            .unwrap_or_else(|e| panic!("spawn sandbox-broker: {e}")),
        name: "sandbox-broker",
    }
}

/// POST /mcp tools/call,返回完整 McpResponse JSON。
async fn mcp_call(port: u16, session_id: &str, policy_json: Option<&str>,
                  tool: &str, command: &str) -> serde_json::Value {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60)).build().unwrap();
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": {"command": command}}
    });
    let mut req = client.post(format!("http://127.0.0.1:{port}/mcp"))
        .header("X-Fixus-Session-Id", session_id)
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(p) = policy_json {
        req = req.header("X-Fixus-Policy", p);
    }
    let resp = req.send().await.expect("POST /mcp");
    resp.json().await.expect("mcp json")
}

/// 本层:起 in-process CGI 上游 + swap-proxy,验证 token 兑换通道就绪。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "full-stack: needs FIXUS_E2E_TOOLS_BANK_BIN + FIXUS_E2E_BROKER_BIN + prebuilt workspace"]
async fn e2e_full_stack_bringup() {
    let root = tempfile::tempdir().expect("tempdir");

    // 1) 本地 TLS git-http-backend 上游(记录 Authorization)。
    let auth_seen = Arc::new(std::sync::Mutex::new(None));
    let (up_port, up_cert) = spawn_cgi_tls_server(root.path().to_path_buf(), auth_seen.clone());

    // 2) swap-proxy 入站证 + in-process serve。
    let inbound = gen_cert();
    let swap_cfg = SwapConfig {
        listen: "127.0.0.1:0".into(),
        sentinel: SENTINEL.into(),
        real_token: REAL_TOKEN.into(),
        upstream: format!("127.0.0.1:{up_port}"),
        cert_pem: inbound.cert_pem.clone(),
        key_pem: inbound.key_pem.clone(),
        upstream_ca_pem: Some(up_cert.cert_pem.clone()),
    };
    let server_cfg = Arc::new(server::build_server_config(&swap_cfg).expect("server cfg"));
    let upstream_cfg = Arc::new(server::build_upstream_client_config(&swap_cfg).expect("upstream cfg"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind swap-proxy");
    let swap_port = listener.local_addr().expect("addr").port();
    let cfg = Arc::new(swap_cfg);
    let _swap = tokio::spawn(async move {
        let _ = server::serve(listener, server_cfg, upstream_cfg, cfg).await;
    });

    // 3) 验证:swap-proxy 端口起来(后续 Task 在此之上接 sandbox-server)。
    wait_port(&format!("127.0.0.1:{swap_port}"));
    eprintln!("[scaffold] swap-proxy :{swap_port} → upstream :{up_port} OK");

    // 4) 起外部进程栈。
    let _broker = spawn_broker(root.path());
    wait_port("127.0.0.1:5100");
    let _sandbox_server = spawn_sandbox_server(root.path(), swap_port, &inbound.cert_pem);
    wait_port("127.0.0.1:8080");
    let _sandbox_broker = spawn_sandbox_broker();
    let _tools_bank = spawn_tools_bank();
    wait_port("127.0.0.1:3001");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await; // consumer group 加入
    eprintln!("[bringup] broker+server+bridge+tools-bank ready");

    // 5) plain bash(shell profile,无 policy)→ 证上栈通。
    let task_id = "e2e-plain-bash";
    let v = mcp_call(3001, task_id, None, "fixus_bash", "echo hello-from-sandbox").await;
    let text = v["result"]["content"][0]["text"].as_str().expect("content text");
    let out: serde_json::Value = serde_json::from_str(text).expect("ToolResult json");
    assert_eq!(out["exit_code"], 0, "plain bash failed: {v}");
    let stdout = out["stdout"].as_str().unwrap_or("");
    assert!(stdout.contains("hello-from-sandbox"), "stdout mismatch: {stdout}");
    eprintln!("[plain-bash] stdout={stdout:?} — upper stack OK");
}
