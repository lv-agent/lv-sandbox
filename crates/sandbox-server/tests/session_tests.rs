//! cr-026 SessionManager 集成测试。
use sandbox_core::job::{JobRequest, JobStatus};
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
