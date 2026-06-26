//! cr-026 SessionManager 集成测试。
use sandbox_core::job::{JobRequest, JobStatus};
use sandbox_core::profile::SandboxProfile;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use sandbox_server::audit::AuditLogger;
use sandbox_server::session::{SessionManager, VolumeMount};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn mgr() -> (tempfile::TempDir, SessionManager) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    (
        tmp,
        SessionManager::new(runner, Arc::new(AuditLogger::noop())),
    )
}

fn req(argv: Vec<String>) -> JobRequest {
    JobRequest {
        job_id: "s".to_string(),
        argv,
        profile_name: "shell".to_string(), // 会话 exec 用绑定 profile,此项忽略
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    }
}

#[tokio::test]
async fn session_exec_shares_workspace_across_calls() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // exec A 写文件
    let r1 = m
        .exec_session(
            &id,
            req(vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo hello > out.txt".into(),
            ]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(r1.status, JobStatus::Completed));
    // exec B 读同一文件(证明工作区跨 exec 持久)
    let r2 = m
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "out.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r2.stdout).contains("hello"),
        "shared workspace should retain out.txt: {:?}",
        r2.stdout
    );
}

#[tokio::test]
async fn session_lifecycle_create_list_get_destroy() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    assert!(m.get_session(&id).is_some());
    assert!(m.list_sessions().iter().any(|s| s.session_id == id));
    m.destroy_session(&id).unwrap();
    assert!(m.get_session(&id).is_none());
    assert!(!m.list_sessions().iter().any(|s| s.session_id == id));
}

#[tokio::test]
async fn create_session_unknown_profile_errors() {
    let (_tmp, m) = mgr().await;
    assert!(m.create_session("nope", HashMap::new(), None, vec![]).is_err());
}

#[tokio::test]
async fn snapshot_then_restore_forks_session() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // exec 写文件
    m.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo forked > f.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    // 快照
    let snap = m.snapshot_session(&id).await.unwrap();
    // 从快照建新会话(fork)
    let id2 = m
        .create_session("shell", HashMap::new(), Some(snap.clone()), vec![])
        .unwrap();
    let r = m
        .exec_session(
            &id2,
            req(vec!["/bin/cat".into(), "f.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("forked"),
        "forked session should see snapshot file: {:?}",
        r.stdout
    );
    // list / destroy 快照
    assert!(m.list_snapshots().unwrap().contains(&snap));
    m.destroy_snapshot(&snap).unwrap();
    assert!(!m.list_snapshots().unwrap().contains(&snap));
}

#[tokio::test]
async fn volume_persists_across_sessions() {
    let (_tmp, m) = mgr().await;
    let vol = VolumeMount {
        name: "shared".into(),
        mount: "volumes/shared".into(),
    };
    // 会话 A 挂卷 + 写
    let a = m
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    m.exec_session(
        &a,
        req(vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo persist > volumes/shared/x.txt".into(),
        ]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    m.destroy_session(&a).unwrap();
    // 会话 B 挂同卷 + 读(跨会话持久)
    let b = m
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    let r = m
        .exec_session(
            &b,
            req(vec!["/bin/cat".into(), "volumes/shared/x.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("persist"),
        "volume should persist across sessions: {:?}",
        r.stdout
    );
    m.cleanup_volume("shared").unwrap();
}

#[tokio::test]
async fn session_survives_restart_via_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    // SM1: 建会话 + 写文件
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    sm1.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo survived > keep.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    drop(sm1);
    // SM2: 新 manager(同 base_dir)= "重启"
    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    let n = sm2.rebuild_from_disk().unwrap();
    assert_eq!(n, 1, "one session should be rebuilt");
    assert!(sm2.get_session(&id).is_some(), "session should be reconnectable");
    // exec 读到重启前写入的文件
    let r = sm2
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "keep.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("survived"),
        "rebuilt session should see pre-restart file: {:?}",
        r.stdout
    );
}

/// cr-029 bug 修复:带卷会话重启后仍可写卷(landlock 重新授权)。
#[tokio::test]
async fn volume_survives_restart_with_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let vol = VolumeMount {
        name: "persist".into(),
        mount: "volumes/persist".into(),
    };
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    sm1.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo old > volumes/persist/v.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    drop(sm1);

    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    sm2.rebuild_from_disk().unwrap();
    // 重启后仍可写卷
    // 重启后仍可写卷(builtin echo,无 fork);经 file API 读取(无 fork,避开 nproc)
    sm2.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo new >> volumes/persist/v.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    let data = sm2.get_file(&id, "volumes/persist/v.txt").unwrap();
    let out = String::from_utf8_lossy(&data);
    assert!(
        out.contains("old") && out.contains("new"),
        "volume should persist + be writable post-restart: {out}"
    );
}

/// 会话内 exec 串行(exec_lock 互斥):A1 与 A2 必相邻(A 原子执行,B 不插中间)。
#[tokio::test]
async fn session_exec_is_serialized() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let a = m.exec_session(
        &id,
        req(vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo A1 >> out.txt; i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done; echo A2 >> out.txt".into(),
        ]),
        CancellationToken::new(),
        None,
    );
    let b = m.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo B >> out.txt".into()]),
        CancellationToken::new(),
        None,
    );
    let (ra, rb) = tokio::join!(a, b);
    assert!(
        ra.as_ref().is_ok_and(|r| matches!(r.status, JobStatus::Completed)),
        "concurrent exec A should complete: {ra:?}"
    );
    assert!(
        rb.as_ref().is_ok_and(|r| matches!(r.status, JobStatus::Completed)),
        "concurrent exec B should complete: {rb:?}"
    );
    let r = m
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "out.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    let out = String::from_utf8_lossy(&r.stdout).into_owned();
    let lines: Vec<&str> = out.lines().collect();
    let a1 = lines.iter().position(|l| *l == "A1").expect("A1 present");
    assert_eq!(
        lines.get(a1 + 1),
        Some(&"A2"),
        "exec not serialized (B interleaved between A1/A2): {:?}",
        lines
    );
}

/// rebuild 跳过 profile 已不存在的会话。
#[tokio::test]
async fn rebuild_skips_session_with_missing_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    // 预置一个 meta 指向不存在的 profile
    let dir = tmp.path().join("sessions").join("ghost");
    std::fs::create_dir_all(dir.join("workspace")).unwrap();
    std::fs::write(
        dir.join(".session-meta.json"),
        r#"{"profile_name":"ghost","env":{},"volumes":[]}"#,
    )
    .unwrap();
    let runner = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let m = SessionManager::new(runner, Arc::new(AuditLogger::noop()));
    let n = m.rebuild_from_disk().unwrap();
    assert_eq!(n, 0, "session with missing profile should be skipped");
    assert!(m.get_session("ghost").is_none());
}

/// 会话 exec 可被 cancel。
#[tokio::test]
async fn session_exec_cancel() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });
    let r = m
        .exec_session(
            &id,
            req(vec!["/bin/sleep".into(), "10".into()]),
            cancel,
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(r.status, JobStatus::Cancelled),
        "expected Cancelled, got {:?}",
        r.status
    );
}

/// disk_quota 在会话 exec 里生效(看门狗测 workspace.root)。
#[tokio::test]
async fn session_disk_quota_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let mut runner = SandboxRunner::new(&cfg).await.unwrap();
    let mut rlimit = SandboxProfile::shell().rlimit;
    rlimit.nproc = None;
    rlimit.fsize_bytes = Some(1024 * 1024 * 1024);
    runner.register_profile(SandboxProfile {
        name: "quota".to_string(),
        disk_quota_mb: Some(1),
        rlimit,
        ..SandboxProfile::shell()
    });
    let m = SessionManager::new(Arc::new(runner), Arc::new(AuditLogger::noop()));
    let id = m.create_session("quota", HashMap::new(), None, vec![]).unwrap();
    let r = m
        .exec_session(
            &id,
            req(vec![
                "/bin/sh".into(),
                "-c".into(),
                "yes | head -c 200000000 > big; sleep 5".into(),
            ]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(r.status, JobStatus::DiskQuotaExceeded),
        "session disk quota should be enforced, got {:?}",
        r.status
    );
}
