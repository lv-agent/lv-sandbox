//! cr-12 G1 步骤3.2:拨号层 —— UDS 连接 → SOCKS5h 握手 → rustls TLS 握手 → 加密流。
//!
//! AF_UNIX-only jail 内的进程无法建 TCP(seccomp),故所有出站必经 per-job 的
//! SOCKS5h-over-UDS 代理(`SANDBOX_PROXY_SOCK`)。本模块实现该路径的客户端侧:
//!
//! 1. 连接代理 UDS(`std::os::unix::net::UnixStream`)。
//! 2. SOCKS5h 握手(RFC 1928,DOMAIN ATYP 强制远程 DNS)。字节序列与
//!    `sandbox_core::proxy` 的服务端实现严格对应:
//!    - 问候:`[05 01 00]`(VER, NMETHODS=1, NO-AUTH)→ 期待 `[05 00]`
//!    - 请求:`[05 01 00 03 <len> <host> <port-be>]`(DOMAINNAME,CONNECT)→ 期待 reply 第二字节 `00`
//! 3. rustls 客户端握手(SNI=host),返回 `StreamOwned<ClientConnection, UnixStream>`。
//!
//! 成功后调用方拿到的是 `Read+Write` 的 TLS 流;HTTP/git 协议在其上运行。

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};

/// 拨号成功的 TLS 流(底层 = UDS,经 SOCKS5h 到上游后 TLS 包装)。
pub type TlsStream = rustls::StreamOwned<ClientConnection, UnixStream>;

/// 用 Mozilla 内置根证书(webpki-roots)构造生产用 ClientConfig。
pub fn default_client_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// 同 [`default_client_config`],但若环境 `SANDBOX_CA_FILE` 指向一个 PEM CA 文件,
/// 则以其根证书**替换**内置根(供内部/自签 git 上游使用;jail 注入)。
pub fn env_client_config() -> Arc<ClientConfig> {
    match std::env::var("SANDBOX_CA_FILE") {
        Ok(path) if !path.is_empty() => ca_file_config(&path).unwrap_or_else(|_| default_client_config()),
        _ => default_client_config(),
    }
}

fn ca_file_config(path: &str) -> io::Result<Arc<ClientConfig>> {
    let pem = std::fs::read_to_string(path)?;
    let mut roots = RootCertStore::empty();
    let mut reader = std::io::Cursor::new(pem);
    for cert in rustls_pemfile::certs(&mut reader) {
        let cert = cert.map_err(|e| io::Error::other(e.to_string()))?;
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(io::Error::other(format!("no CA certs in {path}")));
    }
    Ok(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// 经 SOCKS5h-over-UDS 代理拨到 `(host, port)` 并完成 TLS 握手,返回加密流。
///
/// 使用生产默认根证书(webpki-roots)。测试或自签场景请用 [`connect_with_config`]。
#[allow(dead_code)] // 公共便利入口(被 http.rs 经 connect_with_config 间接使用)
pub fn connect(proxy_sock: &Path, host: &str, port: u16) -> io::Result<TlsStream> {
    connect_with_config(proxy_sock, host, port, default_client_config())
}

/// 同 [`connect`],但由调用方提供 rustls `ClientConfig`(测试可传跳过校验的配置)。
pub fn connect_with_config(
    proxy_sock: &Path,
    host: &str,
    port: u16,
    config: Arc<ClientConfig>,
) -> io::Result<TlsStream> {
    // 1) 连代理 UDS
    let mut sock = UnixStream::connect(proxy_sock)?;

    // 2) SOCKS5h 问候:VER=5, NMETHODS=1, NO-AUTH(0x00)
    sock.write_all(&[0x05, 0x01, 0x00])?;
    let mut greeting_reply = [0u8; 2];
    sock.read_exact(&mut greeting_reply)?;
    if greeting_reply != [0x05, 0x00] {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!(
                "socks5 greeting rejected: got {:?}, expected [05 00]",
                greeting_reply
            ),
        ));
    }

    // 3) SOCKS5h CONNECT 请求:VER=5, CMD=CONNECT(1), RSV=0, ATYP=DOMAIN(3)
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host name too long for SOCKS5 DOMAIN (>255 bytes)",
        ));
    }
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    sock.write_all(&req)?;

    // 期待 10 字节 reply;REP 在第二字节,0x00=成功,其余=失败。
    // 假设 IPv4 ATYP 应答:沙箱代理固定回 [05 00 00 01 <4 BND.ADDR> <2 BND.PORT>] = 10 字节。
    let mut rep = [0u8; 10];
    sock.read_exact(&mut rep)?;
    if rep[0] != 0x05 {
        return Err(io::Error::other(format!("socks5 reply bad VER: {}", rep[0])));
    }
    if rep[1] != 0x00 {
        // REP 码见 RFC 1928。0x02=connection not allowed by ruleset(白名单拒绝)。
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socks5 connect rejected: REP={}", rep[1]),
        ));
    }

    // 4) rustls 客户端握手(SNI=host)
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let conn = ClientConnection::new(config, server_name)
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string()))?;
    let stream = rustls::StreamOwned::new(conn, sock);

    // StreamOwned::new 不驱动握手;强制完成握手以确保证书校验/连通性在拨号阶段就暴露。
    let mut stream = stream;
    stream.flush()?; // 触发 ClientConnection 发送 ClientHello 并完成握手
    Ok(stream)
}

// ──────────────────────────── 测试支持 ────────────────────────────

/// 构造一个跳过证书校验的 ClientConfig(仅测试用,自签证书场景)。
#[cfg(test)]
pub(crate) fn insecure_client_config() -> Arc<ClientConfig> {
    Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth(),
    )
}

#[cfg(test)]
#[derive(Debug)]
struct NoVerify;

#[cfg(test)]
impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

/// 从 PEM 文本解析证书链(测试服务器用)。
#[cfg(test)]
pub(crate) fn pem_chain(pem: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    let mut reader = std::io::Cursor::new(pem);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("valid cert pem")
}

/// 从 PEM 文本解析私钥(测试服务器用)。
#[cfg(test)]
pub(crate) fn pem_key(pem: &str) -> rustls::pki_types::PrivateKeyDer<'static> {
    let mut reader = std::io::Cursor::new(pem);
    rustls_pemfile::private_key(&mut reader)
        .expect("valid private key pem")
        .expect("at least one private key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// 极简 *阻塞* SOCKS5h-over-UDS 代理(测试替身,复刻 sandbox_core::proxy 的客户端侧契约)。
    /// `allow` 为允许的主机集合(空=拒绝全部),用于验证白名单拒绝路径。
    fn spawn_test_proxy(sock_path: PathBuf, allow: Vec<String>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let listener = match std::os::unix::net::UnixListener::bind(&sock_path) {
                Ok(l) => l,
                Err(_) => return,
            };
            for stream in listener.incoming() {
                let Ok(s) = stream else { break };
                let allow = allow.clone();
                // 每连接独立线程
                thread::spawn(move || {
                    let _ = handle_socks(s, &allow);
                });
            }
        })
    }

    fn handle_socks(mut s: std::os::unix::net::UnixStream, allow: &[String]) -> io::Result<()> {
        // 问候
        let mut hdr = [0u8; 2];
        s.read_exact(&mut hdr)?;
        if hdr[0] != 0x05 {
            return Ok(());
        }
        let mut methods = vec![0u8; hdr[1] as usize];
        s.read_exact(&mut methods)?;
        s.write_all(&[0x05, 0x00])?;
        // 请求
        let mut req = [0u8; 4];
        s.read_exact(&mut req)?;
        if req[1] != 0x01 {
            s.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
            return Ok(());
        }
        let host = match req[3] {
            0x03 => {
                let mut len = [0u8; 1];
                s.read_exact(&mut len)?;
                let mut hb = vec![0u8; len[0] as usize];
                s.read_exact(&mut hb)?;
                let mut pb = [0u8; 2];
                s.read_exact(&mut pb)?;
                (String::from_utf8_lossy(&hb).into_owned(), u16::from_be_bytes(pb))
            }
            _ => {
                // IPv4/IPv6 字面量 → 拒绝(强制 DOMAIN)
                s.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
                return Ok(());
            }
        };
        if !allow.iter().any(|a| a.eq_ignore_ascii_case(&host.0)) {
            s.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
            return Ok(());
        }
        // 连上游(loopback,TCP)
        let mut upstream = match TcpStream::connect(("127.0.0.1", host.1)) {
            Ok(u) => u,
            Err(_) => {
                s.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
                return Ok(());
            }
        };
        s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
        // 双向 relay(阻塞):client↔upstream
        let mut client2 = s.try_clone()?;
        let mut upstream2 = upstream.try_clone()?;
        let t = thread::spawn(move || {
            // client → upstream
            let _ = copy_until_eof(&mut s, &mut upstream);
            let _ = upstream.shutdown(std::net::Shutdown::Write);
        });
        // upstream → client
        let _ = copy_until_eof(&mut upstream2, &mut client2);
        let _ = client2.shutdown(std::net::Shutdown::Write);
        let _ = t.join();
        Ok(())
    }

    fn copy_until_eof<R: Read, W: Write>(r: &mut R, w: &mut W) -> io::Result<u64> {
        let mut buf = [0u8; 4096];
        let mut total = 0;
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    w.write_all(&buf[..n])?;
                    total += n as u64;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        let _ = w.flush();
        // 半关闭写端,促使对端收尾
        // (UnixStream/TcpStream 无通用 shutdown,忽略)
        Ok(total)
    }

    /// 启一个阻塞 TLS 回显上游:收到什么就原样回。
    fn spawn_tls_echo_server() -> (u16, Arc<AtomicBool>) {
        // 自签证书(localhost)
        let certified_key =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_pem = certified_key.cert.pem();
        let key_pem = certified_key.signing_key.serialize_pem();
        let chain = pem_chain(&cert_pem);
        let key = pem_key(&key_pem);

        let server_cfg = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap(),
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(tcp) = stream else { break };
                let cfg = server_cfg.clone();
                thread::spawn(move || {
                    let conn = match rustls::ServerConnection::new(cfg) {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut tls = rustls::StreamOwned::new(conn, tcp);
                    // 回显:读一段写回
                    let mut buf = [0u8; 1024];
                    loop {
                        match tls.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if tls.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        });
        (port, stop)
    }

    #[test]
    fn socks5h_tls_roundtrip() {
        // 给上游一小段时间起来
        let (port, _stop) = spawn_tls_echo_server();
        // 偶尔端口未就绪,小睡等一下
        thread::sleep(Duration::from_millis(50));

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join(".proxy.sock");
        let _proxy = spawn_test_proxy(sock_path.clone(), vec!["localhost".into()]);

        // 等代理 bind
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let mut stream =
            connect_with_config(&sock_path, "localhost", port, insecure_client_config())
                .expect("dial should succeed");

        let msg = b"hello-fixus!";
        stream.write_all(msg).unwrap();
        // 主动 flush 触发 TLS 发送
        stream.flush().unwrap();

        let mut got = vec![0u8; msg.len()];
        stream.read_exact(&mut got).unwrap();
        assert_eq!(got, msg);
    }

    #[test]
    fn non_allowlisted_host_is_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join(".proxy.sock");
        // 白名单只放 evil-allowed,实际请求 localhost → 拒绝
        let _proxy = spawn_test_proxy(sock_path.clone(), vec!["evil-allowed.example".into()]);
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let err = connect_with_config(&sock_path, "localhost", 443, insecure_client_config())
            .expect_err("non-allowlisted host must be denied");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "expected PermissionDenied, got: {err:?}"
        );
    }

    #[test]
    fn ipv4_literal_rejected_by_proxy() {
        // 代理强制 DOMAIN,IPv4 ATYP 应被拒(reply 0x02)→ PermissionDenied
        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join(".proxy.sock");
        let _proxy = spawn_test_proxy(sock_path.clone(), vec!["127.0.0.1".into()]);
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        // 直接发 IPv4 ATYP 的握手,绕过 connect()(它总用 DOMAIN)
        let mut s = std::os::unix::net::UnixStream::connect(&sock_path).unwrap();
        use std::io::Write;
        s.write_all(&[0x05, 0x01, 0x00]).unwrap();
        let mut gr = [0u8; 2];
        s.read_exact(&mut gr).unwrap();
        // IPv4 ATYP=0x01
        s.write_all(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0x00, 0x50])
            .unwrap();
        let mut rep = [0u8; 10];
        s.read_exact(&mut rep).unwrap();
        assert_eq!(rep[1], 0x02, "IPv4-literal ATYP should be rejected");
    }
}
