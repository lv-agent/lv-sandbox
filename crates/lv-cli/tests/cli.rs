//! Integration tests for the `lvs` CLI binary (need a running server).
//!
//! Skips silently if the server is not reachable.
use std::process::{Command, Stdio};

const URL: &str = "http://127.0.0.1:8080";

fn server_up() -> bool {
    Command::new(env!("CARGO_BIN_EXE_lvs"))
        .args(["--url", URL, "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run `lvs` with args; return (success, stdout).
fn run(args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lvs"))
        .args(["--url", URL])
        .args(args)
        .output()
        .expect("lvs binary");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn cli_smoke_jobs_and_sessions() {
    if !server_up() {
        eprintln!("skip: lv-sandbox server not running at {URL}");
        return;
    }
    // one-shot job
    let (ok, out) = run(&["jobs", "run", "--", "/bin/echo", "cli-smoke"]);
    assert!(ok, "jobs run failed");
    assert!(out.contains("cli-smoke"), "stdout: {out}");

    // session roundtrip
    let (ok, sid) = run(&["sessions", "new"]);
    assert!(ok, "sessions new failed");
    let sid = sid.trim().to_string();
    assert!(!sid.is_empty(), "no session id");
    let (ok, out) = run(&["exec", &sid, "--", "/bin/echo", "exec-smoke"]);
    assert!(ok, "exec failed");
    assert!(out.contains("exec-smoke"), "exec stdout: {out}");
    let (ok, _) = run(&["sessions", "rm", &sid]);
    assert!(ok, "sessions rm failed");
}
