//! cr-12 G1 步骤3.4/3.5: git remote-helper 主入口。
//!
//! 用法:由 git 以 `git-remote-fixus <remote|url> <url>` 调起。URL 形如
//! `fixus::https://<host>/<repo>.git`(git 自动剥离 `fixus::`,以 `https://...` 作第二参数)。
//! 出口 UDS 代理路径来自环境 `SANDBOX_PROXY_SOCK`(由 jail 注入,见 sandbox_context.rs)。
//!
//! stdin 协议(capabilities / list[/for-push] / fetch / push),响应写 stdout。
//! 详细 git smart-HTTP 逻辑见 `smart.rs`;HTTP+TLS+SOCKS5h 见 `http.rs`/`dialer.rs`。

mod dialer;
mod http;
mod smart;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

use http::FixusHttp;

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

fn main() {
    if let Err(e) = run() {
        // remote-helper 约定:致命错误写 stderr 后退出
        eprintln!("git-remote-fixus: {e}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let url = parse_url(std::env::args().collect())?;
    let base = strip_trailing_slash(&url);
    let proxy_sock: PathBuf = std::env::var_os("SANDBOX_PROXY_SOCK")
        .ok_or_else(|| io::Error::other("SANDBOX_PROXY_SOCK not set"))?
        .into();

    let mut http = FixusHttp::with_default_roots(proxy_sock);
    // 最近一次 list for-push 的远端引用值(dst -> sha),用于 push 时填 old。
    let mut remote_refs: HashMap<String, String> = HashMap::new();

    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    let mut lines = stdin.lock().lines();

    while let Some(line) = lines.next().transpose()? {
        let cmd = line.trim_end_matches('\n');
        match cmd {
            "" => {
                // 空行:命令流结束信号(disconnect)。直接退出。
                let _ = out.flush();
                return Ok(());
            }
            "capabilities" => {
                writeln!(out, "fetch")?;
                writeln!(out, "push")?;
                writeln!(out)?; // 空行结束
                out.flush()?;
            }
            "list" | "list for-push" => {
                let for_push = cmd == "list for-push";
                let (refs, head_sha, head_target) = smart::list_refs(&mut http, &base, for_push)?;
                // HEAD(symref)
                if let Some(sha) = &head_sha {
                    if let Some(t) = &head_target {
                        writeln!(out, "@{t} HEAD")?;
                    } else {
                        writeln!(out, "{sha} HEAD")?;
                    }
                }
                for r in &refs {
                    writeln!(out, "{} {}", r.sha, r.name)?;
                }
                writeln!(out)?; // 空行结束
                out.flush()?;
                if for_push {
                    remote_refs.clear();
                    for r in refs {
                        remote_refs.insert(r.name, r.sha);
                    }
                }
            }
            other if other.starts_with("fetch ") => {
                // 收集整个批次的 wants(直到空行)
                let mut wants: Vec<String> = Vec::new();
                let first = other.trim_start_matches("fetch ");
                if !first.is_empty() {
                    let mut parts = first.split_whitespace();
                    if let Some(sha) = parts.next() {
                        push_unique(&mut wants, sha);
                    }
                }
                while let Some(l) = lines.next().transpose()? {
                    if l.trim().is_empty() {
                        break;
                    }
                    let mut parts = l.split_whitespace();
                    let _kw = parts.next(); // "fetch"
                    if let Some(sha) = parts.next() {
                        push_unique(&mut wants, sha);
                    }
                }
                match smart::fetch_pack(&mut http, &base, &wants) {
                    Ok(()) => {}
                    Err(e) => {
                        // 写 stderr 让 git 报错;仍需空行收尾 stdout
                        eprintln!("git-remote-fixus fetch error: {e}");
                    }
                }
                writeln!(out)?; // fetch 批次完成信号
                out.flush()?;
            }
            other if other.starts_with("push ") => {
                // 收集整个批次的 push(直到空行),可能含尾部 option 行
                let mut specs: Vec<String> = Vec::new();
                let first_spec = other.trim_start_matches("push ");
                if !first_spec.is_empty() {
                    specs.push(first_spec.to_string());
                }
                while let Some(l) = lines.next().transpose()? {
                    let t = l.trim();
                    if t.is_empty() {
                        break;
                    }
                    if t.starts_with("push ") {
                        specs.push(t.trim_start_matches("push ").to_string());
                    }
                    // 其它(如 option 行)忽略
                }
                let updates = build_push_updates(&specs, &remote_refs)?;
                let results = match smart::push_refs(&mut http, &base, &updates) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("git-remote-fixus push error: {e}");
                        updates
                            .iter()
                            .map(|u| (u.dst.clone(), Err("transport error".to_string())))
                            .collect()
                    }
                };
                for (dst, res) in &results {
                    match res {
                        Ok(()) => writeln!(out, "ok {dst}")?,
                        Err(why) => writeln!(out, "error {dst} {why}")?,
                    }
                }
                writeln!(out)?; // 空行结束
                out.flush()?;
            }
            other if other.starts_with("option ") => {
                // 未广告 option 能力,理论上不会收到;宽容处理。
                writeln!(out, "unsupported")?;
                out.flush()?;
            }
            _ => {
                // 未知命令:忽略
            }
        }
    }
    let _ = out.flush();
    Ok(())
}

/// 取 remote-helper 第二参数(URL),否则第一参数;剥离 `fixus::` 前缀。
fn parse_url(args: Vec<String>) -> io::Result<String> {
    let raw = match args.get(2) {
        Some(u) if !u.is_empty() => u.clone(),
        _ => args
            .get(1)
            .cloned()
            .ok_or_else(|| io::Error::other("missing URL argument"))?,
    };
    Ok(raw
        .strip_prefix("fixus::")
        .map(|s| s.to_string())
        .unwrap_or(raw))
}

fn strip_trailing_slash(s: &str) -> String {
    let mut s = s.trim_end_matches('/').to_string();
    if s.is_empty() {
        s.push('/');
    }
    s
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    let s = s.to_string();
    if !v.contains(&s) {
        v.push(s);
    }
}

/// 把 `push [+]<src>:<dst>` 批转为 receive-pack 更新项。
/// old 取自最近 list for-push;new = `git rev-parse <src>`(src 空 = 删除)。
fn build_push_updates(
    specs: &[String],
    remote_refs: &HashMap<String, String>,
) -> io::Result<Vec<smart::PushUpdate>> {
    let mut out = Vec::new();
    for spec in specs {
        let spec = spec.trim_start_matches('+'); // 强制更新标记,不影响 old/new
        let (src, dst) = match spec.split_once(':') {
            Some((s, d)) => (s.trim().to_string(), d.trim().to_string()),
            None => continue,
        };
        let old = remote_refs.get(&dst).cloned().unwrap_or_default();
        let new = if src.is_empty() {
            String::new() // 删除
        } else {
            resolve_ref_or_obj(&src)?
        };
        out.push(smart::PushUpdate { old, new, dst });
    }
    Ok(out)
}

/// 解析本地引用名/sha → 40-hex sha。
fn resolve_ref_or_obj(name: &str) -> io::Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", &format!("{name}^{{commit}}")])
        .output()?;
    if !out.status.success() {
        // 退而求其次:直接 rev-parse(可能指向非 commit,如 tag)
        let out2 = Command::new("git").args(["rev-parse", name]).output()?;
        if !out2.status.success() {
            return Err(io::Error::other(format!(
                "cannot resolve ref {name}: {}",
                String::from_utf8_lossy(&out2.stderr).trim()
            )));
        }
        return Ok(String::from_utf8_lossy(&out2.stdout).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// 避免 unused 警告(ZERO_SHA 在 report-status 里硬编码,此常量留作文档)
#[allow(dead_code)]
const _: &str = ZERO_SHA;
