//! cr-12 G2 swap-proxy 服务端:accept 循环 + 单连接处理 + TLS 配置构造。
//!
//! 从 `main.rs` 拆出(原行为不变),让 Task 4 的 E2E 测试可直接驱动
//! `serve(listener, ...)` 在进程内跑完整 TLS relay 路径,而非 spawn 子进程。
//!
//! 一请求一连接:helper 每请求新拨 + 发 `Connection: close`,
//! 故无需完整 HTTP 解析 —— 只读头部块到 `\r\n\r\n`,改写一行,其余字节透传。

use std::sync::Arc;
use std::time::Duration;

use crate::config;
use crate::swap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

/// 单连接整体超时(含 relay 阶段),避免恶意/卡死连接占资源。
pub const CONN_TIMEOUT: Duration = Duration::from_secs(300);
/// 头部块上限(防恶意大头部占内存)。超 → 431。
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// accept 循环 —— 绑定一个 `TcpListener` 后无限 accept,每个新连接 spawn
/// 一个 `handle_conn` task(单连接 `CONN_TIMEOUT` 兜底)。
///
/// accept 错 → warn + continue(单次 accept 失败不影响整体);
/// `handle_conn` 错 → warn(单连接失败不影响其他连接,cr-12 G2 review I-2)。
///
/// 永不返回(无限循环);只在 listener 关闭 / fatal io 错时返回 Err。
/// E2E 测试把此函数 spawn 成 tokio task,task 在测试结束时随 runtime 一起回收。
pub async fn serve(
    listener: TcpListener,
    server_cfg: Arc<rustls::ServerConfig>,
    upstream_cfg: Arc<rustls::ClientConfig>,
    cfg: Arc<config::SwapConfig>,
) -> std::io::Result<()> {
    let local = listener.local_addr().ok();
    info!(
        listen = ?local,
        upstream = %cfg.upstream,
        "egress-swap-proxy ready (sentinel→real swap)"
    );
    let acceptor = TlsAcceptor::from(server_cfg);
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                warn!(error = %e, "accept failed");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let cfg = cfg.clone();
        let up = upstream_cfg.clone();
        tokio::spawn(async move {
            // I-2:结果显式 match —— 非 SwapError 失败(TLS 握手 / upstream 连接 / relay 错)
            //       必须在 warn 级可见,否则生产故障不可观测。
            let res = tokio::time::timeout(CONN_TIMEOUT, handle_conn(tcp, acceptor, cfg, up)).await;
            match res {
                Ok(Ok(())) => tracing::trace!(%peer, "conn closed"),
                Ok(Err(e)) => warn!(%peer, error = %e, "conn handler"),
                Err(_) => warn!(%peer, timeout = ?CONN_TIMEOUT, "conn timed out"),
            }
        });
    }
}

/// 处理单条入站 TLS 连接:TLS-accept → 读头 → 改写 → 连上游 → 中继。
///
/// 标 `pub` 是为让 E2E 测试在需要时也能直接驱动单连接(进出现 serve 入口的常见入口);
/// 正常调用路径是 `serve` 内 spawn。
pub async fn handle_conn(
    tcp: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    cfg: Arc<config::SwapConfig>,
    upstream_cfg: Arc<rustls::ClientConfig>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tls = acceptor.accept(tcp).await?;

    // 1) 读到头部块结束(\r\n\r\n)。其余字节(请求体)留在 buf 里待中继。
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    loop {
        let mut tmp = [0u8; 4096];
        let n = tls.read(&mut tmp).await?;
        if n == 0 {
            // 客户端在发完头部前先关,无完整请求 → 静默丢弃。
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEADER_BYTES {
            tls.write_all(
                b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\n\r\n",
            )
            .await?;
            let _ = tls.shutdown().await;
            return Ok(());
        }
    }

    let hdr_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("checked above");
    let header_block = &buf[..hdr_end]; // 不含尾 \r\n\r\n
    let body_so_far = &buf[hdr_end + 4..]; // 头之后已读到的请求体字节

    // 2) 改写 Authorization。错 → 401 不转发(missing / mismatch / multiple)。
    let rewritten = match swap::rewrite_authorization(header_block, &cfg.sentinel, &cfg.real_token) {
        Ok(b) => b,
        Err(e) => {
            tls.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .await?;
            let _ = tls.shutdown().await;
            info!(error = %e, "rejected (not forwarded) → 401");
            return Ok(());
        }
    };

    // 3) 连真上游(TLS)。ServerName 取 upstream host。
    let (up_host, up_port) = split_host_port(&cfg.upstream);
    let up_tcp = tokio::net::TcpStream::connect((up_host.as_str(), up_port)).await?;
    let connector = tokio_rustls::TlsConnector::from(upstream_cfg);
    let server_name = rustls::pki_types::ServerName::try_from(up_host.clone())?;
    let mut up = connector.connect(server_name, up_tcp).await?;

    // 4) 发改写后头部 + 终止空行 + 头之后已读体。
    up.write_all(&rewritten).await?;
    up.write_all(b"\r\n\r\n").await?;
    if !body_so_far.is_empty() {
        up.write_all(body_so_far).await?;
    }
    up.flush().await?;

    // 5) 双向中继:客户端→上游(剩余请求体)+ 上游→客户端(响应)。
    //    helper 发 Connection: close ⇒ 上游发完响应即关,u2c 先 EOF,select 返回后整个连接收尾。
    let (mut client_r, mut client_w) = tokio::io::split(tls);
    let (mut up_r, mut up_w) = tokio::io::split(up);

    let c2u = async {
        tokio::io::copy(&mut client_r, &mut up_w).await?;
        up_w.shutdown().await
    };
    let u2c = async {
        tokio::io::copy(&mut up_r, &mut client_w).await?;
        client_w.shutdown().await
    };

    // 任一方向结束即返回(对 Connection: close 的 git smart-HTTP 足够);外层 timeout 兜底。
    tokio::select! {
        _ = c2u => {}
        _ = u2c => {}
    }
    Ok(())
}

/// `host:port` → `(host, port)`;无端口 → 默认 443。
fn split_host_port(s: &str) -> (String, u16) {
    match s.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
        None => (s.to_string(), 443),
    }
}

/// 构建入站 TLS 服务端配置。cert/key 必填(`from_env` 已 fail-fast 校验)。
/// I-5:**无自签回退** —— 生产 misconfig(忘设 CERT_PEM/KEY_PEM)在 `from_env` 即退出码 2,
/// 而不是静默起一个自签证(fail-open 风险)。测试用证书由 tests/common 生成后经 env 注入。
pub fn build_server_config(
    cfg: &config::SwapConfig,
) -> Result<rustls::ServerConfig, Box<dyn std::error::Error>> {
    let cert_chain = load_certs(&cfg.cert_pem)?;
    let key_der = load_key(&cfg.key_pem)?;
    Ok(rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key_der)?)
}

/// 构建连真上游的 TLS 客户端配置。CA 优先 env(`FIXUS_SWAP_UPSTREAM_CA_PEM`);缺则 webpki-roots。
pub fn build_upstream_client_config(
    cfg: &config::SwapConfig,
) -> Result<rustls::ClientConfig, Box<dyn std::error::Error>> {
    let root_store = match &cfg.upstream_ca_pem {
        Some(pem) => {
            let mut s = rustls::RootCertStore::empty();
            let mut reader = std::io::Cursor::new(pem.as_bytes());
            for cert in rustls_pemfile::certs(&mut reader) {
                let cert = cert?;
                let _ = s.add(cert);
            }
            if s.is_empty() {
                return Err("FIXUS_SWAP_UPSTREAM_CA_PEM set but contained no certs".into());
            }
            s
        }
        None => {
            let mut s = rustls::RootCertStore::empty();
            s.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            s
        }
    };
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth())
}

fn load_certs(
    pem: &str,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, Box<dyn std::error::Error>> {
    // rustls-pemfile 2.x 的 certs() 已 yield CertificateDer<'static>(PEM 解码出的 DER 已拥有),
    // 故无需 into_owned()。
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let mut v = Vec::new();
    for cert in rustls_pemfile::certs(&mut reader) {
        v.push(cert?);
    }
    Ok(v)
}

fn load_key(
    pem: &str,
) -> Result<rustls::pki_types::PrivateKeyDer<'static>, Box<dyn std::error::Error>> {
    // rustls-pemfile 2.x 的 private_key() 已返回 PrivateKeyDer<'static>。
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let key = rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| "no private key found in FIXUS_SWAP_KEY_PEM".to_string())?;
    Ok(key)
}
