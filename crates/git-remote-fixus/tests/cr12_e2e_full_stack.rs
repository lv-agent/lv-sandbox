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
#[allow(dead_code)] // Task 4+ 起调用
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
#[allow(dead_code)] // Task 4+ 起调用
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
#[allow(dead_code)] // Task 4+ 起调用
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

/// 本层:起 in-process CGI 上游 + swap-proxy,验证 token 兑换通道就绪。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "full-stack: needs FIXUS_E2E_TOOLS_BANK_BIN + FIXUS_E2E_BROKER_BIN + prebuilt workspace"]
async fn e2e_scaffold_swap_proxy_and_upstream() {
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

    // 占位:让 `Stdio`/`Command` import 在本层不算 dead-code(Task 4 起用 Proc 起外部 bin)。
    let _ = Command::new("true").stdout(Stdio::null());
}
