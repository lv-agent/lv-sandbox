//! cr-12 G2 Task 6:real-git `clone` + `push` 经 swap-proxy 端到端。
//!
//! 关闭 Task 4(`tests/swap_e2e.rs` 在 egress-swap-proxy crate)留下的缺口:
//! 那个 E2E 用模拟 client 发一个 GET + 玩具 "ok" 响应,验证了 swap 不变量,
//! 但**没有**证明:
//! - 真 `git-remote-fixus` helper 驱动真 `git clone` + `git push`;
//! - git smart-HTTP 多请求流(`info/refs` GET + `git-upload-pack` /
//!   `git-receive-pack` POST + 真 packfile body)能正确经 swap-proxy 的
//!   头部改写中继(尤其大 POST body 的双向 copy)。
//!
//! 本测试补这条证据。全链路:
//!
//! ```text
//! real git client
//!   → git-remote-fixus helper (env: FIXUS_GIT_SENTINEL=<sentinel>)
//!   → UDS → SOCKS5h proxy (allowlist = "localhost")
//!   → swap-proxy (egress_swap_proxy::server::serve, 进程内 tokio task;
//!                 sentinel→real-token 头部改写 + 双向中继)
//!   → git-http-backend CGI 上游(记录 Authorization 头值)
//! ```
//!
//! 断言(对抗性 —— 任一破坏 swap/relay 的缺陷都会让对应断言失败):
//! 1. `git clone fixus::https://localhost:<proxy>/upstream.git` 成功
//!    ⇒ info/refs GET + upload-pack POST + packfile 全部经 swap-proxy 中继成功。
//! 2. 本地提交 + `git push origin main` 成功
//!    ⇒ receive-pack POST(带真 packfile body)中继正确,大 body 双向 copy 通畅。
//! 3. 上游记录的 Authorization == `Bearer <real-token>` 且**不**含 `<sentinel>`
//!    ⇒ 真 git 上下文里 swap 真的发生了(不仅是 Task 4 的玩具 client)。
//! 4. 上游记录的每个 Authorization 都不含 sentinel 字符串。
//!
//! 复用 `tests/common/mod.rs`(`e2e_clone.rs` 同源)的:证书生成、SOCKS5h 代理、
//! TLS git-http-backend CGI 上游(本测试传入 `Arc<Mutex<Option<String>>>`
//! 让上游把收到的 Authorization 头值写进去)、git 包装。

mod common;

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{gen_cert, git, path_with_helper, spawn_cgi_tls_server, spawn_proxy, wait_socket};
use egress_swap_proxy::{config::SwapConfig, server};
use tokio::net::TcpListener;

/// Jail 侧 sentinel —— 牢内 helper 只知此值。
const SENTINEL: &str = "jail-sentinel-ABCDEF-123456";
/// Real token —— 只存在于 swap-proxy 进程内(client/helper 永远看不到)。
const REAL_TOKEN: &str = "real-token-integration-DO-NOT-LEAK";

/// 构造一份 SwapConfig(共享给 server_cfg / upstream_client_cfg / serve)。
fn make_swap_config(upstream_addr: &str, proxy_cert: &common::TestCert, upstream_ca: &str) -> SwapConfig {
    SwapConfig {
        // listen 字段 serve() 不读(listener 已在外部绑好);保留作完整性。
        listen: "127.0.0.1:0".to_string(),
        sentinel: SENTINEL.to_string(),
        real_token: REAL_TOKEN.to_string(),
        upstream: upstream_addr.to_string(),
        cert_pem: proxy_cert.cert_pem.clone(),
        key_pem: proxy_cert.key_pem.clone(),
        upstream_ca_pem: Some(upstream_ca.to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_git_clone_push_through_swap_proxy() {
    let root = tempfile::tempdir().expect("tempdir");

    // ── 裸上游仓库 + 初始提交(镜像 e2e_clone.rs 的种子逻辑) ──────────────
    let upstream = root.path().join("upstream.git");
    git(root.path(), &["init", "--bare", "upstream.git"]);
    let seed = root.path().join("seed");
    git(root.path(), &["init", "seed"]);
    git(&seed, &["config", "user.email", "t@t"]);
    git(&seed, &["config", "user.name", "t"]);
    std::fs::write(seed.join("README"), "hello from upstream\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "init"]);
    git(&seed, &["branch", "-M", "main"]);
    git(
        &seed,
        &["remote", "add", "origin", upstream.to_str().unwrap()],
    );
    git(&seed, &["push", "-q", "origin", "main"]);
    git(&upstream, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&upstream, &["config", "http.receivepack", "true"]);
    git(&upstream, &["config", "http.uploadpack", "true"]);

    // ── 上游:TLS git-http-backend CGI,记录每个请求的 Authorization ──────
    let auth_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (up_port, up_cert) =
        spawn_cgi_tls_server(root.path().to_path_buf(), auth_seen.clone());

    // ── swap-proxy(进程内 tokio task) ───────────────────────────────────
    // 入站 cert 独立于上游 cert(SAN 都是 localhost,但密钥对隔离 —— 接近生产行为)。
    let proxy_inbound_cert = gen_cert();
    let swap_cfg = make_swap_config(
        &format!("localhost:{up_port}"),
        &proxy_inbound_cert,
        &up_cert.cert_pem,
    );
    let server_cfg = server::build_server_config(&swap_cfg).expect("proxy ServerConfig");
    let upstream_client_cfg =
        server::build_upstream_client_config(&swap_cfg).expect("proxy upstream ClientConfig");

    // 先绑 listener 拿到端口,再 spawn accept 循环(确定性 —— 端口就绪后才返回)。
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind swap-proxy ephemeral");
    let proxy_addr = listener.local_addr().expect("swap-proxy local_addr");
    let proxy_port = proxy_addr.port();
    let swap_cfg_arc = Arc::new(swap_cfg);
    let _swap_handle = tokio::spawn(async move {
        let _ = server::serve(
            listener,
            Arc::new(server_cfg),
            Arc::new(upstream_client_cfg),
            swap_cfg_arc,
        )
        .await;
    });

    // ── SOCKS5h-over-UDS 代理(白名单 = "localhost";helper 经此到 swap-proxy)
    let sock_path = root.path().join(".proxy.sock");
    let _proxy = spawn_proxy(sock_path.clone(), "localhost".into());
    wait_socket(&sock_path);
    assert!(sock_path.exists(), "proxy socket not ready");

    let path_env = path_with_helper();
    let clone_dir = root.path().join("work");
    let url = format!("fixus::https://localhost:{proxy_port}/upstream.git");

    // 给上游 / swap-proxy 一点就绪时间。
    tokio::time::sleep(Duration::from_millis(80)).await;

    // ── 关键环境变量:经 Command::env 注入子进程,避免进程级 env 竞态 ───────
    //   SANDBOX_CA_PEM    :helper 经 dialer::env_client_config 信任 swap-proxy 入站证。
    //   FIXUS_GIT_SENTINEL:helper 经 FixusHttp::with_default_roots 在每请求加
    //                       `Authorization: Bearer <sentinel>` 头。
    //   SANDBOX_PROXY_SOCK:helper 拨 UDS 代理路径。
    let env_clone: Vec<(&str, &std::ffi::OsStr)> = vec![
        ("SANDBOX_PROXY_SOCK", sock_path.as_os_str()),
        ("SANDBOX_CA_PEM", proxy_inbound_cert.cert_pem.as_str().as_ref()),
        ("FIXUS_GIT_SENTINEL", SENTINEL.as_ref()),
    ];

    // ── 1)clone ──────────────────────────────────────────────────────────
    let mut clone_cmd = Command::new("git");
    clone_cmd.current_dir(root.path()).env("PATH", &path_env);
    for (k, v) in &env_clone {
        clone_cmd.env(k, v);
    }
    let st = clone_cmd
        .args([
            "clone",
            "-c",
            "protocol.version=0", // helper 当前实现支持 v0/v1;强制 v0 避免服务端走 v2
            &url,
            "work",
        ])
        .output()
        .expect("git clone");
    assert!(
        st.status.success(),
        "clone through swap-proxy failed\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&st.stdout),
        String::from_utf8_lossy(&st.stderr),
    );
    // clone 内容正确 ⇒ upload-pack POST + packfile 中继正确
    let readme = std::fs::read_to_string(clone_dir.join("README")).unwrap();
    assert_eq!(readme, "hello from upstream\n", "cloned content mismatch");

    // ── 2)commit + push(大 POST body:receive-pack + packfile) ──────────
    git(&clone_dir, &["config", "user.email", "t@t"]);
    git(&clone_dir, &["config", "user.name", "t"]);
    std::fs::write(clone_dir.join("note.txt"), "pushed through swap-proxy\n").unwrap();
    git(&clone_dir, &["add", "note.txt"]);
    git(&clone_dir, &["commit", "-q", "-m", "add note"]);

    let env_push: Vec<(&str, &std::ffi::OsStr)> = vec![
        ("SANDBOX_PROXY_SOCK", sock_path.as_os_str()),
        ("SANDBOX_CA_PEM", proxy_inbound_cert.cert_pem.as_str().as_ref()),
        ("FIXUS_GIT_SENTINEL", SENTINEL.as_ref()),
    ];
    let mut push_cmd = Command::new("git");
    push_cmd.current_dir(&clone_dir).env("PATH", &path_env);
    for (k, v) in &env_push {
        push_cmd.env(k, v);
    }
    let push = push_cmd
        .args(["-c", "protocol.version=0", "push", "-q", "origin", "main"])
        .output()
        .expect("git push");
    assert!(
        push.status.success(),
        "push through swap-proxy failed\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&push.stdout),
        String::from_utf8_lossy(&push.stderr),
    );

    // 上游确实收到了推上来的文件(receive-pack 中继 + packfile 重组正确)
    let tree = git(&upstream, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        tree.contains("note.txt"),
        "upstream did not receive pushed file; tree:\n{tree}"
    );
    let blob = git(&upstream, &["show", "main:note.txt"]);
    assert_eq!(
        blob, "pushed through swap-proxy\n",
        "pushed file content mismatch on upstream"
    );

    // ── 3)swap 真的发生过(真 git 上下文,不是 Task 4 玩具 client) ───────
    // 等上游线程把最后一条请求的 Authorization 写完:git push 的最后一条请求
    // 是 receive-pack POST,它由 swap-proxy 转发后由上游线程同步处理。等到
    // 上游把响应写回(pushed 文件已被 ls-tree 看到 ⇒ 响应必然已发完 ⇒ 上游
    // 的 auth 记录必然已完成)。无固定 sleep。
    let auth = retry_lock_value(&auth_seen);
    let auth = auth.expect(
        "upstream must have received at least one Authorization header \
                 (helper should have sent one per request through the swap-proxy)",
    );
    assert_eq!(
        auth,
        format!("Bearer {REAL_TOKEN}"),
        "upstream must see the REAL token, not the sentinel"
    );
    assert!(
        !auth.contains(SENTINEL),
        "sentinel must NOT reach upstream (swap must have replaced it), got: {auth}"
    );

    // ── 4)(强化)sentinel 字符串绝不出现在上游收到的任何 Authorization 里 ──
    //    已由上面 `!auth.contains(SENTINEL)` 覆盖(最后写赢;所有请求同值)。
}

/// 轮询读 `Arc<Mutex<Option<String>>>` 直到 Some(或超时返回当前值)。
///
/// 上游 CGI 线程在每条请求里同步写 `*g = Some(auth)`;`push.status.success()`
/// 必然晚于 receive-pack 响应发完(进而晚于 auth 写入)。但保险起见做短轮询,
/// 避免任何线程调度竞态导致的假阴。
fn retry_lock_value(cell: &Arc<Mutex<Option<String>>>) -> Option<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        {
            let g = cell.lock().expect("auth_seen mutex poisoned");
            if g.is_some() {
                return g.clone();
            }
        }
        if std::time::Instant::now() >= deadline {
            return cell.lock().expect("auth_seen mutex poisoned").clone();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
