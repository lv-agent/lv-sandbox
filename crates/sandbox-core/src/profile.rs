use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::rlimit::RlimitConfig;

/// Landlock 策略模板。
/// profile 中不绑定具体路径，运行时根据 job workspace 路径实例化 FsPolicy。
#[derive(Debug, Clone)]
pub enum LandlockTemplate {
    Shell,
    Python,
    Node,
    Custom { extra_readonly_paths: Vec<PathBuf> },
    Disabled,
}

/// 完整的沙箱策略配置
#[derive(Debug, Clone)]
pub struct SandboxProfile {
    pub name: String,
    pub rlimit: RlimitConfig,
    pub landlock_template: LandlockTemplate,
    pub seccomp_profile: Option<sandbox_seccomp::SeccompProfile>,
    pub cgroup_resources: Option<sandbox_cgroup::CgroupResources>,
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
    pub default_timeout: Duration,
    pub fail_closed: bool,
    /// 额外的共享只读路径（包目录、离线 wheel 等）
    pub extra_readonly_paths: Vec<PathBuf>,
    /// cr-019: 出站白名单。空 = 零出站(默认)。
    pub egress_allowlist: Vec<crate::egress::EgressRule>,
    /// cr-022: 工作区聚合磁盘上限(MB)。None = 不限(默认,看门狗不起)。
    pub disk_quota_mb: Option<u64>,
    /// cr-025: template baseline 环境变量(覆盖核心非保护项;HOME/TMPDIR 保护)。
    pub env: HashMap<String, String>,
    /// cr-028: 额外可写路径(卷等,landlock ReadWrite)。默认空。
    pub extra_writable_paths: Vec<PathBuf>,
}

impl SandboxProfile {
    /// 默认 cgroup 资源限制（轻量级 shell 任务级别）
    fn default_cgroup_resources() -> sandbox_cgroup::CgroupResources {
        sandbox_cgroup::CgroupResources {
            memory_max: Some(128 * 1024 * 1024),    // 128MB
            cpu_max_quota: Some(200_000),             // 200ms
            cpu_max_period: Some(1_000_000),          // 每 1s 周期
            pids_max: Some(32),
            io_max: None,
        }
    }

    /// 轻量 shell 任务 profile
    pub fn shell() -> Self {
        Self {
            name: "shell".into(),
            rlimit: RlimitConfig::new()
                .cpu_seconds(2)
                .nofile(64)
                .nproc(32)
                .fsize_mb(10)
                .core_disabled()
                .stack_mb(8)
                .memlock_disabled(),
            landlock_template: LandlockTemplate::Shell,
            seccomp_profile: Some(sandbox_seccomp::SeccompProfile::default_denylist()),
            cgroup_resources: Some(Self::default_cgroup_resources()),
            fail_closed: false, // cgroup 不可用时优雅降级
            max_stdout_bytes: 5 * 1024 * 1024,
            max_stderr_bytes: 5 * 1024 * 1024,
            default_timeout: Duration::from_secs(5),
            extra_readonly_paths: vec![],
            egress_allowlist: vec![],
            disk_quota_mb: None,
            env: HashMap::new(),
            extra_writable_paths: vec![],
        }
    }

    /// Python 任务 profile
    pub fn python() -> Self {
        Self {
            name: "python".into(),
            rlimit: RlimitConfig::new()
                .cpu_seconds(2)
                .nofile(64)
                .nproc(32)
                .fsize_mb(10)
                .core_disabled()
                .stack_mb(8)
                .memlock_disabled(),
            landlock_template: LandlockTemplate::Python,
            seccomp_profile: Some(sandbox_seccomp::SeccompProfile::default_denylist()),
            cgroup_resources: Some(sandbox_cgroup::CgroupResources {
                memory_max: Some(256 * 1024 * 1024), // Python 需要更多内存
                cpu_max_quota: Some(200_000),
                cpu_max_period: Some(1_000_000),
                pids_max: Some(32),
                io_max: None,
            }),
            fail_closed: false,
            max_stdout_bytes: 5 * 1024 * 1024,
            max_stderr_bytes: 5 * 1024 * 1024,
            default_timeout: Duration::from_secs(5),
            extra_readonly_paths: vec![],
            egress_allowlist: vec![],
            disk_quota_mb: None,
            env: HashMap::new(),
            extra_writable_paths: vec![],
        }
    }

    /// Node.js 任务 profile
    pub fn node() -> Self {
        Self {
            name: "node".into(),
            rlimit: RlimitConfig::new()
                .cpu_seconds(2)
                .nofile(64)
                .nproc(32)
                .fsize_mb(10)
                .core_disabled()
                .stack_mb(8)
                .memlock_disabled(),
            landlock_template: LandlockTemplate::Node,
            seccomp_profile: Some(sandbox_seccomp::SeccompProfile::default_denylist()),
            cgroup_resources: Some(sandbox_cgroup::CgroupResources {
                memory_max: Some(256 * 1024 * 1024), // Node 需要更多内存
                cpu_max_quota: Some(200_000),
                cpu_max_period: Some(1_000_000),
                pids_max: Some(32),
                io_max: None,
            }),
            fail_closed: false,
            max_stdout_bytes: 5 * 1024 * 1024,
            max_stderr_bytes: 5 * 1024 * 1024,
            default_timeout: Duration::from_secs(5),
            extra_readonly_paths: vec![],
            egress_allowlist: vec![],
            disk_quota_mb: None,
            env: HashMap::new(),
            extra_writable_paths: vec![],
        }
    }
}

/// Profile 注册表：名称 → 策略配置
#[derive(Debug, Clone)]
pub struct ProfileRegistry {
    profiles: HashMap<String, SandboxProfile>,
}

impl ProfileRegistry {
    /// 空注册表
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    /// 内置默认 profile 的注册表（shell/python/node）
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(SandboxProfile::shell());
        registry.register(SandboxProfile::python());
        registry.register(SandboxProfile::node());
        registry
    }

    /// 注册自定义 profile
    pub fn register(&mut self, profile: SandboxProfile) {
        self.profiles.insert(profile.name.clone(), profile);
    }

    /// 按名称查询
    pub fn get(&self, name: &str) -> Option<&SandboxProfile> {
        self.profiles.get(name)
    }

    /// 列出所有已注册的 profile 名
    pub fn names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_returns_all_registered() {
        let registry = ProfileRegistry::with_defaults();
        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["node", "python", "shell"]);
    }

    #[test]
    fn names_empty_registry_returns_empty() {
        let registry = ProfileRegistry::new();
        assert!(registry.names().is_empty());
    }
}
