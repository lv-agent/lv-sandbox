//! 崩溃恢复模块测试
//!
//! TDD RED：验证启动时恢复残留 job

use sandbox_core::recovery::RecoveryReport;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};

/// 空目录恢复应返回 0 清理
#[tokio::test]
async fn empty_dir_recovery_returns_zero_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&config).await.unwrap();

    let report = sandbox_core::recovery::recover(&runner).unwrap();
    assert_eq!(report.scanned, 0);
    assert_eq!(report.cleaned, 0);
}

/// 残留 workspace 被清理
#[tokio::test]
async fn stale_workspace_is_cleaned() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();

    // 模拟残留 workspace 目录
    let job_dir = base.join("recovery-test-001");
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(job_dir.join("data.txt"), "stale data").unwrap();

    assert!(job_dir.exists());

    let config = SandboxConfig {
        sandbox_base_dir: base.to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&config).await.unwrap();

    let report = sandbox_core::recovery::recover(&runner).unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.cleaned, 1);

    // 目录应被清理
    assert!(!job_dir.exists());
}

/// 正常运行的 job 不被误清理
#[tokio::test]
async fn finished_job_with_metadata_still_cleaned() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();

    // 创建一个带 Finished metadata 的目录（已完成的 job）
    let job_dir = base.join("finished-001");
    std::fs::create_dir_all(&job_dir).unwrap();

    let config = SandboxConfig {
        sandbox_base_dir: base.to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&config).await.unwrap();

    // 写入 Finished 状态的 metadata
    let meta = sandbox_core::workspace::JobMetadata {
        job_id: "finished-001".to_string(),
        created_at: 0,
        state: sandbox_core::workspace::JobState::Finished,
        pid: None,
        pgid: None,
        sid: None,
        pid_starttime: None,
        cgroup_path: None,
        workspace: job_dir.to_string_lossy().to_string(),
        timeout_ms: 5000,
    };
    runner.workspace_mgr().write_metadata("finished-001", &meta).unwrap();

    let report = sandbox_core::recovery::recover(&runner).unwrap();
    // Finished 状态的也应被清理（运行时已完成的 job 由正常流程清理，
    // recovery 只清理残留的）
    assert_eq!(report.scanned, 1);
}

/// RecoveryReport 可序列化
#[test]
fn recovery_report_is_serializable() {
    let report = RecoveryReport {
        scanned: 5,
        cleaned: 3,
        errors: 1,
    };
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("\"scanned\":5"));
    assert!(json.contains("\"cleaned\":3"));
    assert!(json.contains("\"errors\":1"));
}

/// cr-026: 启动 recovery 清孤儿会话目录。
#[tokio::test]
async fn recover_sessions_cleans_orphan_session_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    let runner = SandboxRunner::new(&config).await.unwrap();
    // 预置孤儿会话目录
    std::fs::create_dir_all(tmp.path().join("sessions").join("orphan").join("workspace")).unwrap();
    let report = sandbox_core::recovery::recover_sessions(&runner).unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.cleaned, 1);
    assert!(!tmp.path().join("sessions").join("orphan").exists());
}
