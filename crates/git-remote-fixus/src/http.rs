//! cr-12 G1 步骤3.3: 实现 gix [`Http`] trait —— HTTP/1.1 GET/POST over 拨号层的 TLS 流。
//!
//! 设计要点:
//! - 每次请求新拨一条连接(`dialer::connect`),无连接复用(简单;git smart-HTTP 每次操作 1 GET + 1 POST,可接受)。
//! - GET:立即发请求;POST:`post()` 返回 `PostBody` 缓冲请求体,drop 时带 `Content-Length` 一起发出
//!   (git 的 upload-pack/receive-pack 请求体均可入内存;G1 已知限制,见 self-review)。
//! - 响应帧:`Content-Length`(定长)、`Transfer-Encoding: chunked`(分块)、其余读到 EOF(`Connection: close`)。
//! - 状态码契约:401→`io::Error(PermissionDenied)`,其它非 2xx→`io::Error(Other)`,
//!   经 `Headers`/`ResponseBody` 的首次读返回(与 gix-transport reqwest 后端 `error_for_status` 语义一致)。
//!
//! HTTP 逻辑以 `Conn`(单条 `TlsStream` + 解析状态)承载,trait 的三个关联类型
//! (`Headers`/`ResponseBody`/`PostBody`)是持有 `Arc<Mutex<Conn>>` 的薄封装;
//! 因 gix 先消费 `Headers`/`PostBody` 再读 `ResponseBody`,无锁竞争。

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gix_transport::client::blocking_io::http::{
    Error, GetResponse, Http, PostBodyDataKind, PostResponse,
};
use rustls::ClientConfig;

use crate::dialer;

/// git remote helper 用的 HTTP 客户端,实现 gix `Http` trait。
/// 出站路径固定 = UDS 代理(`proxy_sock`)→ SOCKS5h → TLS → HTTP/1.1。
pub struct FixusHttp {
    proxy_sock: PathBuf,
    config: Arc<ClientConfig>,
}

impl FixusHttp {
    pub fn new(proxy_sock: PathBuf, config: Arc<ClientConfig>) -> Self {
        Self { proxy_sock, config }
    }

    /// 用 webpki-roots 默认根证书;若环境 `SANDBOX_CA_FILE` 有值则改用该 CA。
    pub fn with_default_roots(proxy_sock: PathBuf) -> Self {
        Self::new(proxy_sock, dialer::env_client_config())
    }

    /// 测试用:自签场景传跳过校验的 config。
    #[cfg(test)]
    pub(crate) fn with_insecure(proxy_sock: PathBuf) -> Self {
        Self::new(proxy_sock, dialer::insecure_client_config())
    }
}

/// 单条连接的共享状态。
struct Conn {
    stream: Option<dialer::TlsStream>,
    read_buf: Vec<u8>,
    read_buf_pos: usize,
    src_eof: bool,
    // 响应
    resp_parsed: bool,
    status: u16,
    status_err: Option<(io::ErrorKind, String)>,
    head_lines: Vec<u8>,
    framing: Framing,
    body_read: u64,
    chunk_rem: usize,
    chunk_done: bool,
    // 请求(POST 缓冲)
    request_sent: bool,
}

#[derive(Clone, Copy)]
enum Framing {
    Len(u64),
    Chunked,
    Eof,
}

impl Conn {
    fn new(stream: dialer::TlsStream) -> Self {
        Conn {
            stream: Some(stream),
            read_buf: Vec::new(),
            read_buf_pos: 0,
            src_eof: false,
            resp_parsed: false,
            status: 0,
            status_err: None,
            head_lines: Vec::new(),
            framing: Framing::Eof,
            body_read: 0,
            chunk_rem: 0,
            chunk_done: false,
            request_sent: false,
        }
    }

    fn stream_mut(&mut self) -> io::Result<&mut dialer::TlsStream> {
        self.stream
            .as_mut()
            .ok_or_else(|| io::Error::other("connection already closed"))
    }

    /// 底层读到 `read_buf`(保证至少有 1 字节,除非 EOF)。
    fn pump(&mut self) -> io::Result<()> {
        if self.read_buf_pos < self.read_buf.len() {
            return Ok(());
        }
        self.read_buf.clear();
        self.read_buf_pos = 0;
        let mut tmp = [0u8; 8192];
        let stream = self.stream_mut()?;
        match stream.read(&mut tmp) {
            Ok(0) => self.src_eof = true,
            Ok(n) => self.read_buf.extend_from_slice(&tmp[..n]),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// 读一行(含尾部 `\n`);去 CR。无更多数据(EOF 且未取到 `\n`)返回 None。
    fn read_crlf_line(&mut self) -> io::Result<Option<String>> {
        let mut line = Vec::new();
        loop {
            if self.read_buf_pos >= self.read_buf.len() {
                self.pump()?;
                if self.src_eof && self.read_buf_pos >= self.read_buf.len() {
                    if line.is_empty() {
                        return Ok(None);
                    }
                    // 末行无换行
                    return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                }
            }
            let avail = &self.read_buf[self.read_buf_pos..];
            match avail.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    line.extend_from_slice(&avail[..i]);
                    self.read_buf_pos += i + 1;
                    // 去 CR
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                }
                None => {
                    line.extend_from_slice(avail);
                    self.read_buf_pos = self.read_buf.len();
                }
            }
        }
    }

    /// 发送原始请求(请求行 + 头 + body)。
    fn send_request(
        &mut self,
        method: &str,
        path: &str,
        host: &str,
        headers: &[String],
        body: Option<&[u8]>,
    ) -> io::Result<()> {
        let stream = self.stream_mut()?;
        write!(stream, "{method} {path} HTTP/1.1\r\n")?;
        write!(stream, "Host: {host}\r\n")?;
        // 要求连接在响应后关闭,便于 EOF 帧定界(亦可处理 chunked/Content-Length)。
        let mut had_conn = false;
        for h in headers {
            if h.to_ascii_lowercase().starts_with("connection:") {
                had_conn = true;
            }
            stream.write_all(h.as_bytes())?;
            if !h.ends_with('\n') {
                stream.write_all(b"\r\n")?;
            }
        }
        if !had_conn {
            stream.write_all(b"Connection: close\r\n")?;
        }
        match body {
            Some(b) => write!(stream, "Content-Length: {}\r\n\r\n", b.len())?,
            None => stream.write_all(b"\r\n")?,
        }
        if let Some(b) = body {
            stream.write_all(b)?;
        }
        stream.flush()?;
        self.request_sent = true;
        Ok(())
    }

    /// 解析状态行 + 响应头(幂等)。
    fn parse_response(&mut self) -> io::Result<()> {
        if self.resp_parsed {
            return Ok(());
        }
        self.resp_parsed = true;

        // 状态行:HTTP/1.1 200 OK
        let status_line = match self.read_crlf_line()? {
            Some(l) => l,
            None => {
                self.status_err = Some((io::ErrorKind::Other, "empty HTTP response".to_string()));
                return Ok(());
            }
        };
        let mut parts = status_line.splitn(3, ' ');
        let _ver = parts.next();
        let code = parts.next().unwrap_or("0");
        self.status = code.parse().unwrap_or(0);

        let mut chunked = false;
        let mut len: Option<u64> = None;
        while let Some(line) = self.read_crlf_line()? {
            if line.is_empty() {
                break; // 头结束
            }
            // 累积响应头行(供 Headers reader)。格式 "Name: value\n"
            self.head_lines.extend_from_slice(line.as_bytes());
            self.head_lines.push(b'\n');

            let (name, value) = match line.split_once(':') {
                Some((n, v)) => (n.trim(), v.trim()),
                None => continue,
            };
            if name.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }

        self.framing = if chunked {
            Framing::Chunked
        } else if let Some(n) = len {
            Framing::Len(n)
        } else {
            Framing::Eof
        };

        if self.status == 401 {
            self.status_err = Some((
                io::ErrorKind::PermissionDenied,
                "HTTP 401 Unauthorized".to_string(),
            ));
        } else if !(200..300).contains(&self.status) {
            self.status_err =
                Some((io::ErrorKind::Other, format!("HTTP status {}", self.status)));
        }
        Ok(())
    }

    /// 若有状态错误,重建一个 `io::Error`(可多次取用)。
    fn take_status_err(&self) -> Option<io::Error> {
        self.status_err
            .as_ref()
            .map(|(k, m)| io::Error::new(*k, m.clone()))
    }

    /// 读响应体到 `out`(按帧)。返回读到的字节数;0=结束。
    fn read_body(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(e) = self.take_status_err() {
            return Err(e);
        }
        if !self.resp_parsed {
            self.parse_response()?;
            if let Some(e) = self.take_status_err() {
                return Err(e);
            }
        }
        match self.framing {
            Framing::Eof => self.read_eof(out),
            Framing::Len(n) => {
                if self.body_read >= n {
                    return Ok(0);
                }
                let max = ((n - self.body_read) as usize).min(out.len());
                let got = self.read_raw(&mut out[..max])?;
                self.body_read += got as u64;
                Ok(got)
            }
            Framing::Chunked => self.read_chunked(out),
        }
    }

    fn read_eof(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.read_raw(out)
    }

    /// 从 read_buf/stream 直接读(透传 EOF)。
    fn read_raw(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.read_buf_pos >= self.read_buf.len() {
            self.pump()?;
        }
        if self.read_buf_pos >= self.read_buf.len() {
            return Ok(0); // EOF
        }
        let n = (self.read_buf.len() - self.read_buf_pos).min(out.len());
        out[..n].copy_from_slice(&self.read_buf[self.read_buf_pos..self.read_buf_pos + n]);
        self.read_buf_pos += n;
        Ok(n)
    }

    fn read_chunked(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if self.chunk_done {
                break;
            }
            if self.chunk_rem == 0 {
                // 读下一个 chunk size 行
                let line = match self.read_crlf_line()? {
                    Some(l) => l,
                    None => {
                        self.chunk_done = true;
                        break;
                    }
                };
                let size = match line.split(';').next().unwrap_or("").trim() {
                    s if !s.is_empty() => usize::from_str_radix(s, 16).unwrap_or(0),
                    _ => 0,
                };
                if size == 0 {
                    // 尾 chunk:跳过 trailing header / 空行
                    let _ = self.read_crlf_line();
                    self.chunk_done = true;
                    break;
                }
                self.chunk_rem = size;
            }
            // 先吐 read_buf 里属于本 chunk 的字节
            let want = self.chunk_rem.min(out.len() - written);
            let avail = (self.read_buf.len() - self.read_buf_pos).min(want);
            if avail > 0 {
                out[written..written + avail]
                    .copy_from_slice(&self.read_buf[self.read_buf_pos..self.read_buf_pos + avail]);
                self.read_buf_pos += avail;
                self.chunk_rem -= avail;
                written += avail;
            }
            if self.chunk_rem == 0 {
                // chunk 数据后跟 CRLF
                let _ = self.read_crlf_line();
            }
            if written < out.len() && self.read_buf_pos >= self.read_buf.len() {
                self.pump()?;
                if self.src_eof && self.read_buf_pos >= self.read_buf.len() {
                    break;
                }
            }
        }
        Ok(written)
    }
}

// ──────────────────────────── trait 关联类型 ────────────────────────────

type Shared = Arc<Mutex<Conn>>;

/// `Headers` reader:内部缓冲 `head_lines`(小,一次性拷入);状态错误首次读返回。
pub struct HeadersReader {
    conn: Shared,
    buf: Vec<u8>,
    pos: usize,
    loaded: bool,
    err_served: bool,
}

impl HeadersReader {
    fn ensure_loaded(&mut self) -> io::Result<()> {
        if !self.loaded {
            self.loaded = true;
            let mut c = self.conn.lock().unwrap();
            c.parse_response()?;
            self.buf.clear();
            self.buf.extend_from_slice(&c.head_lines);
        }
        Ok(())
    }
}

impl Read for HeadersReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.ensure_loaded()?;
        if !self.err_served {
            self.err_served = true;
            let c = self.conn.lock().unwrap();
            if let Some(e) = c.take_status_err() {
                return Err(e);
            }
        }
        let remaining = &self.buf[self.pos..];
        let n = remaining.len().min(out.len());
        out[..n].copy_from_slice(&remaining[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl BufRead for HeadersReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.ensure_loaded()?;
        if !self.err_served {
            self.err_served = true;
            let c = self.conn.lock().unwrap();
            if let Some(e) = c.take_status_err() {
                return Err(e);
            }
        }
        Ok(&self.buf[self.pos..])
    }
    fn consume(&mut self, amt: usize) {
        self.pos = (self.pos + amt).min(self.buf.len());
    }
}

/// `ResponseBody` reader:8KB 内部缓冲,从 `Conn::read_body` 续填;支持 BufRead。
pub struct BodyReader {
    conn: Shared,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl BodyReader {
    fn ensure(&mut self) -> io::Result<()> {
        if self.pos < self.buf.len() || self.eof {
            return Ok(());
        }
        self.buf.clear();
        self.pos = 0;
        let mut tmp = vec![0u8; 8192];
        let mut c = self.conn.lock().unwrap();
        let n = c.read_body(&mut tmp)?;
        if n == 0 {
            self.eof = true;
        } else {
            self.buf.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }
}

impl Read for BodyReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.ensure()?;
        if self.eof && self.pos >= self.buf.len() {
            return Ok(0);
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl BufRead for BodyReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.ensure()?;
        Ok(&self.buf[self.pos..])
    }
    fn consume(&mut self, amt: usize) {
        self.pos = (self.pos + amt).min(self.buf.len());
    }
}

/// `PostBody` writer:缓冲请求体;drop 时连 Content-Length 一起发出。
pub struct PostBody {
    conn: Shared,
    host: String,
    path: String,
    headers: Vec<String>,
    buf: Vec<u8>,
    sent: bool,
}

impl Write for PostBody {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.sent {
            return Err(io::Error::other("post body already sent"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PostBody {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        self.sent = true;
        if let Ok(mut c) = self.conn.lock() {
            let _ = c.send_request(
                "POST",
                &self.path,
                &self.host,
                &self.headers,
                Some(&self.buf),
            );
        }
    }
}

/// 把 `https://host[:port]/path?q` 拆成 (host_with_port, path_with_query)。
fn split_url(url: &str) -> io::Result<(String, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = if path.is_empty() { "/" } else { path };
    Ok((auth.to_string(), path.to_string()))
}

impl Http for FixusHttp {
    type Headers = HeadersReader;
    type ResponseBody = BodyReader;
    type PostBody = PostBody;

    fn get(
        &mut self,
        url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<GetResponse<Self::Headers, Self::ResponseBody>, Error> {
        let (host, path) = split_url(url).map_err(io_err)?;
        let host_only = host.split(':').next().unwrap_or(&host);
        // 默认端口 443(https)/ 80(http)
        let port = port_from_auth(&host);
        let stream =
            dialer::connect_with_config(&self.proxy_sock, host_only, port, self.config.clone())
                .map_err(io_err)?;
        let mut conn = Conn::new(stream);
        let hdrs: Vec<String> = headers.into_iter().map(|h| h.as_ref().to_string()).collect();
        conn.send_request("GET", &path, &host, &hdrs, None)
            .map_err(io_err)?;
        let shared = Arc::new(Mutex::new(conn));
        Ok(GetResponse {
            headers: HeadersReader {
                conn: shared.clone(),
                buf: Vec::new(),
                pos: 0,
                loaded: false,
                err_served: false,
            },
            body: BodyReader {
                conn: shared,
                buf: Vec::new(),
                pos: 0,
                eof: false,
            },
        })
    }

    fn post(
        &mut self,
        url: &str,
        _base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        _body: PostBodyDataKind,
    ) -> Result<PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>, Error> {
        let (host, path) = split_url(url).map_err(io_err)?;
        let host_only = host.split(':').next().unwrap_or(&host);
        let port = port_from_auth(&host);
        let stream =
            dialer::connect_with_config(&self.proxy_sock, host_only, port, self.config.clone())
                .map_err(io_err)?;
        let conn = Conn::new(stream);
        let hdrs: Vec<String> = headers.into_iter().map(|h| h.as_ref().to_string()).collect();
        let shared = Arc::new(Mutex::new(conn));
        Ok(PostResponse {
            post_body: PostBody {
                conn: shared.clone(),
                host: host.clone(),
                path: path.clone(),
                headers: hdrs,
                buf: Vec::new(),
                sent: false,
            },
            headers: HeadersReader {
                conn: shared.clone(),
                buf: Vec::new(),
                pos: 0,
                loaded: false,
                err_served: false,
            },
            body: BodyReader {
                conn: shared,
                buf: Vec::new(),
                pos: 0,
                eof: false,
            },
        })
    }

    fn configure(
        &mut self,
        _config: &dyn std::any::Any,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(())
    }
}

fn port_from_auth(auth: &str) -> u16 {
    match auth.rsplit_once(':') {
        Some((_, p)) => p.parse().unwrap_or(443),
        None => 443,
    }
}

fn io_err(e: io::Error) -> Error {
    Error::Detail {
        description: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    /// 极简 HTTP/1.1 over TLS 测试服务器:按 `responder` 生成响应字节。
    fn spawn_tls_http<F>(responder: F) -> u16
    where
        F: Fn(&str, &str, &[u8]) -> Vec<u8> + Send + Sync + 'static,
    {
        let certified_key =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_pem = certified_key.cert.pem();
        let key_pem = certified_key.signing_key.serialize_pem();
        let chain = dialer::pem_chain(&cert_pem);
        let key = dialer::pem_key(&key_pem);
        let server_cfg = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap(),
        );
        let responder = Arc::new(responder);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(tcp) = stream else { break };
                let cfg = server_cfg.clone();
                let responder = responder.clone();
                thread::spawn(move || {
                    let conn = match rustls::ServerConnection::new(cfg) {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut tls = rustls::StreamOwned::new(conn, tcp);
                    // 读取整个请求(到头结束;POST 时读到 body 收齐)
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        match tls.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                req.extend_from_slice(&buf[..n]);
                                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                    if let Some(cl) = content_length(&req) {
                                        let body_start =
                                            req.windows(4).position(|w| w == b"\r\n\r\n").unwrap()
                                                + 4;
                                        if req.len() - body_start >= cl {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let (path, method) = parse_req(&req);
                    let out = responder(method.as_str(), path.as_str(), &req);
                    let _ = tls.write_all(&out);
                    let _ = tls.flush();
                    let _ = tls.get_ref().shutdown(std::net::Shutdown::Write);
                });
            }
        });
        port
    }

    fn content_length(req: &[u8]) -> Option<usize> {
        let s = String::from_utf8_lossy(req).to_ascii_lowercase();
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("content-length:") {
                return v.trim().parse().ok();
            }
        }
        None
    }

    fn parse_req(req: &[u8]) -> (String, String) {
        let s = String::from_utf8_lossy(req);
        let first = s.lines().next().unwrap_or("");
        let mut it = first.split_whitespace();
        let method = it.next().unwrap_or("").to_string();
        let path = it.next().unwrap_or("/").to_string();
        (path, method)
    }

    /// 启本地 SOCKS5h-over-UDS 代理(复用 dialer 测试中的逻辑:转发到 loopback TCP)。
    fn spawn_proxy(sock_path: PathBuf) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let listener = match std::os::unix::net::UnixListener::bind(&sock_path) {
                Ok(l) => l,
                Err(_) => return,
            };
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                thread::spawn(move || {
                    use std::io::{Read, Write};
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
                            (
                                String::from_utf8_lossy(&hb).into_owned(),
                                u16::from_be_bytes(pb),
                            )
                        }
                        _ => return,
                    };
                    let _ = host; // 不做白名单,转发
                    let up = match std::net::TcpStream::connect(("127.0.0.1", port)) {
                        Ok(u) => u,
                        Err(_) => {
                            let _ = s.write_all(&[0x05, 0x05, 0, 1, 0, 0, 0, 0, 0, 0]);
                            return;
                        }
                    };
                    let _ = s.write_all(&[0x05, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
                    let s2 = s.try_clone().unwrap();
                    let up2 = up.try_clone().unwrap();
                    let t = thread::spawn(move || {
                        let mut s = s;
                        let mut up = up;
                        copy(&mut s, &mut up);
                        let _ = up.shutdown(std::net::Shutdown::Write);
                    });
                    let mut up2 = up2;
                    let mut s2 = s2;
                    copy(&mut up2, &mut s2);
                    let _ = s2.shutdown(std::net::Shutdown::Write);
                    let _ = t.join();
                });
            }
        })
    }

    fn copy<R: Read, W: Write>(r: &mut R, w: &mut W) {
        let mut buf = [0u8; 4096];
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

    fn http_200(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes()
    }

    fn http_status(code: u16, msg: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {code} {msg}\r\nContent-Length: 0\r\n\r\n"
        )
        .into_bytes()
    }

    fn wait_sock(p: &std::path::Path) {
        for _ in 0..100 {
            if p.exists() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn get_returns_body() {
        let port = spawn_tls_http(|_m, _p, _req| http_200("hello-body"));
        thread::sleep(std::time::Duration::from_millis(50));
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join(".proxy.sock");
        let _proxy = spawn_proxy(sock.clone());
        wait_sock(&sock);

        let mut http = FixusHttp::with_insecure(sock);
        let url = format!("https://localhost:{port}/info/refs");
        let mut resp = http
            .get(&url, "", std::iter::empty::<&str>())
            .expect("get ok");
        // 消费 headers(gix 契约)
        drop(resp.headers);
        let mut body = String::new();
        resp.body.read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello-body");
    }

    #[test]
    fn get_401_maps_to_permission_denied() {
        let port = spawn_tls_http(|_m, _p, _r| http_status(401, "Unauthorized"));
        thread::sleep(std::time::Duration::from_millis(50));
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join(".proxy.sock");
        let _proxy = spawn_proxy(sock.clone());
        wait_sock(&sock);

        let mut http = FixusHttp::with_insecure(sock);
        let url = format!("https://localhost:{port}/secret");
        let mut resp = http
            .get(&url, "", std::iter::empty::<&str>())
            .expect("get returns response");
        // 错误经 body 首次读返回
        drop(resp.headers);
        let mut sink = String::new();
        let err = resp.body.read_to_string(&mut sink).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn get_500_maps_to_other() {
        let port = spawn_tls_http(|_m, _p, _r| http_status(500, "Internal"));
        thread::sleep(std::time::Duration::from_millis(50));
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join(".proxy.sock");
        let _proxy = spawn_proxy(sock.clone());
        wait_sock(&sock);

        let mut http = FixusHttp::with_insecure(sock);
        let url = format!("https://localhost:{port}/oops");
        let mut resp = http
            .get(&url, "", std::iter::empty::<&str>())
            .expect("get returns response");
        drop(resp.headers);
        let err = resp.body.fill_buf().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn post_writes_body_and_reads_response() {
        let port = spawn_tls_http(|_m, _p, req| {
            // 回显收到的 POST body,便于断言写入
            let body = body_of(req);
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\nPOSTED:{body}",
                format!("POSTED:{body}").len()
            )
            .into_bytes()
        });
        thread::sleep(std::time::Duration::from_millis(50));
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join(".proxy.sock");
        let _proxy = spawn_proxy(sock.clone());
        wait_sock(&sock);

        let mut http = FixusHttp::with_insecure(sock);
        let url = format!("https://localhost:{port}/git-upload-pack");
        let mut resp = http
            .post(&url, "", std::iter::empty::<&str>(), PostBodyDataKind::BoundedAndFitsIntoMemory)
            .expect("post ok");
        // 写请求体
        {
            let mut pb = resp.post_body;
            pb.write_all(b"want-payload").unwrap();
            // drop 显式发送
        }
        drop(resp.headers);
        let mut got = String::new();
        resp.body.read_to_string(&mut got).unwrap();
        assert_eq!(got, "POSTED:want-payload");
    }

    fn body_of(req: &[u8]) -> String {
        let pos = req.windows(4).position(|w| w == b"\r\n\r\n");
        match pos {
            Some(i) => String::from_utf8_lossy(&req[i + 4..]).into_owned(),
            None => String::new(),
        }
    }
}

