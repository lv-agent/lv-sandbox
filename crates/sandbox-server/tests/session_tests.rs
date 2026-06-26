//! cr-026 SessionManager 集成测试。
use sandbox_core::job::{JobRequest, JobStatus};
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use sandbox_server::audit::AuditLogger;
use sandbox_server::session::SessionManager;
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
    let id = m.create_session("shell", HashMap::new()).unwrap();
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
    let id = m.create_session("shell", HashMap::new()).unwrap();
    assert!(m.get_session(&id).is_some());
    assert!(m.list_sessions().iter().any(|s| s.session_id == id));
    m.destroy_session(&id).unwrap();
    assert!(m.get_session(&id).is_none());
    assert!(!m.list_sessions().iter().any(|s| s.session_id == id));
}

#[tokio::test]
async fn create_session_unknown_profile_errors() {
    let (_tmp, m) = mgr().await;
    assert!(m.create_session("nope", HashMap::new()).is_err());
}
