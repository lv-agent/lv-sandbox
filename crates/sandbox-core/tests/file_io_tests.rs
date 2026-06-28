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

// ==================== cr-037 gap: io.max 设备探测 ====================

#[test]
fn io_max_default_values_are_generous() {
    use sandbox_core::profile::SandboxProfile;
    let p = SandboxProfile::shell();
    let io = p.cgroup_resources.as_ref().unwrap().io_max.as_ref().unwrap();
    // 宽松默认(防失控,不限正常)
    assert!(io.read_bps.unwrap() >= 100 * 1024 * 1024, "read >= 100 MB/s");
    assert!(io.write_bps.unwrap() >= 50 * 1024 * 1024, "write >= 50 MB/s");
    // sentinel for auto-detect
    assert_eq!((io.major, io.minor), (0, 0));
}

#[test]
fn workspace_dev_detection_produces_nonzero() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let dev = tmp.path().metadata().unwrap().dev();
    let major = (dev >> 8) as u64;
    let minor = (dev & 0xff) as u64;
    // /tmp 所在设备应有非零 major
    assert!(major > 0 || minor > 0, "dev detection: major={major} minor={minor}");
}

// ==================== cr-038: 资源使用报告 ====================

#[tokio::test]
async fn job_result_has_resource_usage_when_cgroup_available() {
    use sandbox_core::job::{JobRequest, JobStatus};
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
    use std::collections::HashMap;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&cfg).await.unwrap();

    let req = JobRequest {
        job_id: "res-001".to_string(),
        argv: vec!["/bin/echo".into(), "resource-test".into()],
        profile_name: "shell".to_string(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    };
    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert!(matches!(result.status, JobStatus::Completed));
    // cgroup 可用时应有 resource_usage;不可用时(如无 cgroup v2)为 None——两者都合法
    if let Some(usage) = &result.resource_usage {
        // memory_peak 应非零(echo 至少分配了内存)
        assert!(usage.memory_peak_bytes.unwrap_or(0) > 0, "memory_peak should be non-zero: {:?}", usage);
    }
}

// ==================== cr-041: 违规检测(Seccomp SIGSYS / OOM) ====================

/// 被 seccomp kill 的进程应报告 SeccompDenied 违规。
/// `unshare` 调用 unshare(2) syscall，该 syscall 在 default_denylist 中。
#[tokio::test]
async fn job_result_has_seccomp_denied_when_killed_by_sigsys() {
    use sandbox_core::job::{JobRequest, JobStatus, SandboxViolation};
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
    use std::collections::HashMap;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&cfg).await.unwrap();

    // unshare(2) 在 seccomp default_denylist → SIGSYS=31
    let req = JobRequest {
        job_id: "sec-001".to_string(),
        argv: vec!["/usr/bin/unshare".into(), "-r".into(), "/bin/true".into()],
        profile_name: "shell".to_string(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    };
    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert!(
        matches!(result.status, JobStatus::Killed),
        "unshare should be killed by seccomp, got {:?}",
        result.status
    );
    assert_eq!(result.signal, Some(31), "signal should be SIGSYS(31)");
    assert!(
        result
            .sandbox_violations
            .iter()
            .any(|v| matches!(v, SandboxViolation::SeccompDenied { .. })),
        "should contain SeccompDenied: {:?}",
        result.sandbox_violations
    );
}

/// cgroup OOM 应报告 OomKill 违规。
/// 仅 cgroup v2 可用时生效;否则跳过。
#[tokio::test]
async fn job_result_has_oom_kill_when_cgroup_oom() {
    use sandbox_core::job::{JobRequest, JobStatus, SandboxViolation};
    use sandbox_core::profile::SandboxProfile;
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
    use sandbox_cgroup::CgroupResources;
    use std::collections::HashMap;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let mut runner = SandboxRunner::new(&cfg).await.unwrap();

    // 注册低内存 profile(cgroup v2 可用且能迁移进程时测试;否则 skip)
    let cap = runner.capability();
    if !cap.cgroup.can_migrate_processes {
        eprintln!("skipping: cgroup v2 not available");
        return;
    }

    let cgroup = SandboxProfile::shell().cgroup_resources.unwrap();
    runner.register_profile(SandboxProfile {
        name: "oom-test".to_string(),
        cgroup_resources: Some(CgroupResources {
            memory_max: Some(5 * 1024 * 1024), // 5MB
            pids_max: Some(32),
            ..cgroup
        }),
        ..SandboxProfile::shell()
    });

    // 串联 10MB 字符串于 shell 变量(cgroup OOM 在 5MB 上限触发)
    let req = JobRequest {
        job_id: "oom-001".to_string(),
        argv: vec![
            "/bin/bash".into(),
            "-c".into(),
            "v=; for ((i=0;i<10000000;i++)); do v+=x; done; echo ok".into(),
        ],
        profile_name: "oom-test".to_string(),
        timeout: Some(Duration::from_secs(10)),
        custom_env: HashMap::new(),
        stdin_data: None,
    };
    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert!(
        matches!(result.status, JobStatus::Killed),
        "OOM should kill the process, got {:?}",
        result.status
    );
    assert!(
        result
            .sandbox_violations
            .iter()
            .any(|v| matches!(v, SandboxViolation::OomKill)),
        "should contain OomKill: {:?}",
        result.sandbox_violations
    );
}
