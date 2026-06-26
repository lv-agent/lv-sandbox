//! cr-026 会话工作区 + 文件 I/O 单测。
use sandbox_core::workspace::{sanitize_relpath, WorkspaceManager};
use std::path::Path;

fn mgr(tmp: &Path) -> WorkspaceManager {
    WorkspaceManager::new(tmp, 0)
}

#[test]
fn create_and_cleanup_session_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(tmp.path());
    let ws = m.create_session_workspace("s1").unwrap();
    assert!(ws.workspace.is_dir());
    assert!(ws.tmp.is_dir());
    assert!(ws.root.starts_with(tmp.path().join("sessions")));
    m.cleanup_session("s1").unwrap();
    assert!(!ws.root.exists());
}

#[test]
fn put_get_list_delete_file_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(tmp.path());
    let ws = m.create_session_workspace("s2").unwrap();
    let base = &ws.workspace;

    sandbox_core::workspace::put_file(base, "hello.txt", b"hi there").unwrap();
    let got = sandbox_core::workspace::get_file(base, "hello.txt").unwrap();
    assert_eq!(got, b"hi there");

    let listed = sandbox_core::workspace::list_files(base, "").unwrap();
    assert!(listed.iter().any(|e| e.name == "hello.txt" && e.size == 8 && !e.is_dir));

    sandbox_core::workspace::delete_file(base, "hello.txt").unwrap();
    assert!(sandbox_core::workspace::get_file(base, "hello.txt").is_err());
}

#[test]
fn put_file_creates_subdirs() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(tmp.path());
    let ws = m.create_session_workspace("s3").unwrap();
    sandbox_core::workspace::put_file(&ws.workspace, "sub/deep/x.txt", b"x").unwrap();
    assert_eq!(sandbox_core::workspace::get_file(&ws.workspace, "sub/deep/x.txt").unwrap(), b"x");
}

#[test]
fn sanitize_rejects_traversal_and_absolute() {
    let base = Path::new("/tmp/ws");
    assert!(sanitize_relpath(base, "../etc/passwd").is_err());
    assert!(sanitize_relpath(base, "a/../b").is_err());
    assert!(sanitize_relpath(base, "/etc/passwd").is_err());
    assert!(sanitize_relpath(base, "..").is_err());
    assert!(sanitize_relpath(base, "").is_err());
    // 合法
    assert!(sanitize_relpath(base, "a/b.txt").is_ok());
    assert!(sanitize_relpath(base, "x").is_ok());
}

#[test]
fn get_file_missing_is_err() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(tmp.path());
    let ws = m.create_session_workspace("s4").unwrap();
    assert!(sandbox_core::workspace::get_file(&ws.workspace, "nope.txt").is_err());
}
