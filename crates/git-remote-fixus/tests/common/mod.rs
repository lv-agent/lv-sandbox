//! Shared test helpers for git-remote-fixus integration tests.
//!
//! cr-12 G1 (`tests/e2e_clone.rs`) 与 cr-12 G2(`tests/cr12_g2_swap_clone_push.rs`)
//! 共用的脚手架:rcgen 自签证书 + SOCKS5h-over-UDS 代理 + TLS git-http-backend CGI
//! 上游(可选记录 Authorization 头)+ git 命令包装。
//!
//! 刻意保持同步 std::thread 风格(G1 测试本身不依赖 tokio);G2 的 swap-proxy 在
//! 各自测试文件内用 tokio task 起,与此处的同步上游解耦。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// git-remote-fixus 二进制路径(由 cargo 在编译时注入)。
pub const HELPER: &str = env!("CARGO_BIN_EXE_git-remote-fixus");

/// 自签证书(localhost):既作 TLS 服务器证书,也作 PEM 信任根。
pub struct TestCert {
    pub cert_pem: String,
    pub key_pem: String,
}

/// 生成 localhost SAN 自签证书。
pub fn gen_cert() -> TestCert {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    TestCert {
        cert_pem: ck.cert.pem(),
        key_pem: ck.signing_key.serialize_pem(),
    }
}

pub fn pem_chain(pem: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    rustls_pemfile::certs(&mut std::io::Cursor::new(pem))
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

pub fn pem_key(pem: &str) -> rustls::pki_types::PrivateKeyDer<'static> {
    rustls_pemfile::private_key(&mut std::io::Cursor::new(pem))
        .unwrap()
        .unwrap()
}

/// 起 SOCKS5h-over-UDS 代理(允许 `allow_host`)。
pub fn spawn_proxy(sock_path: PathBuf, allow_host: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let listener = match UnixListener::bind(&sock_path) {
            Ok(l) => l,
            Err(_) => return,
        };
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let allow = allow_host.clone();
            thread::spawn(move || handle_socks(&mut s, &allow));
        }
    })
}

fn handle_socks(s: &mut std::os::unix::net::UnixStream, allow: &str) {
    let mut hdr = [0u8; 2];
    if s.read_exact(&mut hdr).is_err() {
        return;
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    let _ = s.read_exact(&mut methods);
    let _ = s.write_all(&[0x05, 0x00]);
    let mut req = [0u8; 4];
    if s.read_exact(&mut req).is_err() {
        return;
    }
    if req[1] != 0x01 {
        let _ = s.write_all(&[0x05, 0x07, 0, 1, 0, 0, 0, 0, 0, 0]);
        return;
    }
    let (host, port) = match req[3] {
        0x03 => {
            let mut len = [0u8; 1];
            let _ = s.read_exact(&mut len);
            let mut hb = vec![0u8; len[0] as usize];
            let _ = s.read_exact(&mut hb);
            let mut pb = [0u8; 2];
            let _ = s.read_exact(&mut pb);
            (String::from_utf8_lossy(&hb).into_owned(), u16::from_be_bytes(pb))
        }
        _ => return,
    };
    if !host.eq_ignore_ascii_case(allow) {
        let _ = s.write_all(&[0x05, 0x02, 0, 1, 0, 0, 0, 0, 0, 0]);
        return;
    }
    let up = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(u) => u,
        Err(_) => {
            let _ = s.write_all(&[0x05, 0x05, 0, 1, 0, 0, 0, 0, 0, 0]);
            return;
        }
    };
    let _ = s.write_all(&[0x05, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    let s2 = s.try_clone().unwrap();
    let up2 = up.try_clone().unwrap();
    let mut s_owned = s.try_clone().unwrap();
    let mut up_owned = up;
    let t = thread::spawn(move || {
        copy_stream(&mut s_owned, &mut up_owned);
        let _ = up_owned.shutdown(std::net::Shutdown::Write);
    });
    let mut up2 = up2;
    let mut s2 = s2;
    copy_stream(&mut up2, &mut s2);
    let _ = s2.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

fn copy_stream<R: Read, W: Write>(r: &mut R, w: &mut W) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if w.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// TLS git-http-backend CGI 服务器。
///
/// `project_root` = 含仓库的目录;仓库以 PATH_INFO 相对定位。
/// `auth_seen` —— 每条请求把 `Authorization` 头值写入(最后写赢;G1 传 throwaway)。
pub fn spawn_cgi_tls_server(
    project_root: PathBuf,
    auth_seen: Arc<Mutex<Option<String>>>,
) -> (u16, TestCert) {
    let cert = gen_cert();
    let chain = pem_chain(&cert.cert_pem);
    let key = pem_key(&cert.key_pem);
    let server_cfg = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cfg = server_cfg;
    let root = project_root;
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(tcp) = stream else { break };
            let cfg = cfg.clone();
            let root = root.clone();
            let auth_seen = auth_seen.clone();
            thread::spawn(move || {
                let conn = match rustls::ServerConnection::new(cfg) {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                if let Err(e) = serve_one(&mut tls, &root, &auth_seen) {
                    eprintln!("cgi server error: {e}");
                }
            });
        }
    });
    (port, cert)
}

#[allow(clippy::needless_pass_by_value)]
fn serve_one(
    tls: &mut rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
    root: &Path,
    auth_seen: &Arc<Mutex<Option<String>>>,
) -> std::io::Result<()> {
    // 读取请求行 + 头(若 POST:继续读至 body 收齐)
    let mut req = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf)? {
            0 => break,
            n => {
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    if let Some(cl) = header(&req, "content-length") {
                        let cl: usize = cl.parse().unwrap_or(0);
                        let body_start =
                            req.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        if req.len() - body_start >= cl {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }

    // G2:记录 Authorization 头值(swap 是否生效的关键证据)。
    if let Some(v) = header(&req, "authorization") {
        if let Ok(mut g) = auth_seen.lock() {
            *g = Some(v);
        }
    }

    let text = String::from_utf8_lossy(&req).into_owned();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut rl = request_line.split_whitespace();
    let method = rl.next().unwrap_or("GET").to_string();
    let raw_path = rl.next().unwrap_or("/").to_string();
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_path, String::new()),
    };
    let ctype = header(&req, "content-type").unwrap_or_default();
    let clen = header(&req, "content-length").unwrap_or_else(|| "0".into());
    let body_start = req
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(req.len());
    let body = &req[body_start..];

    // 调 git http-backend(CGI)
    let mut cmd = Command::new("git");
    cmd.arg("http-backend");
    cmd.env_clear();
    cmd.env("GIT_PROJECT_ROOT", root);
    cmd.env("GIT_HTTP_EXPORT_ALL", "1");
    cmd.env("PATH_INFO", &path);
    if !query.is_empty() {
        cmd.env("QUERY_STRING", &query);
    }
    cmd.env("REQUEST_METHOD", &method);
    cmd.env("CONTENT_TYPE", &ctype);
    cmd.env("CONTENT_LENGTH", &clen);
    cmd.env("REMOTE_ADDR", "127.0.0.1");
    cmd.env("REMOTE_USER", "fixus-test");
    cmd.env("GATEWAY_INTERFACE", "CGI/1.1");
    cmd.env("SERVER_PROTOCOL", "HTTP/1.1");
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    let mut child = cmd.spawn()?;
    {
        let mut cin = child.stdin.take().unwrap();
        if method == "POST" {
            cin.write_all(body)?;
        }
    }
    let out = child.wait_with_output()?;
    // CGI 输出:头 + 空行 + 体。Status 行可选(默认 200)。
    let cgi = out.stdout;
    let sep = cgi
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(cgi.len());
    let head = &cgi[..sep];
    let cgi_body = if sep < cgi.len() { &cgi[sep + 4..] } else { &[][..] };
    let mut status = "200 OK".to_string();
    let mut extra_headers: Vec<&[u8]> = Vec::new();
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(b"Status:") {
            status = String::from_utf8_lossy(rest).trim().to_string();
        } else {
            extra_headers.push(line);
        }
    }
    let mut resp = format!("HTTP/1.1 {status}\r\n").into_bytes();
    for h in &extra_headers {
        resp.extend_from_slice(h);
        resp.extend_from_slice(b"\r\n");
    }
    resp.extend_from_slice(format!("Content-Length: {}\r\n", cgi_body.len()).as_bytes());
    resp.extend_from_slice(b"Connection: close\r\n\r\n");
    resp.extend_from_slice(cgi_body);
    tls.write_all(&resp)?;
    tls.flush()?;
    let _ = tls.get_ref().shutdown(std::net::Shutdown::Write);
    Ok(())
}

/// 按头名查找(大小写不敏感),返回**原样大小写**的头值。
///
/// 关键:不能整体 `to_ascii_lowercase` —— 这会丢掉 Authorization token 的原始
/// 大小写,让 G2 的 swap 断言(`Bearer <REAL_TOKEN>` 精确大小写比较)失效。
fn header(req: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(req);
    for line in text.split("\r\n").skip(1) {
        // header line: "Name: value"
        if let Some((n, v)) = line.split_once(':') {
            if n.trim().eq_ignore_ascii_case(name) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// 同步 git 子进程;失败 panic(stderr 进消息)。
pub fn git(workdir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(workdir)
        .args(args)
        .output()
        .expect("git");
    if !out.status.success() {
        panic!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 让 git 找到 helper:把 helper 所在目录加到 PATH 前面。
pub fn path_with_helper() -> std::ffi::OsString {
    let helper_dir = Path::new(HELPER).parent().unwrap().to_path_buf();
    let cur = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = std::ffi::OsString::new();
    joined.push(helper_dir.as_os_str());
    joined.push(":");
    joined.push(&cur);
    joined
}

/// 等待 UDS 就绪(最多 ~1s)。
pub fn wait_socket(p: &Path) {
    for _ in 0..100 {
        if p.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}
