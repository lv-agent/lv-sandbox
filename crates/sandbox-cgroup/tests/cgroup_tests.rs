//! sandbox-cgroup 集成测试
//!
//! TDD: cgroup v2 检测 + JobCgroup 创建/资源限制/进程迁移/销毁

use std::path::Path;

use sandbox_cgroup::{
    detect, CgroupAvailability, CgroupResources, JobCgroup,
};

/// 获取可写的 cgroup 父目录
///
/// 优先使用检测到的 cgroup_path，如果不可写则搜索 /sys/fs/cgroup 下的可写目录。
fn writable_cgroup_parent() -> Option<String> {
    let avail = detect();
    if !avail.available {
        return None;
    }

    // 1. 先尝试检测到的 cgroup_path
    if let Some(ref path) = avail.cgroup_path {
        if try_create_subgroup(path) {
            return Some(path.to_string_lossy().to_string());
        }
    }

    // 2. 搜索 /sys/fs/cgroup 下可写的目录
    if let Ok(entries) = std::fs::read_dir("/sys/fs/cgroup") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && try_create_subgroup(&path) {
                return Some(path.to_string_lossy().to_string());
            }
            // 递归一层
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                for sub_entry in sub_entries.flatten() {
                    let sub_path = sub_entry.path();
                    if sub_path.is_dir() && try_create_subgroup(&sub_path) {
                        return Some(sub_path.to_string_lossy().to_string());
                    }
                    // 再递归一层
                    if let Ok(deep_entries) = std::fs::read_dir(&sub_path) {
                        for deep_entry in deep_entries.flatten() {
                            let deep_path = deep_entry.path();
                            if deep_path.is_dir() && try_create_subgroup(&deep_path) {
                                return Some(deep_path.to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn try_create_subgroup(path: &std::path::Path) -> bool {
    let test = path.join(".sandbox-probe");
    if std::fs::create_dir(&test).is_ok() {
        let _ = std::fs::remove_dir(&test);
        return true;
    }
    false
}

// ==================== 检测 ====================

#[test]
fn cgroup_v2检测_返回有效结构() {
    let avail = detect();

    // 在此环境中 cgroup v2 应该可用
    if avail.available {
        assert!(
            avail.cgroup_path.is_some(),
            "可用时应有 cgroup_path"
        );
        assert!(
            !avail.controllers.is_empty(),
            "应有至少一个控制器"
        );
    } else {
        eprintln!("cgroup v2 不可用: {:?}", avail.reason);
    }
}

// ==================== JobCgroup ====================

#[test]
fn job_cgroup_创建成功() {
    let parent = match writable_cgroup_parent() {
        Some(p) => p,
        None => {
            eprintln!("跳过: 无可写的 cgroup 目录");
            return;
        }
    };

    let avail = detect();
    let resources = CgroupResources {
        memory_max: Some(100 * 1024 * 1024), // 100MB
        cpu_max_quota: Some(50000),           // 50ms
        cpu_max_period: Some(100000),         // 100ms = 50% CPU
        pids_max: Some(32),
        io_max: None,
    };

    let cg = JobCgroup::create(
        "test-create-001",
        Path::new(&parent),
        &resources,
        &avail,
    );

    assert!(cg.is_ok(), "创建 cgroup 应成功: {:?}", cg.err());

    let cg = cg.unwrap();
    let cg_path = cg.path().clone();

    // 验证目录存在
    assert!(cg_path.exists(), "cgroup 目录应存在");

    // 验证 cgroup.procs 文件存在
    assert!(
        cg_path.join("cgroup.procs").exists(),
        "cgroup.procs 应存在"
    );

    // 销毁
    cg.destroy().expect("销毁不应失败");
    assert!(!cg_path.exists(), "销毁后目录应被删除");
}

#[test]
fn job_cgroup_资源限制写入() {
    let parent = match writable_cgroup_parent() {
        Some(p) => p,
        None => {
            eprintln!("跳过: 无可写的 cgroup 目录");
            return;
        }
    };

    let avail = detect();
    let resources = CgroupResources {
        memory_max: Some(50 * 1024 * 1024), // 50MB
        cpu_max_quota: Some(100000),         // 100ms
        cpu_max_period: Some(200000),         // 200ms = 50% CPU
        pids_max: Some(16),
        io_max: None,
    };

    let cg = JobCgroup::create(
        "test-resources-001",
        Path::new(&parent),
        &resources,
        &avail,
    ).expect("创建应成功");

    let cg_path = cg.path().clone();

    // 验证 memory.max
    let mem_max = std::fs::read_to_string(cg_path.join("memory.max"))
        .expect("应能读取 memory.max");
    assert_eq!(mem_max.trim(), "52428800", "memory.max 应为 50MB");

    // 验证 pids.max
    let pids_max = std::fs::read_to_string(cg_path.join("pids.max"))
        .expect("应能读取 pids.max");
    assert_eq!(pids_max.trim(), "16", "pids.max 应为 16");

    // 验证 cpu.max
    let cpu_max = std::fs::read_to_string(cg_path.join("cpu.max"))
        .expect("应能读取 cpu.max");
    assert!(
        cpu_max.trim().starts_with("100000 200000"),
        "cpu.max 应为 '100000 200000': {}",
        cpu_max.trim()
    );

    cg.destroy().expect("销毁不应失败");
}

#[test]
fn job_cgroup_进程迁移() {
    let parent = match writable_cgroup_parent() {
        Some(p) => p,
        None => {
            eprintln!("跳过: 无可写的 cgroup 目录");
            return;
        }
    };

    let avail = detect();
    let resources = CgroupResources {
        memory_max: Some(500 * 1024 * 1024),
        cpu_max_quota: None,
        cpu_max_period: None,
        pids_max: Some(64),
        io_max: None,
    };

    let cg = JobCgroup::create(
        "test-migrate-001",
        Path::new(&parent),
        &resources,
        &avail,
    ).expect("创建应成功");

    let cg_path = cg.path().clone();

    // 启动一个 sleep 子进程
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .expect("启动子进程失败");
    let pid = child.id();

    // 尝试迁移子进程到 cgroup
    // 注意：在某些环境（如 WSL2 的 nsdelegate）下可能因权限不足失败
    match cg.migrate_process(pid) {
        Ok(()) => {
            // 验证进程在 cgroup 中
            let procs = std::fs::read_to_string(cg_path.join("cgroup.procs"))
                .expect("应能读取 cgroup.procs");
            assert!(
                procs.contains(&pid.to_string()),
                "cgroup.procs 应包含 PID {}: {}",
                pid,
                procs
            );

            // 列出进程
            let listed = cg.processes().expect("列出进程不应失败");
            assert!(listed.contains(&pid), "processes() 应包含 PID {}", pid);
        }
        Err(e) => {
            // 迁移失败在受限环境下是预期的（nsdelegate、跨 cgroup 树等）
            eprintln!("进程迁移跳过（环境限制）: {}", e);
        }
    }

    // 杀掉子进程
    unsafe { libc::kill(pid as i32, libc::SIGKILL); }
    let _ = child.wait();

    // 销毁 cgroup
    cg.destroy().expect("销毁不应失败");
}

#[test]
fn job_cgroup_资源使用统计() {
    let parent = match writable_cgroup_parent() {
        Some(p) => p,
        None => {
            eprintln!("跳过: 无可写的 cgroup 目录");
            return;
        }
    };

    let avail = detect();
    let resources = CgroupResources {
        memory_max: Some(500 * 1024 * 1024),
        cpu_max_quota: None,
        cpu_max_period: None,
        pids_max: Some(64),
        io_max: None,
    };

    let cg = JobCgroup::create(
        "test-usage-001",
        Path::new(&parent),
        &resources,
        &avail,
    ).expect("创建应成功");

    // 空 cgroup 应能读取统计
    let usage = cg.resource_usage().expect("读取统计不应失败");
    // 空 cgroup 的 memory_current 应为 0 或很小
    if let Some(mem) = usage.memory_current {
        assert!(mem < 1024 * 1024, "空 cgroup 内存应很小: {}", mem);
    }

    cg.destroy().expect("销毁不应失败");
}

#[test]
fn job_cgroup_不可用时创建返回错误() {
    let avail = CgroupAvailability {
        available: false,
        cgroup_path: None,
        controllers: vec![],
        can_create_subgroup: false,
        can_migrate_processes: false,
        reason: Some("测试不可用".into()),
    };

    let resources = CgroupResources {
        memory_max: Some(1024),
        cpu_max_quota: None,
        cpu_max_period: None,
        pids_max: None,
        io_max: None,
    };

    let result = JobCgroup::create(
        "test-unavail",
        Path::new("/nonexistent"),
        &resources,
        &avail,
    );

    assert!(result.is_err(), "不可用时应返回错误");
}
