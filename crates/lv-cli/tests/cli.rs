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

    // files put/get/ls
    let tmp = tempfile::tempdir().expect("tempdir");
    let local = tmp.path().join("upload.txt");
    std::fs::write(&local, b"cli-file-data").expect("write");
    let (ok, _) = run(&["files", "put", &sid, "remote.txt", local.to_str().unwrap()]);
    assert!(ok, "files put failed");
    let (ok, out) = run(&["files", "get", &sid, "remote.txt"]);
    assert!(ok, "files get failed");
    assert_eq!(out, "cli-file-data");
    let (ok, out) = run(&["files", "ls", &sid]);
    assert!(ok, "files ls failed");
    assert!(out.contains("remote.txt"), "files ls should list remote.txt: {out}");

    // snapshots ls/new/rm
    let (ok, snap) = run(&["snapshots", "new", &sid]);
    assert!(ok, "snapshots new failed");
    let snap = snap.trim().to_string();
    assert!(!snap.is_empty());
    let (ok, out) = run(&["snapshots", "ls"]);
    assert!(ok, "snapshots ls failed");
    assert!(out.contains(&snap), "snapshots ls should contain {snap}: {out}");
    let (ok, _) = run(&["snapshots", "rm", &snap]);
    assert!(ok, "snapshots rm failed");

    // volumes new/ls/rm
    let vol_name = "cli-test-vol";
    let (ok, _) = run(&["volumes", "new", vol_name]);
    assert!(ok, "volumes new failed");
    let (ok, out) = run(&["volumes", "ls"]);
    assert!(ok, "volumes ls failed");
    assert!(out.contains(vol_name), "volumes ls should contain {vol_name}: {out}");
    let (ok, _) = run(&["volumes", "rm", vol_name]);
    assert!(ok, "volumes rm failed");

    let (ok, _) = run(&["sessions", "rm", &sid]);
    assert!(ok, "sessions rm failed");
}
