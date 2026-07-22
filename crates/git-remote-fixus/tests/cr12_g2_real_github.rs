//! cr-12 G2 live 验证(真 github.com,`#[ignore]` —— 依赖外网 + 真 PAT,不进默认 CI)。
//!
//! 闭合"本地测试(G2 集成测试用本地 git-http-backend CGI)能跑,但对真 github 呢?"的悬念。
//!
//! **需 operator 提供真 github PAT**(测试不硬编码凭据):运行时从 `FIXUS_GITHUB_PAT`
//! 读 PAT 作 swap-proxy 的 `real_token`。helper 只持 sentinel;真 PAT 只在 swap-proxy 进程内。
//!
//! 链路:
//! ```text
//! real git client
//!   → git-remote-fixus helper (FIXUS_GIT_SENTINEL)
//!   → UDS → SOCKS5h proxy (allowlist = "localhost")
//!   → swap-proxy (server::serve; upstream = github.com:443, webpki-roots;
//!                sentinel→PAT 头部改写 + Host 重写 + 双向中继)
//!   → REAL github.com (octocat/Hello-World.git)
//! ```
//!
//! 跑法(设 PAT 后):
//! `FIXUS_GITHUB_PAT=<pat> cargo test -p git-remote-fixus --test cr12_g2_real_github -- --ignored --nocapture`
//!
//! **Tier 1 已手工验证(无需 PAT)**:链路到真 github 通(TLS webpki-roots + Host 重写
//! + v0 协议)—— 用 dummy token 时 github 返回 401(对在场无效凭据的正确反应),
//! 证明请求确达 github 的 git-http-backend 而非路由错。本 `#[ignored]` 测试是 Tier 2:
//! 用真 PAT 走完整 sentinel→PAT 兑换 + 认证 clone。

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{gen_cert, path_with_helper, spawn_proxy, wait_socket};
use egress_swap_proxy::{config::SwapConfig, server};
use tokio::net::TcpListener;

const SENTINEL: &str = "jail-sentinel-REAL-GITHUB";
/// 公开小仓(连通性已确认)。用真 PAT 时 github 认证后放行 clone。
const GITHUB_REPO_PATH: &str = "octocat/Hello-World.git";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "network + needs FIXUS_GITHUB_PAT: live clone from real github.com"]
async fn real_github_clone_through_swap_proxy() {
    // 真 PAT 只在此测试进程内(作 swap-proxy 的 real_token);helper 只见 sentinel。
    let real_token = std::env::var("FIXUS_GITHUB_PAT").unwrap_or_else(|_| {
        panic!(
            "set FIXUS_GITHUB_PAT=<your-github-pat> to run this live validation\n\
             e.g. FIXUS_GITHUB_PAT=ghp_... cargo test -p git-remote-fixus \
             --test cr12_g2_real_github -- --ignored --nocapture"
        )
    });

    let root = tempfile::tempdir().expect("tempdir");

    // swap-proxy 入站证(self-signed localhost;helper 经 SANDBOX_CA_PEM 信任)。
    let proxy_inbound_cert = gen_cert();
    let swap_cfg = SwapConfig {
        listen: "127.0.0.1:0".to_string(),
        sentinel: SENTINEL.to_string(),
        real_token,
        upstream: "github.com:443".to_string(),
        cert_pem: proxy_inbound_cert.cert_pem.clone(),
        key_pem: proxy_inbound_cert.key_pem.clone(),
        upstream_ca_pem: None, // webpki-roots —— 信任真 github 的真实 CA 证。
    };
    let server_cfg = server::build_server_config(&swap_cfg).expect("proxy ServerConfig");
    let upstream_client_cfg =
        server::build_upstream_client_config(&swap_cfg).expect("proxy upstream ClientConfig");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind swap-proxy");
    let proxy_port = listener.local_addr().expect("addr").port();
    let cfg = Arc::new(swap_cfg);
    let _swap = tokio::spawn(async move {
        let _ = server::serve(listener, Arc::new(server_cfg), Arc::new(upstream_client_cfg), cfg).await;
    });

    // SOCKS5h-over-UDS(allowlist = localhost;helper 经此到 swap-proxy)。
    let sock_path = root.path().join(".proxy.sock");
    let _proxy = spawn_proxy(sock_path.clone(), "localhost".into());
    wait_socket(&sock_path);

    let path_env = path_with_helper();
    // URL 指向 swap-proxy(G2 拓扑);path 部分是真 github 仓。
    let url = format!("fixus::https://localhost:{proxy_port}/{GITHUB_REPO_PATH}");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let clone_dir = root.path().join("work");
    let mut cmd = Command::new("git");
    cmd.current_dir(root.path())
        .env("PATH", &path_env)
        .env("SANDBOX_PROXY_SOCK", &sock_path)
        .env("SANDBOX_CA_PEM", &proxy_inbound_cert.cert_pem)
        .env("FIXUS_GIT_SENTINEL", SENTINEL)
        // 故意不强制 protocol.version —— 测真 git 默认协商(v0 还是 v2)。
        .args(["clone", "--depth", "1", &url, "work"]);
    let out = cmd.output().expect("git clone spawn");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "real-github clone through swap-proxy failed.\n\
         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}\n\
         (Host 重写/v0/TLS 已 Tier1 验证;失败多因 PAT 权限或 github 侧问题)"
    );

    let entries: Vec<_> = std::fs::read_dir(&clone_dir)
        .expect("clone dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !entries.is_empty() && entries.iter().any(|n| n == ".git"),
        "clone dir missing .git; entries: {entries:?}"
    );
    eprintln!("real-github clone OK; entries: {entries:?}");
}

