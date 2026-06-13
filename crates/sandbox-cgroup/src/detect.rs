use serde::Serialize;
use std::path::{Path, PathBuf};

/// cgroup v2 控制器类型
#[derive(Debug, Clone, Copy, Serialize)]
pub enum CgroupController {
    Memory,
    Cpu,
    Pids,
    Io,
}

/// cgroup v2 可用性检测结果
#[derive(Debug, Clone, Serialize)]
pub struct CgroupAvailability {
    /// cgroup v2 是否可用
    pub available: bool,
    /// 当前 cgroup 路径
    pub cgroup_path: Option<PathBuf>,
    /// 可用的控制器
    pub controllers: Vec<CgroupController>,
    /// 是否能创建子 cgroup
    pub can_create_subgroup: bool,
    /// 是否能迁移进程
    pub can_migrate_processes: bool,
    /// 不可用原因
    pub reason: Option<String>,
}

/// 检测当前环境中 cgroup v2 的可用性
///
/// 检查流程：
/// 1. /sys/fs/cgroup 是否为 cgroup2 文件系统
/// 2. 读取 /proc/self/cgroup 获取当前 cgroup 路径
/// 3. 检测可用控制器
/// 4. 尝试创建子 cgroup（验证写权限）
pub fn detect() -> CgroupAvailability {
    let cgroup_root = Path::new("/sys/fs/cgroup");

    // 1. 检查 cgroup2 文件系统
    if !cgroup_root.exists() {
        return CgroupAvailability {
            available: false,
            cgroup_path: None,
            controllers: vec![],
            can_create_subgroup: false,
            can_migrate_processes: false,
            reason: Some("/sys/fs/cgroup 不存在".into()),
        };
    }

    // 2. 读取当前进程的 cgroup 路径
    let cgroup_path = match read_self_cgroup_path(cgroup_root) {
        Some(p) => p,
        None => {
            return CgroupAvailability {
                available: false,
                cgroup_path: None,
                controllers: vec![],
                can_create_subgroup: false,
                can_migrate_processes: false,
                reason: Some("无法读取 /proc/self/cgroup".into()),
            };
        }
    };

    // 3. 检测控制器
    let controllers = detect_controllers(&cgroup_path);

    // 4. 尝试创建子 cgroup
    let can_create = can_create_subgroup(&cgroup_path);
    let can_migrate = can_create; // 能创建子 cgroup 就能迁移进程

    CgroupAvailability {
        available: true,
        cgroup_path: Some(cgroup_path),
        controllers,
        can_create_subgroup: can_create,
        can_migrate_processes: can_migrate,
        reason: if can_create {
            None
        } else {
            Some("当前用户无权在 cgroup 目录创建子组".into())
        },
    }
}

/// 读取 /proc/self/cgroup 获取当前进程的 cgroup v2 路径
fn read_self_cgroup_path(cgroup_root: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;

    // cgroup v2 格式: "0::/path/to/cgroup"
    for line in content.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() == 3 && parts[0] == "0" {
            let relative_path = parts[2].trim_start_matches('/');
            if relative_path.is_empty() {
                return Some(cgroup_root.to_path_buf());
            }
            return Some(cgroup_root.join(relative_path));
        }
    }

    // fallback: 直接用 cgroup_root
    Some(cgroup_root.to_path_buf())
}

/// 检测可用的 cgroup 控制器
fn detect_controllers(cgroup_path: &Path) -> Vec<CgroupController> {
    let mut controllers = Vec::new();

    // 从 cgroup.controllers 读取
    if let Ok(content) = std::fs::read_to_string(cgroup_path.join("cgroup.controllers")) {
        let available: Vec<&str> = content.split_whitespace().collect();
        if available.iter().any(|c| *c == "memory") {
            controllers.push(CgroupController::Memory);
        }
        if available.iter().any(|c| *c == "cpu") {
            controllers.push(CgroupController::Cpu);
        }
        if available.iter().any(|c| *c == "pids") {
            controllers.push(CgroupController::Pids);
        }
        if available.iter().any(|c| *c == "io") {
            controllers.push(CgroupController::Io);
        }
    }

    controllers
}

/// 测试是否能在 cgroup 目录下创建子组
fn can_create_subgroup(cgroup_path: &Path) -> bool {
    let test_path = cgroup_path.join(".sandbox-probe");
    if std::fs::create_dir(&test_path).is_ok() {
        let _ = std::fs::remove_dir(&test_path);
        true
    } else {
        false
    }
}
