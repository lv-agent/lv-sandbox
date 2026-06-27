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

// ==================== cr-027: 快照(copy_dir_recursive + WorkspaceManager ops) ====================

#[test]
fn copy_dir_recursive_copies_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    sandbox_core::workspace::put_file(&src, "a.txt", b"A").unwrap();
    sandbox_core::workspace::put_file(&src, "sub/b.txt", b"B").unwrap();
    let dst = tmp.path().join("dst");
    sandbox_core::workspace::copy_dir_recursive(&src, &dst).unwrap();
    assert_eq!(sandbox_core::workspace::get_file(&dst, "a.txt").unwrap(), b"A");
    assert_eq!(sandbox_core::workspace::get_file(&dst, "sub/b.txt").unwrap(), b"B");
}

#[test]
fn snapshot_create_restore_list_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let m = WorkspaceManager::new(tmp.path(), 0);
    let ws = m.create_session_workspace("s1").unwrap();
    sandbox_core::workspace::put_file(&ws.workspace, "data.txt", b"hello").unwrap();

    m.create_snapshot(&ws.workspace, "snap1").unwrap();
    assert!(m.list_snapshots().unwrap().contains(&"snap1".to_string()));

    let ws2 = m.create_session_workspace("s2").unwrap();
    m.restore_snapshot("snap1", &ws2.workspace).unwrap();
    assert_eq!(
        sandbox_core::workspace::get_file(&ws2.workspace, "data.txt").unwrap(),
        b"hello"
    );

    m.cleanup_snapshot("snap1").unwrap();
    assert!(!m.list_snapshots().unwrap().contains(&"snap1".to_string()));
}

// ==================== cr-028: 卷 ====================

#[test]
fn volume_create_list_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let m = WorkspaceManager::new(tmp.path(), 0);
    m.create_volume("data").unwrap();
    assert!(m.list_volumes().unwrap().contains(&"data".to_string()));
    let p = m.volume_path("data");
    assert!(p.starts_with(tmp.path().join("volumes")));
    m.cleanup_volume("data").unwrap();
    assert!(!m.list_volumes().unwrap().contains(&"data".to_string()));
}

// ==================== cr-037: io.max 默认值 ====================

#[test]
fn builtin_profiles_have_io_limits() {
    use sandbox_core::profile::SandboxProfile;
    let shell = SandboxProfile::shell();
    let cg = shell.cgroup_resources.as_ref().expect("shell has cgroup");
    let io = cg.io_max.as_ref().expect("shell has io_max");
    assert!(io.read_bps.unwrap_or(0) > 0, "read_bps should be set");
    assert!(io.write_bps.unwrap_or(0) > 0, "write_bps should be set");
    // major=0 minor=0 = sentinel(auto-detect at runtime)
    assert_eq!(io.major, 0);
    assert_eq!(io.minor, 0);
    // python/node also
    for p in [SandboxProfile::python(), SandboxProfile::node()] {
        let io2 = p.cgroup_resources.as_ref().unwrap().io_max.as_ref().unwrap();
        assert!(io2.write_bps.unwrap_or(0) > 0);
    }
}
