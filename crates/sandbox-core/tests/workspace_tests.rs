//! workspace 模块集成测试：WorkspaceManager
//!
//! 测试工作空间目录管理、metadata 读写、清理。

use sandbox_core::workspace::{JobMetadata, JobState, WorkspaceManager};
use std::time::{SystemTime, UNIX_EPOCH};

/// 创建临时工作目录的辅助函数
fn create_tmp_workspace() -> (tempfile::TempDir, WorkspaceManager) {
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024 * 1024);
    (tmp, mgr)
}

#[test]
fn 创建job工作空间_生成正确的目录结构() {
    let (_tmp, mgr) = create_tmp_workspace();

    let ws = mgr.create_job_workspace("job-001").expect("创建失败");

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
fn 写入并读取metadata() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("创建失败");

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

    mgr.write_metadata("job-001", &meta).expect("写入失败");

    let loaded = mgr.read_metadata("job-001").expect("读取失败");
    assert!(loaded.is_some());

    let loaded = loaded.unwrap();
    assert_eq!(loaded.job_id, "job-001");
    assert_eq!(loaded.pid, Some(12345));
    assert_eq!(loaded.timeout_ms, 5000);
    assert!(matches!(loaded.state, JobState::Running));
}

#[test]
fn 读取不存在的metadata返回none() {
    let (_tmp, mgr) = create_tmp_workspace();

    let result = mgr.read_metadata("nonexistent").expect("不应失败");
    assert!(result.is_none());
}

#[test]
fn 列出所有job() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("创建失败");
    mgr.create_job_workspace("job-002").expect("创建失败");
    mgr.create_job_workspace("job-003").expect("创建失败");

    let mut jobs = mgr.list_jobs().expect("列出失败");
    jobs.sort();

    assert_eq!(jobs, vec!["job-001", "job-002", "job-003"]);
}

#[test]
fn 清理job删除整个目录() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("job-001").expect("创建失败");
    assert!(mgr.base_dir().join("job-001").exists());

    mgr.cleanup_job("job-001").expect("清理失败");
    assert!(!mgr.base_dir().join("job-001").exists());
}

#[test]
fn 清理不存在的job不报错() {
    let (_tmp, mgr) = create_tmp_workspace();

    // 不存在的 job 应该静默成功
    mgr.cleanup_job("nonexistent").expect("清理不存在的 job 不应报错");
}

#[test]
fn metadata状态转换() {
    let (_tmp, mgr) = create_tmp_workspace();
    mgr.create_job_workspace("job-001").expect("创建失败");

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
    mgr.write_metadata("job-001", &meta).expect("写入失败");

    // 更新为 Running
    let meta = JobMetadata {
        state: JobState::Running,
        pid: Some(999),
        pgid: Some(999),
        sid: Some(999),
        ..meta
    };
    mgr.write_metadata("job-001", &meta).expect("写入失败");

    let loaded = mgr.read_metadata("job-001").unwrap().unwrap();
    assert!(matches!(loaded.state, JobState::Running));
    assert_eq!(loaded.pid, Some(999));
}

// ==================== 磁盘水位监控 ====================

#[test]
fn 正常磁盘空间_水位检查返回true() {
    let (tmp, _mgr) = create_tmp_workspace();

    // 水位线设为 1MB，临时目录所在文件系统肯定有超过 1MB 剩余空间
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024);

    let result = mgr.check_disk_watermark().expect("水位检查不应报错");
    assert!(result, "正常空间时水位检查应通过");
}

#[test]
fn 水位线设为极大值_水位检查返回false() {
    let (tmp, _mgr) = create_tmp_workspace();

    // 水位线设为 1 PB，任何文件系统都不可能有这么多剩余空间
    let one_pb: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
    let mgr = WorkspaceManager::new(tmp.path(), one_pb);

    let result = mgr.check_disk_watermark().expect("水位检查不应报错");
    assert!(!result, "水位线超过可用空间时应返回 false");
}

// ==================== Phase 6: 磁盘隔离增强 ====================

#[test]
fn workspace_size_计算目录总大小() {
    let (_tmp, mgr) = create_tmp_workspace();
    let ws = mgr.create_job_workspace("job-size-001").expect("创建失败");

    // 空 workspace 应为 0
    let size = mgr.workspace_size("job-size-001").expect("计算大小不应报错");
    assert_eq!(size, 0, "空 workspace 应为 0 字节");

    // 写入一些数据
    std::fs::write(ws.workspace.join("test.txt"), "hello world").expect("写入失败");
    std::fs::write(ws.workspace.join("data.bin"), &[0u8; 1024]).expect("写入失败");

    let size = mgr.workspace_size("job-size-001").expect("计算大小不应报错");
    // "hello world" = 11 字节 + 1024 字节 = 1035 字节
    // 加上目录项本身的开销（通常 4096 per dir），但至少应 ≥ 1035
    assert!(
        size >= 1035,
        "workspace 大小应至少 1035 字节，实际 {}",
        size
    );

    mgr.cleanup_job("job-size-001").expect("清理失败");
}

#[test]
fn workspace_size_不存在的job返回0() {
    let (_tmp, mgr) = create_tmp_workspace();

    let size = mgr.workspace_size("nonexistent").expect("不应报错");
    assert_eq!(size, 0, "不存在的 job 大小应为 0");
}

#[test]
fn cleanup_all_jobs_批量清理() {
    let (_tmp, mgr) = create_tmp_workspace();

    mgr.create_job_workspace("batch-001").expect("创建失败");
    mgr.create_job_workspace("batch-002").expect("创建失败");
    mgr.create_job_workspace("batch-003").expect("创建失败");

    assert_eq!(mgr.list_jobs().expect("列出失败").len(), 3);

    let cleaned = mgr.cleanup_all_jobs().expect("批量清理不应报错");
    assert_eq!(cleaned, 3, "应清理 3 个 job");
    assert_eq!(mgr.list_jobs().expect("列出失败").len(), 0);
}

#[test]
fn cleanup_all_jobs_空目录不报错() {
    let (tmp, _mgr) = create_tmp_workspace();
    let mgr = WorkspaceManager::new(tmp.path(), 1024 * 1024);

    let cleaned = mgr.cleanup_all_jobs().expect("空目录不应报错");
    assert_eq!(cleaned, 0, "空目录应清理 0 个");
}
