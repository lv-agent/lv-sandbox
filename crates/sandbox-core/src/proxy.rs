//! cr-019: SOCKS5h over UDS 受控出口代理。
//!
//! server 进程内运行:监听 per-job UDS,SOCKS5h 握手,按 allowlist 校验,
//! 远程 DNS + 真 TCP 连接上游,双向 relay。任务侧只能建 AF_UNIX(seccomp),
//! 故所有出站必经此代理。

use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::egress::AllowlistMatcher;

/// 运行 per-job SOCKS5h 代理,直到 cancel 或 listener 关闭。
pub async fn run_job_proxy(
    listener: UnixListener,
    matcher: AllowlistMatcher,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            res = listener.accept() => match res {
                Ok((stream, _)) => {
                    let m = matcher.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, m).await {
                            tracing::debug!(error = %e, "socks5 connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::debug!(error = %e, "proxy accept exited");
                    break;
                }
            }
        }
    }
}

/// 处理单个 SOCKS5h 连接。
async fn handle_conn(mut stream: UnixStream, matcher: AllowlistMatcher) -> std::io::Result<()> {
    // 1) 问候:VER, NMETHODS, METHODS... → 回 NO-AUTH
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).await?;
    if hdr[0] != 0x05 {
        return Ok(()); // 非 SOCKS5
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    stream.read_exact(&mut methods).await?;
    stream.write_all(&[0x05, 0x00]).await?;

    // 2) 请求:VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT
    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    if req[0] != 0x05 {
        return Ok(());
    }
    if req[1] != 0x01 {
        // 非 CONNECT
        reply(&mut stream, 0x07).await?;
        return Ok(());
    }
    let (host, port) = match req[3] {
        0x03 => {
            // DOMAINNAME(强制远程 DNS)
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut host_buf = vec![0u8; len[0] as usize];
            stream.read_exact(&mut host_buf).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            (
                String::from_utf8_lossy(&host_buf).into_owned(),
                u16::from_be_bytes(port_buf),
            )
        }
        0x01 | 0x04 => {
            // IPv4/IPv6 字面量 → 拒绝(强制 DOMAIN)
            reply(&mut stream, 0x02).await?;
            return Ok(());
        }
        _ => {
            reply(&mut stream, 0x01).await?;
            return Ok(());
        }
    };

    // 3) allowlist 校验
    if !matcher.is_allowed(&host, port) {
        tracing::info!(%host, %port, "egress denied by allowlist");
        reply(&mut stream, 0x02).await?;
        return Ok(());
    }

    // 4) 远程 DNS + 连接(逐地址尝试)
    let addrs = match tokio::net::lookup_host(format!("{host}:{port}")).await {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(_) => {
            reply(&mut stream, 0x04).await?; // host unreachable
            return Ok(());
        }
    };
    if addrs.is_empty() {
        reply(&mut stream, 0x04).await?;
        return Ok(());
    }
    let mut upstream: Option<TcpStream> = None;
    for addr in addrs {
        match TcpStream::connect(&addr).await {
            Ok(s) => {
                upstream = Some(s);
                break;
            }
            Err(_) => continue,
        }
    }
    let mut up = match upstream {
        Some(s) => s,
        None => {
            reply(&mut stream, 0x05).await?; // connection refused
            return Ok(());
        }
    };

    // 5) 成功 + 双向 relay
    reply(&mut stream, 0x00).await?;
    tracing::info!(%host, %port, "egress allowed");
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut up).await;
    Ok(())
}

/// 写 SOCKS5 reply。REP 见 RFC 1928。
async fn reply(stream: &mut UnixStream, code: u8) -> std::io::Result<()> {
    stream
        .write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
}

/// per-job 代理句柄:启动时 bind UDS,停止时 cancel + 清理 socket 文件。
pub struct JobProxy {
    task: Option<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
    sock_path: std::path::PathBuf,
}

impl JobProxy {
    /// 在 workspace 内 bind `.proxy.sock` 并起代理。返回 (句柄, socket 路径)。
    pub async fn start(
        workspace: &Path,
        matcher: AllowlistMatcher,
    ) -> std::io::Result<(Self, std::path::PathBuf)> {
        let sock_path = workspace.join(".proxy.sock");
        let _ = std::fs::remove_file(&sock_path); // 清理残留
        let listener = UnixListener::bind(&sock_path)?;
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let task = tokio::spawn(async move {
            run_job_proxy(listener, matcher, cancel2).await;
        });
        Ok((
            Self {
                task: Some(task),
                cancel,
                sock_path: sock_path.clone(),
            },
            sock_path,
        ))
    }

    /// 停止代理:cancel → 等 task(最多 500ms)→ 删 socket 文件。
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), task).await;
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

impl Drop for JobProxy {
    fn drop(&mut self) {
        // 安全网:覆盖 spawn 失败 / pre_exec 失败等早返回路径(此时未显式 stop)。
        // cancel 使 accept 循环退出,remove_file 清理 socket 目录项。
        // stop() 消耗 self 后 Drop 仍会运行一次,但 cancel()/remove_file 均幂等,无副作用。
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{AllowlistMatcher, EgressRule};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UnixStream};
    use tokio_util::sync::CancellationToken;

    /// 最小 SOCKS5h 客户端:拨 UDS,握手,CONNECT 到 (host, port),返回已就绪 stream。
    async fn socks5h_connect(
        proxy_path: &str,
        host: &str,
        port: u16,
    ) -> std::io::Result<UnixStream> {
        let mut s = UnixStream::connect(proxy_path).await?;
        s.write_all(&[0x05, 0x01, 0x00]).await?; // VER, NMETHODS=1, NO-AUTH
        let mut gr = [0u8; 2];
        s.read_exact(&mut gr).await?;
        assert_eq!(gr, [0x05, 0x00]);
        let hb = host.as_bytes();
        let mut req = vec![0x05, 0x01, 0x00, 0x03, hb.len() as u8];
        req.extend_from_slice(hb);
        req.extend_from_slice(&port.to_be_bytes());
        s.write_all(&req).await?;
        let mut rep = [0u8; 10];
        s.read_exact(&mut rep).await?;
        if rep[1] != 0x00 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("socks5 reply {}", rep[1]),
            ));
        }
        Ok(s)
    }

    #[tokio::test]
    async fn allowlisted_roundtrip_via_loopback_upstream() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");

        // mock 上游:回显收到的字节
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = [0u8; 16];
            let n = s.read(&mut buf).await.unwrap();
            s.write_all(&buf[..n]).await.unwrap();
        });

        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: Some(upstream_port),
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, cancel2).await });

        let mut s = socks5h_connect(proxy_path.to_str().unwrap(), "localhost", upstream_port)
            .await
            .expect("should connect to allowlisted upstream");
        s.write_all(b"PING").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"PING");

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn non_allowlisted_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: Some(1),
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, cancel2).await });

        let err = socks5h_connect(proxy_path.to_str().unwrap(), "evil.com", 443)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 gap1: JobProxy 不显式 stop 而 drop(模拟 pre_exec/spawn 失败早返回)时,
    /// Drop 应 cancel 代理 + 清理 socket 文件(否则 task + listener fd 泄漏)。
    #[tokio::test]
    async fn jobproxy_drop_stops_proxy_and_cleans_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        let matcher = AllowlistMatcher::new(vec![]);
        let (proxy, sock_path) = JobProxy::start(&ws, matcher).await.unwrap();
        assert!(sock_path.exists(), "socket file should exist after start");

        drop(proxy); // 不调 stop,直接 drop(模拟早返回路径)

        // Drop 同步执行 cancel + remove_file,socket 目录项应立即消失
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!sock_path.exists(), "socket file should be removed by Drop");
    }

    /// 发 SOCKS5 请求(cmd + atyp + host + port),返回 reply 的 REP 码(第二字节)。
    async fn socks5_request_rep(
        proxy_path: &str,
        cmd: u8,
        atyp: u8,
        host: &str,
        port: u16,
    ) -> std::io::Result<u8> {
        let mut s = UnixStream::connect(proxy_path).await?;
        s.write_all(&[0x05, 0x01, 0x00]).await?; // 问候
        let mut gr = [0u8; 2];
        s.read_exact(&mut gr).await?;
        let mut req = vec![0x05, cmd, 0x00, atyp];
        match atyp {
            0x03 => {
                let hb = host.as_bytes();
                req.push(hb.len() as u8);
                req.extend_from_slice(hb);
            }
            0x01 => req.extend_from_slice(&[127, 0, 0, 1]), // 占位 IPv4
            _ => {}
        }
        req.extend_from_slice(&port.to_be_bytes());
        s.write_all(&req).await?;
        let mut rep = [0u8; 10];
        s.read_exact(&mut rep).await?;
        Ok(rep[1])
    }

    /// cr-019 gap3:IPv4 字面量 ATYP 应被拒绝(强制 DOMAIN/远程 DNS)→ reply 0x02
    #[tokio::test]
    async fn proxy_rejects_ipv4_literal() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: None,
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        let rep = socks5_request_rep(proxy_path.to_str().unwrap(), 0x01, 0x01, "127.0.0.1", 80)
            .await
            .unwrap();
        assert_eq!(rep, 0x02, "IPv4-literal ATYP should be rejected");

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 gap3:非 CONNECT 命令(如 BIND)应回 command not supported → reply 0x07
    #[tokio::test]
    async fn proxy_rejects_non_connect() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: None,
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        let rep = socks5_request_rep(proxy_path.to_str().unwrap(), 0x02, 0x03, "localhost", 80)
            .await
            .unwrap();
        assert_eq!(rep, 0x07, "non-CONNECT should return command not supported");

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 gap3:白名单内但上游无监听 → connection refused → reply 0x05
    #[tokio::test]
    async fn proxy_upstream_refused_returns_refused() {
        // 取一个端口后立即 drop,保证它无人监听(loopback 上连空闲端口 → ECONNREFUSED)
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);

        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: Some(closed_port),
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        let rep =
            socks5_request_rep(proxy_path.to_str().unwrap(), 0x01, 0x03, "localhost", closed_port)
                .await
                .unwrap();
        assert_eq!(rep, 0x05, "upstream connect failure should return connection refused");

        cancel.cancel();
        let _ = task.await;
    }

    /// 原始客户端:连 UDS,发送任意字节,关闭写端,读回对端响应(2s 超时兜底防挂)。
    async fn raw_send_recv(proxy_path: &str, bytes: &[u8]) -> Vec<u8> {
        let mut s = UnixStream::connect(proxy_path).await.unwrap();
        let _ = s.write_all(bytes).await;
        let _ = s.shutdown().await; // 关闭写端,促使对端处理完关闭
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            s.read_to_end(&mut buf),
        )
        .await;
        buf
    }

    /// cr-019 malformed: 问候 VER 非 5 → 代理静默关闭(不回复),不挂。
    #[tokio::test]
    async fn malformed_bad_ver_silently_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        let buf = raw_send_recv(proxy_path.to_str().unwrap(), &[0x04, 0x01, 0x00]).await;
        assert!(buf.is_empty(), "bad VER: silent close, no reply, actual: {:?}", buf);

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 malformed: 截断问候(只发 1 字节)→ 代理 read_exact 失败,不挂。
    #[tokio::test]
    async fn malformed_truncated_greeting_no_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        let buf = raw_send_recv(proxy_path.to_str().unwrap(), &[0x05]).await;
        assert!(buf.is_empty(), "truncated greeting: handled via EOF, no reply");

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 malformed: 未知 ATYP → 回 general failure(reply 0x01)。
    #[tokio::test]
    async fn malformed_unknown_atyp_returns_general_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: None,
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        // 问候[5,1,0] + 请求[5,1,0,9](ATYP=0x09 未知)
        let buf = raw_send_recv(
            proxy_path.to_str().unwrap(),
            &[0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x09],
        )
        .await;
        // buf = 问候回复[5,0] + 错误回复[5,REP,...],REP 在 buf[3]
        assert!(
            buf.len() >= 4 && buf[3] == 0x01,
            "unknown ATYP should return REP=0x01, actual: {:?}",
            buf
        );

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 malformed: DOMAIN 声明长度但字节不足 → read_exact EOF,不挂。
    #[tokio::test]
    async fn malformed_truncated_domain_no_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![EgressRule {
            host: "localhost".into(),
            port: None,
        }]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        // 问候[5,1,0] + 请求[5,1,0,3](DOMAIN) + len=5,但只给 2 字节 host
        let buf = raw_send_recv(
            proxy_path.to_str().unwrap(),
            &[0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x03, 0x05, b'a', b'b'],
        )
        .await;
        // 代理先回问候[5,0],再读 host 时 EOF → 不写连接回复
        assert_eq!(
            buf,
            vec![0x05, 0x00],
            "truncated DOMAIN: only greeting reply, no connect reply, actual: {:?}",
            buf
        );

        cancel.cancel();
        let _ = task.await;
    }

    /// cr-019 malformed: 超大 NMETHODS(255)但字节不足 → read_exact EOF,不挂、不 OOM。
    #[tokio::test]
    async fn malformed_oversized_nmethods_no_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let proxy_path = tmp.path().join(".proxy.sock");
        let matcher = AllowlistMatcher::new(vec![]);
        let listener = tokio::net::UnixListener::bind(&proxy_path).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let task = tokio::spawn(async move { run_job_proxy(listener, matcher, c2).await });

        // 问候 VER=5, NMETHODS=255, 但不给 methods 字节
        let buf = raw_send_recv(proxy_path.to_str().unwrap(), &[0x05, 0xff]).await;
        assert!(buf.is_empty(), "oversized NMETHODS truncation: handled via EOF, no reply");

        cancel.cancel();
        let _ = task.await;
    }
}
