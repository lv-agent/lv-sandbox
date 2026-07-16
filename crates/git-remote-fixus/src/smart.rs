//! cr-12 G1 步骤3.4: git smart-HTTP 协议 —— 在已测的 [`crate::http::FixusHttp`](Http) impl
//! 之上最小手写 pkt-line(采 Task 0 既定的回退路径)。
//!
//! 关键事实(源 `git/transport-helper.c`):
//! - `fetch` 能力下,git 只逐行读 helper stdout 找 `lock <file>`/空行,**不在 stdout 收 pack**;
//!   helper 必须自己把对象写入本地 object DB(此处 spawn `git index-pack --stdin --fix-thin`,
//!   GIT_DIR 由 git 经环境注入)。故 fetch 端 = 上行 upload-pack 协商 + side-band 解复用 → 喂 index-pack。
//! - `push` 能力下,git 发 `push [+]<src>:<dst>` 批 + 空行;helper 跑 receive-pack,按 ref 回
//!   `ok <dst>` / `error <dst> <why>` + 空行。push 端 = `git pack-objects --revs --stdout` 造包 +
//!   POST git-receive-pack(ref 更新 + 包)→ 解析 report-status。
//!
//! 协议版本:HTTP smart v0/v1(不发 Git-Protocol 头 → 服务端默认 v0/v1)。
//! G1 已知缺口:不支持 protocol v2(需 ls-refs + command=fetch);真实 github 默认 v2,
//! 但本 helper 的集成对象是本地/受控上游(见 3.5)。v2 留作后续。

use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

use gix_transport::client::blocking_io::http::{Http, PostBodyDataKind};

use crate::http::FixusHttp;

type HttpErr = gix_transport::client::blocking_io::http::Error;

const FLUSH: &[u8] = b"0000";

/// pkt-line 编码:`<4-hex-len><data>`(len 含 4 字节长度自身)。
///
/// 协议总长上限 65520(0xfff0);65521..65535 为保留值。超过上限返回 Err
/// (非 panic):所有调用方都在返回 `io::Result` 的函数内,用 `?` 传播。
fn pkt(data: &[u8]) -> io::Result<Vec<u8>> {
    const PKT_MAX_TOTAL: usize = 65520;
    let total = 4 + data.len();
    if total > PKT_MAX_TOTAL {
        return Err(io::Error::other(format!(
            "pkt-line too large: {total} bytes (max {PKT_MAX_TOTAL})"
        )));
    }
    let mut out = format!("{total:04x}").into_bytes();
    out.extend_from_slice(data);
    Ok(out)
}

/// pkt-line 读取:从 BufRead 读一条。返回 None=流结束。
enum Pkt {
    Data(Vec<u8>),
    Flush,
    #[allow(dead_code)]
    Delim,
}

struct PktReader<R: BufRead> {
    r: R,
}

impl<R: BufRead> PktReader<R> {
    fn new(r: R) -> Self {
        PktReader { r }
    }

    fn read_one(&mut self) -> io::Result<Option<Pkt>> {
        let mut len_hex = [0u8; 4];
        // 先试探 1 字节以区分 EOF
        let mut peek = [0u8; 1];
        if self.r.read(&mut peek)? == 0 {
            return Ok(None);
        }
        len_hex[0] = peek[0];
        self.r.read_exact(&mut len_hex[1..4])?;
        let len = match u16::from_str_radix(
            std::str::from_utf8(&len_hex).map_err(|e| io::Error::other(e.to_string()))?,
            16,
        ) {
            Ok(n) => n,
            Err(_) => return Err(io::Error::other("bad pkt-line length")),
        };
        match len {
            0 => Ok(Some(Pkt::Flush)),
            1 => Ok(Some(Pkt::Delim)),
            n if n < 4 => Err(io::Error::other("pkt-line length < 4")),
            n if n > 65520 => Err(io::Error::other(format!(
                "pkt-line length {n} exceeds max 65520 (65521..65535 reserved)"
            ))),
            n => {
                let mut buf = vec![0u8; (n - 4) as usize];
                self.r.read_exact(&mut buf)?;
                Ok(Some(Pkt::Data(buf)))
            }
        }
    }
}

/// 远端引用。
pub struct AdvertisedRef {
    pub name: String,
    pub sha: String,
}

/// 解析广告中的 `symref=HEAD:<target>`(git-http-backend 现代版本会发)。
fn symref_target<'a>(caps: &'a str, head: &str) -> Option<&'a str> {
    let needle = format!("symref={head}:");
    for tok in caps.split_whitespace() {
        if let Some(rest) = tok.strip_prefix(needle.as_str()) {
            return Some(rest);
        }
    }
    None
}

/// `GET <base>/info/refs?service=<svc>`,解析 ref advertisement。
/// 返回 (refs, head_sha, head_target)。head_target 非 None 时 HEAD 是指向它的 symref。
pub fn list_refs(
    http: &mut FixusHttp,
    base_url: &str,
    for_push: bool,
) -> io::Result<(Vec<AdvertisedRef>, Option<String>, Option<String>)> {
    let svc = if for_push {
        "git-receive-pack"
    } else {
        "git-upload-pack"
    };
    let url = if base_url.contains('?') {
        format!("{base_url}&service={svc}")
    } else {
        format!("{base_url}/info/refs?service={svc}")
    };
    let resp = http
        .get(&url, base_url, std::iter::empty::<&str>())
        .map_err(http_err)?;
    drop(resp.headers);

    let mut pr = PktReader::new(resp.body);
    // 首条:"# service=<svc>\n",随后 flush
    let mut head_sha: Option<String> = None;
    let mut head_target: Option<String> = None;
    let mut refs: Vec<AdvertisedRef> = Vec::new();
    let mut first_ref = true;

    while let Some(p) = pr.read_one()? {
        match p {
            Pkt::Flush | Pkt::Delim => continue,
            Pkt::Data(d) => {
                let line = String::from_utf8_lossy(&d);
                let line = line.strip_suffix('\n').unwrap_or(&line);
                if line.starts_with("# service=") {
                    continue;
                }
                // 首条 ref 携带 caps(在 NUL 之后)
                let (main, caps) = match line.split_once('\0') {
                    Some((m, c)) => (m, c),
                    None => (line, ""),
                };
                let mut parts = main.splitn(2, ' ');
                let sha = parts.next().unwrap_or("").to_string();
                let name = parts.next().unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                if first_ref {
                    first_ref = false;
                    if let Some(t) = symref_target(caps, "HEAD") {
                        head_target = Some(t.to_string());
                    }
                }
                if name == "HEAD" {
                    head_sha = Some(sha);
                } else {
                    refs.push(AdvertisedRef { name, sha });
                }
            }
        }
    }
    Ok((refs, head_sha, head_target))
}

/// fetch:POST git-upload-pack(want+done),side-band 解复用,管道喂 `git index-pack --stdin --fix-thin`。
/// wants 为要去取的对象 sha 列表。成功后对象已写入本地 object DB。
pub fn fetch_pack(http: &mut FixusHttp, base_url: &str, wants: &[String]) -> io::Result<()> {
    let url = format!("{base_url}/git-upload-pack");
    let mut resp = http
        .post(
            &url,
            base_url,
            [
                "Content-Type: application/x-git-upload-pack-request",
                "Accept: application/x-git-upload-pack-result",
            ],
            PostBodyDataKind::BoundedAndFitsIntoMemory,
        )
        .map_err(http_err)?;

    // 构造请求体
    let mut body = Vec::new();
    for (i, w) in wants.iter().enumerate() {
        let line = if i == 0 {
            format!("want {w} ofs-delta side-band-64k agent=git-remote-fixus/0.1\n")
        } else {
            format!("want {w}\n")
        };
        body.extend_from_slice(&pkt(line.as_bytes())?);
    }
    body.extend_from_slice(FLUSH); // 想要列表结束
    body.extend_from_slice(&pkt(b"done\n")?);
    resp.post_body.write_all(&body)?;
    drop(resp.post_body); // 触发发送
    drop(resp.headers);

    // 起一个 index-pack 子进程,把 side-band 解出的 band1(pack)喂给它
    let mut index_pack = Command::new("git")
        .args(["index-pack", "--stdin", "--fix-thin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut stdin = index_pack.stdin.take().expect("piped stdin");

    let mut pr = PktReader::new(resp.body);
    let mut pack_started = false;
    let mut err_msg: Option<String> = None;
    while let Some(p) = pr.read_one()? {
        match p {
            Pkt::Flush | Pkt::Delim => break,
            Pkt::Data(d) => {
                if d.is_empty() {
                    continue;
                }
                let band = d[0];
                if (1..=3).contains(&band) {
                    pack_started = true;
                    let payload = &d[1..];
                    match band {
                        1 => {
                            if stdin.write_all(payload).is_err() {
                                break;
                            }
                        }
                        2 => { /* progress,忽略 */ }
                        3 => {
                            err_msg = Some(String::from_utf8_lossy(payload).into_owned());
                            break;
                        }
                        _ => {}
                    }
                } else if !pack_started {
                    // 协商行(NAK / ACK),忽略
                    let s = String::from_utf8_lossy(&d);
                    if let Some(m) = s.strip_prefix("ERR ") {
                        err_msg = Some(m.trim().to_string());
                        break;
                    }
                }
            }
        }
    }
    drop(stdin); // 关闭 index-pack 输入
    let status = index_pack.wait()?;
    if let Some(m) = err_msg {
        return Err(io::Error::other(format!("upload-pack error: {m}")));
    }
    if !status.success() {
        return Err(io::Error::other(format!("index-pack failed: {status}")));
    }
    Ok(())
}

/// push 更新项。src 为本地 ref/sha(空串=删除),dst 为远端 ref。
pub struct PushUpdate {
    pub old: String, // 全 0 = 新建;非 0 = 已有
    pub new: String, // 全 0 = 删除
    pub dst: String,
}

/// push:`git pack-objects --revs --stdout` 造包 → POST git-receive-pack → 解析 report-status。
/// 返回每条更新的 (dst, Result<(), String>)。
pub fn push_refs(
    http: &mut FixusHttp,
    base_url: &str,
    updates: &[PushUpdate],
) -> io::Result<Vec<(String, Result<(), String>)>> {
    // 1) 用 `git pack-objects` 构造要发的包(包含 new 可达、排除 old 可达)。
    let pack = build_push_pack(updates)?;
    let url = format!("{base_url}/git-receive-pack");
    let mut resp = http
        .post(
            &url,
            base_url,
            [
                "Content-Type: application/x-git-receive-pack-request",
                "Accept: application/x-git-receive-pack-result",
            ],
            PostBodyDataKind::BoundedAndFitsIntoMemory,
        )
        .map_err(http_err)?;

    // 2) 请求体:报告状态需求 + 各 ref 更新 + flush + 包
    let mut body = Vec::new();
    const ZERO: &str = "0000000000000000000000000000000000000000";
    for (i, u) in updates.iter().enumerate() {
        let old = if u.old.is_empty() { ZERO } else { u.old.as_str() };
        let new = if u.new.is_empty() { ZERO } else { u.new.as_str() };
        // receive-pack:客户端能力挂在首条 ref 行 NUL 之后(非独立 pkt-line)
        let line = if i == 0 {
            format!("{old} {new} {}\0report-status agent=git-remote-fixus/0.1\n", u.dst)
        } else {
            format!("{old} {new} {}\n", u.dst)
        };
        body.extend_from_slice(&pkt(line.as_bytes())?);
    }
    body.extend_from_slice(FLUSH);
    // 包(可能为空 = 仅删 ref / 已是最新)
    body.extend_from_slice(&pack);
    resp.post_body.write_all(&body)?;
    drop(resp.post_body);
    drop(resp.headers);

    // 3) 解析 report-status(无 side-band):unpack 行 + ok/ng 行 + flush
    let mut pr = PktReader::new(resp.body);
    let mut unpack_ok = false;
    let mut results = Vec::new();
    let mut saw_unpack = false;
    while let Some(p) = pr.read_one()? {
        match p {
            Pkt::Flush | Pkt::Delim => break,
            Pkt::Data(d) => {
                let s = String::from_utf8_lossy(&d);
                let s = s.strip_suffix('\n').unwrap_or(&s);
                if let Some(rest) = s.strip_prefix("unpack ") {
                    saw_unpack = true;
                    unpack_ok = rest.trim() == "ok";
                } else if let Some(rest) = s.strip_prefix("ok ") {
                    results.push((rest.trim().to_string(), Ok(())));
                } else if let Some(rest) = s.strip_prefix("ng ") {
                    let mut it = rest.splitn(2, ' ');
                    let dst = it.next().unwrap_or("").to_string();
                    let why = it.next().unwrap_or("").to_string();
                    results.push((dst, Err(why)));
                }
            }
        }
    }
    if !saw_unpack {
        return Err(io::Error::other("push: no unpack status from server"));
    }
    if !unpack_ok {
        // 整包失败 → 所有 ref 标错
        return Ok(updates
            .iter()
            .map(|u| (u.dst.clone(), Err("unpack failed".to_string())))
            .collect());
    }
    // 服务端未单独报告的 ref 视为 ok
    for u in updates {
        if !results.iter().any(|(d, _)| d == &u.dst) {
            results.push((u.dst.clone(), Ok(())));
        }
    }
    Ok(results)
}

/// `git pack-objects --revs --stdout`:<new> 可达、排除 <old> 可达,产出包字节。
fn build_push_pack(updates: &[PushUpdate]) -> io::Result<Vec<u8>> {
    if updates.is_empty() {
        return Ok(Vec::new());
    }
    // 检查是否全部为删除/空更新(无需发包)
    let any_new = updates.iter().any(|u| !u.new.is_empty());
    if !any_new {
        return Ok(Vec::new());
    }

    let mut revs = String::new();
    for u in updates {
        if !u.new.is_empty() {
            revs.push_str(&u.new);
            revs.push('\n');
        }
        if !u.old.is_empty() {
            revs.push('^');
            revs.push_str(&u.old);
            revs.push('\n');
        }
    }
    let mut child = Command::new("git")
        .args(["pack-objects", "--revs", "--stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().expect("piped");
        stdin.write_all(revs.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "pack-objects failed: {}",
            out.status
        )));
    }
    Ok(out.stdout)
}

fn http_err(e: HttpErr) -> io::Error {
    io::Error::other(format!("http: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_roundtrip() {
        // 4 字节长度含自身
        let enc = pkt(b"done\n").unwrap();
        assert_eq!(&enc, b"0009done\n");
        let enc = pkt(b"want").unwrap();
        assert_eq!(&enc, b"0008want");
    }

    #[test]
    fn pkt_rejects_oversized() {
        // 协议上限:总长 65520(含 4 字节长度前缀);65521..65535 保留。
        // 刚好上限(65516 字节数据)应成功。
        let ok_data = vec![b'x'; 65520 - 4];
        assert!(pkt(&ok_data).is_ok());
        // 超过上限(65517 字节数据 → 总长 65521)应返回 Err,而非 panic。
        let big_data = vec![b'x'; 65520 - 4 + 1];
        assert!(pkt(&big_data).is_err());
    }

    #[test]
    fn pkt_reader_parses_data_and_flush() {
        let raw: Vec<u8> = b"0009done\n0000".to_vec();
        let mut pr = PktReader::new(io::Cursor::new(raw));
        match pr.read_one().unwrap().unwrap() {
            Pkt::Data(d) => assert_eq!(d, b"done\n"),
            _ => panic!("expected data"),
        }
        assert!(matches!(pr.read_one().unwrap(), Some(Pkt::Flush)));
        assert!(pr.read_one().unwrap().is_none());
    }

    #[test]
    fn symref_parse() {
        let caps = "ofs-delta side-band-64k symref=HEAD:refs/heads/main";
        assert_eq!(symref_target(caps, "HEAD"), Some("refs/heads/main"));
        assert_eq!(symref_target(caps, "FOO"), None);
    }
}
