//! workspace 模块集成测试：WorkspaceManager
//!
//! 测试工作空间目录管理、metadata 读写、清理。

use sandbox_core::workspace::{JobMetadata, JobState, WorkspaceManager};
use std::time::{SystemTime, UNIX_EPOCH};

/// 创建临时工作目录的辅助函数
fn create_tmp_workspace() -> (tempfile::TempDir, WorkspaceManager) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024 * 1024);
    (tmp, mgr)
}

#[test]
fn create_job_workspace_generates_correct_directory_layout() {
    let (_tmp, mgr) = create_tmp_workspace();

    let ws = mgr.create_job_workspace("job-001").expect("creation failed");

    assert!(ws.root.exists());
    assert!(ws.workspace.exists());
    assert!(ws.tmp.exists());
    assert!(ws.input.exists());
    assert!(ws.output.exists());

    // 验证路径拼接正确
    assert_eq!(ws.root, mgr.base_dir().join("job-001"));
    assert_eq!(ws.workspace, mgr.base_dir().join("job-001").join("workspace"));
    assert_eq!(ws.tmp, mgr.base_dir().join("job-001").join("tmp"));
}

#[test]
fn write_and_read_metadata() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("creation failed");

    let meta = JobMetadata {
        job_id: "job-001".to_string(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        state: JobState::Running,
        pid: Some(12345),
        pgid: Some(12345),
        sid: Some(12345),
        pid_starttime: Some("99999".to_string()),
        cgroup_path: None,
        workspace: mgr.base_dir().join("job-001").to_string_lossy().to_string(),
        timeout_ms: 5000,
    };

    mgr.write_metadata("job-001", &meta).expect("failed to write");

    let loaded = mgr.read_metadata("job-001").expect("failed to read");
    assert!(loaded.is_some());

    let loaded = loaded.unwrap();
    assert_eq!(loaded.job_id, "job-001");
    assert_eq!(loaded.pid, Some(12345));
    assert_eq!(loaded.timeout_ms, 5000);
    assert!(matches!(loaded.state, JobState::Running));
}

#[test]
fn read_nonexistent_metadata_returns_none() {
    let (_tmp, mgr) = create_tmp_workspace();

    let result = mgr.read_metadata("nonexistent").expect("should not fail");
    assert!(result.is_none());
}

#[test]
fn list_all_jobs() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("creation failed");
    mgr.create_job_workspace("job-002").expect("creation failed");
    mgr.create_job_workspace("job-003").expect("creation failed");

    let mut jobs = mgr.list_jobs().expect("list failed");
    jobs.sort();

    assert_eq!(jobs, vec!["job-001", "job-002", "job-003"]);
}

#[test]
fn cleanup_job_deletes_entire_directory() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("creation failed");
    assert!(mgr.base_dir().join("job-001").exists());

    mgr.cleanup_job("job-001").expect("cleanup failed");
    assert!(!mgr.base_dir().join("job-001").exists());
}

#[test]
fn cleanup_nonexistent_job_does_not_error() {
    let (_tmp, mgr) = create_tmp_workspace();

    // 不存在的 job 应该静默成功
    mgr.cleanup_job("nonexistent").expect("cleaning up a nonexistent job should not error");
}

#[test]
fn metadata_state_transition() {
    let (_tmp, mgr) = create_tmp_workspace();
    mgr.create_job_workspace("job-001").expect("creation failed");

    // Initializing
    let meta = JobMetadata {
        job_id: "job-001".to_string(),
        created_at: 0,
        state: JobState::Initializing,
        pid: None,
        pgid: None,
        sid: None,
        pid_starttime: None,
        cgroup_path: None,
        workspace: "/sandboxes/job-001".to_string(),
        timeout_ms: 5000,
    };
    mgr.write_metadata("job-001", &meta).expect("failed to write");

    // 更新为 Running
    let meta = JobMetadata {
        state: JobState::Running,
        pid: Some(999),
        pgid: Some(999),
        sid: Some(999),
        ..meta
    };
    mgr.write_metadata("job-001", &meta).expect("failed to write");

    let loaded = mgr.read_metadata("job-001").unwrap().unwrap();
    assert!(matches!(loaded.state, JobState::Running));
    assert_eq!(loaded.pid, Some(999));
}

// ==================== 磁盘水位监控 ====================

#[test]
fn normal_disk_space_watermark_check_returns_true() {
    let (tmp, _mgr) = create_tmp_workspace();

    // 水位线设为 1MB，临时目录所在文件系统肯定有超过 1MB 剩余空间
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024);

    let result = mgr.check_disk_watermark().expect("watermark check should not error");
    assert!(result, "watermark check should pass with normal free space");
}

#[test]
fn watermark_set_to_huge_value_watermark_check_returns_false() {
    let (tmp, _mgr) = create_tmp_workspace();

    // 水位线设为 1 PB，任何文件系统都不可能有这么多剩余空间
    let one_pb: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
    let mgr = WorkspaceManager::new(tmp.path(), one_pb);

    let result = mgr.check_disk_watermark().expect("watermark check should not error");
    assert!(!result, "should return false when watermark exceeds free space");
}

// ==================== Phase 6: 磁盘隔离增强 ====================

#[test]
fn workspace_size_computes_total_directory_size() {
    let (_tmp, mgr) = create_tmp_workspace();
    let ws = mgr.create_job_workspace("job-size-001").expect("creation failed");

    // 空 workspace 应为 0
    let size = mgr.workspace_size("job-size-001").expect("size computation should not error");
    assert_eq!(size, 0, "empty workspace should be 0 bytes");

    // 写入一些数据
    std::fs::write(ws.workspace.join("test.txt"), "hello world").expect("failed to write");
    std::fs::write(ws.workspace.join("data.bin"), [0u8; 1024]).expect("failed to write");

    let size = mgr.workspace_size("job-size-001").expect("size computation should not error");
    // "hello world" = 11 字节 + 1024 字节 = 1035 字节
    // 加上目录项本身的开销（通常 4096 per dir），但至少应 ≥ 1035
    assert!(
        size >= 1035,
        "workspace size should be at least 1035 bytes, actual {}",
        size
    );

    mgr.cleanup_job("job-size-001").expect("cleanup failed");
}

#[test]
fn workspace_size_nonexistent_job_returns_0() {
    let (_tmp, mgr) = create_tmp_workspace();

    let size = mgr.workspace_size("nonexistent").expect("should not error");
    assert_eq!(size, 0, "nonexistent job size should be 0");
}

#[test]
fn cleanup_all_jobs_batch_cleanup() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("batch-001").expect("creation failed");
    mgr.create_job_workspace("batch-002").expect("creation failed");
    mgr.create_job_workspace("batch-003").expect("creation failed");

    assert_eq!(mgr.list_jobs().expect("list failed").len(), 3);

    let cleaned = mgr.cleanup_all_jobs().expect("batch cleanup should not error");
    assert_eq!(cleaned, 3, "should clean 3 jobs");
    assert_eq!(mgr.list_jobs().expect("list failed").len(), 0);
}

#[test]
fn cleanup_all_jobs_empty_dir_does_not_error() {
    let (tmp, _mgr) = create_tmp_workspace();
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024);

    let cleaned = mgr.cleanup_all_jobs().expect("empty dir should not error");
    assert_eq!(cleaned, 0, "empty dir should clean 0 jobs");
}
