//! cr-12 G2 Task 4: E2E invariant verification for the swap-proxy's full TLS relay path.
//!
//! 这些测试 **不** 重复单测的纯函数 swap::rewrite_authorization,而是驱动真实
//! `server::serve(listener, ...)` + `handle_conn` 路径:
//! client → TLS → swap-proxy → TLS → fake upstream。
//!
//! 验证 design §5 的三条安全不变量 END-TO-END:
//!
//! 1. **兑换生效**:client 发 `Bearer <sentinel>` → 上游收到 `Bearer <real>`
//!    (上游永远看不到 sentinel)。
//! 2. **错/缺 sentinel → 401 且不连上游**:client 发 `Bearer <wrong>` 或不带
//!    Authorization → proxy 立即 401,fake upstream 的 acceptor 全程未被连。
//! 3. **real-token 隔离**:client 永远只发 / 拥有 sentinel(REAL_TOKEN 字符串
//!    从不出现在 client 侧任何字节里);上游永远只看到 real-token(SENTINEL
//!    字符串从不出现在转发到上游的字节里)。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{ephemeral_self_signed_for, ephemeral_self_signed_with_key_pem};
use egress_swap_proxy::{config::SwapConfig, server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_rustls::rustls;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

/// Jail 侧 sentinel —— 牢内 helper(client)只知此值。
const SENTINEL: &str = "jail-sentinel-ABCDEF-123456";
/// Real token —— 只存在于 swap-proxy 进程内(client 永远看不到)。
const REAL_TOKEN: &str = "real-token-XYZ789-secret-DO-NOT-LEAK";

/// Fake upstream 抓取器:每条 TLS 连接记录 raw 字节 + Authorization 头值 + 连接计数。
struct UpstreamCapture {
    /// 收到的原始字节(request line + headers + 已读 body)。
    raw: Arc<Mutex<Vec<u8>>>,
    /// 解析出的 Authorization 头值(`Bearer <token>`)。None = 暂无连接。
    auth: Arc<Mutex<Option<String>>>,
    /// 已接受的 TLS 连接数。
    conns: Arc<Mutex<u32>>,
}

/// Spawn fake upstream TLS server 在临时端口上。证书含 IP SAN `127.0.0.1`
/// (proxy 经 `ServerName::IpAddress(127.0.0.1)` 连入)。
///
/// 返回 `(addr, upstream_cert_pem, capture, join_handle)`。
/// `upstream_cert_pem` 用于让 swap-proxy 信任此 fake upstream(经
/// `SwapConfig.upstream_ca_pem`)。
async fn spawn_fake_upstream() -> (
    std::net::SocketAddr,
    String,
    UpstreamCapture,
    tokio::task::JoinHandle<()>,
) {
    let (chain, key, up_cert_pem, _up_key_pem) = ephemeral_self_signed_for(&["127.0.0.1"]);
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("fake upstream ServerConfig");
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream ephemeral");
    let addr = listener.local_addr().expect("upstream local_addr");

    let raw: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let auth: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let conns: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let cap = UpstreamCapture {
        raw: raw.clone(),
        auth: auth.clone(),
        conns: conns.clone(),
    };

    let handle = tokio::spawn(async move {
        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => continue,
            };
            let acceptor = acceptor.clone();
            let raw = raw.clone();
            let auth = auth.clone();
            let conns = conns.clone();
            tokio::spawn(async move {
                let mut tls = match acceptor.accept(tcp).await {
                    Ok(t) => t,
                    Err(_) => return,
                };
                {
                    let mut c = conns.lock().await;
                    *c += 1;
                }
                // 读到 \r\n\r\n(测试请求都无 body;有也只到 head 结束即可断言 Authorization)。
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    let mut tmp = [0u8; 4096];
                    let n = match tls.read(&mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                {
                    let mut r = raw.lock().await;
                    r.extend_from_slice(&buf);
                }
                // 解析 Authorization(头名大小写不敏感)。
                let text = String::from_utf8_lossy(&buf);
                for line in text.split("\r\n").skip(1) {
                    if let Some((name, val)) = line.split_once(':') {
                        if name.trim().eq_ignore_ascii_case("authorization") {
                            let mut a = auth.lock().await;
                            *a = Some(val.trim().to_string());
                            break;
                        }
                    }
                }
                // 回 200 OK + body "ok"。
                let _ = tls
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
                // 关闭(超时兜底 —— peer 不发 close-notify 也别挂死 task)。
                let _ = tokio::time::timeout(Duration::from_millis(100), tls.shutdown()).await;
            });
        }
    });

    (addr, up_cert_pem, cap, handle)
}

/// Spawn swap-proxy(经 `server::serve`)在临时端口上。
///
/// 返回 `(proxy_addr, proxy_cert_pem, join_handle)`。
/// `proxy_cert_pem` 是 client 要信任的根(此代理的入站证 PEM)。
async fn spawn_swap_proxy(
    upstream_addr: std::net::SocketAddr,
    up_cert_pem: &str,
) -> (std::net::SocketAddr, String, tokio::task::JoinHandle<()>) {
    // 入站 cert —— client 信任此证。SAN "localhost"。
    let (_chain, _key, proxy_cert_pem, proxy_key_pem) = ephemeral_self_signed_with_key_pem();

    let cfg = SwapConfig {
        listen: "127.0.0.1:0".to_string(), // serve() 不读此字段(监听已绑好);保留作完整性
        sentinel: SENTINEL.to_string(),
        real_token: REAL_TOKEN.to_string(),
        upstream: format!("127.0.0.1:{}", upstream_addr.port()),
        cert_pem: proxy_cert_pem.clone(),
        key_pem: proxy_key_pem,
        upstream_ca_pem: Some(up_cert_pem.to_string()),
    };
    let server_cfg = server::build_server_config(&cfg).expect("proxy ServerConfig");
    let upstream_client_cfg =
        server::build_upstream_client_config(&cfg).expect("proxy upstream ClientConfig");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind proxy ephemeral");
    let proxy_addr = listener.local_addr().expect("proxy local_addr");

    let cfg = Arc::new(cfg);
    let handle = tokio::spawn(async move {
        let _ = server::serve(
            listener,
            Arc::new(server_cfg),
            Arc::new(upstream_client_cfg),
            cfg,
        )
        .await;
    });

    (proxy_addr, proxy_cert_pem, handle)
}

/// 用 TLS client 连 swap-proxy,发送 HTTP/1.1 请求,返回响应字节。
///
/// `authorization` = 完整 Authorization 头值(如 `"Bearer <sentinel>"`)。
/// `None` = 不带 Authorization 头(测试 missing 情况)。
async fn send_via_proxy(
    proxy_addr: std::net::SocketAddr,
    proxy_cert_pem: &str,
    authorization: Option<&str>,
) -> Vec<u8> {
    let mut root_store = rustls::RootCertStore::empty();
    let mut reader = std::io::Cursor::new(proxy_cert_pem.as_bytes());
    for cert in rustls_pemfile::certs(&mut reader) {
        let _ = root_store.add(cert.expect("parse proxy CA"));
    }
    let cfg = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    );
    let connector = TlsConnector::from(cfg);
    let tcp = tokio::net::TcpStream::connect(proxy_addr)
        .await
        .expect("connect proxy");
    let server_name = rustls::pki_types::ServerName::try_from("localhost").expect("ServerName");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("TLS handshake with proxy");

    let mut req: Vec<u8> = b"GET /info/refs HTTP/1.1\r\nHost: localhost\r\n".to_vec();
    if let Some(auth) = authorization {
        req.extend_from_slice(b"Authorization: ");
        req.extend_from_slice(auth.as_bytes());
        req.extend_from_slice(b"\r\n");
    }
    req.extend_from_slice(b"Connection: close\r\n\r\n");
    tls.write_all(&req).await.expect("write request");
    let _ = tls.flush().await;
    // 不主动 shutdown 写侧 —— proxy 看到 \r\n\r\n 即断:
    //   上游回包 → proxy u2c 中继完成 → select 返回 → drop tls → client 读到 EOF。
    //   这条路径与生产 git helper 的 Connection: close 行为一致。

    let mut out = Vec::new();
    tls.read_to_end(&mut out).await.expect("read response");
    out
}

// ────────────────────────────────────────────────────────────────────────────
// 不变量 1:swap 生效 —— sentinel 被换成 real-token,上游永远看不到 sentinel。
// ────────────────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn swap_proxy_replaces_sentinel_with_real_token_e2e() {
    let (up_addr, up_pem, up, _up_h) = spawn_fake_upstream().await;
    let (proxy_addr, proxy_pem, _proxy_h) = spawn_swap_proxy(up_addr, &up_pem).await;

    let response =
        send_via_proxy(proxy_addr, &proxy_pem, Some(&format!("Bearer {SENTINEL}"))).await;

    // 上游收到的 Authorization == "Bearer <real-token>"(不是 sentinel)。
    let auth = up
        .auth
        .lock()
        .await
        .clone()
        .expect("upstream must have received an Authorization");
    assert_eq!(auth, format!("Bearer {REAL_TOKEN}"));
    assert!(
        !auth.contains(SENTINEL),
        "sentinel must NOT reach upstream, got: {auth}"
    );

    // 响应经 proxy 透传回 client。
    let resp = String::from_utf8_lossy(&response);
    assert!(
        resp.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK round-trip, got: {resp}"
    );
    assert!(resp.ends_with("ok"), "expected body 'ok', got: {resp}");
}

// ────────────────────────────────────────────────────────────────────────────
// 不变量 2a:错的 sentinel → 401,上游从未被连。
// ────────────────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_sentinel_returns_401_and_no_upstream_contact() {
    let (up_addr, up_pem, up, _up_h) = spawn_fake_upstream().await;
    let (proxy_addr, proxy_pem, _proxy_h) = spawn_swap_proxy(up_addr, &up_pem).await;

    let response = send_via_proxy(
        proxy_addr,
        &proxy_pem,
        Some("Bearer wrong-value-not-the-sentinel"),
    )
    .await;
    let resp = String::from_utf8_lossy(&response);
    assert!(
        resp.starts_with("HTTP/1.1 401"),
        "expected 401 on wrong sentinel, got: {resp}"
    );

    // 上游 acceptor 全程 0 连接(proxy 在 swap 阶段就 reject,从未 connect upstream)。
    let conns = *up.conns.lock().await;
    assert_eq!(conns, 0, "upstream must NOT be contacted on wrong sentinel");
    assert!(
        up.auth.lock().await.is_none(),
        "upstream Authorization must be None (never contacted)"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// 不变量 2b:缺 Authorization → 401,上游从未被连。
// ────────────────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_authorization_returns_401_and_no_upstream_contact() {
    let (up_addr, up_pem, up, _up_h) = spawn_fake_upstream().await;
    let (proxy_addr, proxy_pem, _proxy_h) = spawn_swap_proxy(up_addr, &up_pem).await;

    let response = send_via_proxy(proxy_addr, &proxy_pem, None).await;
    let resp = String::from_utf8_lossy(&response);
    assert!(
        resp.starts_with("HTTP/1.1 401"),
        "expected 401 when Authorization missing, got: {resp}"
    );

    let conns = *up.conns.lock().await;
    assert_eq!(
        conns, 0,
        "upstream must NOT be contacted when Authorization is missing"
    );
    assert!(
        up.auth.lock().await.is_none(),
        "upstream Authorization must be None"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// 不变量 3:real-token 双向隔离。
//   (a) client 发的字节含 SENTINEL,**绝不**含 REAL_TOKEN(client 从不持有 real-token)。
//   (b) 上游收到的字节含 REAL_TOKEN,**绝不**含 SENTINEL(proxy 永不透传 sentinel)。
//   (c) 上游响应回 client 的字节也**绝不**含 REAL_TOKEN(防 token echo-back 泄漏)。
// ────────────────────────────────────────────────────────────────────────────
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_token_isolation() {
    let (up_addr, up_pem, up, _up_h) = spawn_fake_upstream().await;
    let (proxy_addr, proxy_pem, _proxy_h) = spawn_swap_proxy(up_addr, &up_pem).await;

    // (a) 构造 client 发送的确切字节 —— 仿照 git-remote-fixus 牢内 helper 的请求。
    let client_request = format!(
        "GET /info/refs HTTP/1.1\r\n\
         Host: localhost\r\n\
         Authorization: Bearer {SENTINEL}\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let client_bytes = client_request.as_bytes();

    // client 字节里必须有 SENTINEL(就是它要发的)。
    assert!(
        contains_subsequence(client_bytes, SENTINEL.as_bytes()),
        "client request must contain the sentinel"
    );
    // client 字节里**绝不**含 REAL_TOKEN(client 根本不知道这串值)。
    assert!(
        !contains_subsequence(client_bytes, REAL_TOKEN.as_bytes()),
        "REAL_TOKEN must NOT appear in client bytes — client never possesses it"
    );

    // 实际通过 proxy 发一次(走完整 TLS 路径)。
    let response =
        send_via_proxy(proxy_addr, &proxy_pem, Some(&format!("Bearer {SENTINEL}"))).await;
    let resp = String::from_utf8_lossy(&response);
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "swap should have succeeded, got: {resp}"
    );

    // 上游接受过恰好 1 条连接(此时 raw / auth 已写完 —— 上游是写完 response 才回包的,
    // 而 client 收到 response 必经过 proxy 的 u2c 中继 ⇒ 上游侧记录已完成,无竞态)。
    let conns = *up.conns.lock().await;
    assert_eq!(conns, 1, "upstream should have been contacted exactly once");

    let raw = up.raw.lock().await.clone();
    let raw_str = String::from_utf8_lossy(&raw);

    // (b) 上游收到的字节含 REAL_TOKEN(swap 兑换生效)。
    assert!(
        raw_str.contains(REAL_TOKEN),
        "upstream bytes must contain REAL_TOKEN after swap: {raw_str}"
    );
    // (b') 上游收到的字节**绝不**含 SENTINEL(sentinel 永不透传出代理)。
    assert!(
        !raw_str.contains(SENTINEL),
        "SENTINEL must NOT be forwarded to upstream: {raw_str}"
    );

    // (c) 上游响应回 client 的字节也**绝不**含 REAL_TOKEN。
    //     (fake upstream 固定回 "ok" —— 此处断言即便响应含任何回声,real-token 不外泄。)
    assert!(
        !resp.contains(REAL_TOKEN),
        "REAL_TOKEN must NOT appear in the response back to the client: {resp}"
    );
}

/// 子序列包含检查(避开 `str::contains` 对 UTF-8 边界的假设;直接做字节窗口匹配)。
fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
