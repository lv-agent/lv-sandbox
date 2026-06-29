use std::path::{Path, PathBuf};

use crate::access::{AccessFs, AccessRule};

/// 运行时类型，决定预定义的系统路径白名单
#[derive(Debug, Clone, Copy)]
pub enum RuntimeKind {
    Shell,
    Python,
    Node,
    Custom,
}

/// 文件系统访问策略
#[derive(Debug, Clone)]
pub struct FsPolicy {
    rules: Vec<AccessRule>,
}

impl Default for FsPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl FsPolicy {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// 为指定 job 构建完整的文件系统策略
    pub fn for_job(job_workspace: &Path, runtime: RuntimeKind) -> Self {
        let mut policy = Self::new();

        // job workspace 读写
        policy = policy.add_rule(job_workspace, AccessFs::ReadWrite);

        // 系统路径只读 + 执行
        for path in system_exec_paths() {
            policy = policy.add_rule(path, AccessFs::ReadExecute);
        }

        // 系统库路径只读
        for path in system_lib_paths() {
            policy = policy.add_rule(path, AccessFs::ReadOnly);
        }

        // 运行时依赖路径
        for path in runtime_paths(runtime) {
            policy = policy.add_rule(path, AccessFs::ReadOnly);
        }

        // 通用设备路径只读
        for path in device_paths() {
            policy = policy.add_rule(path, AccessFs::ReadOnly);
        }

        // 通用 etc 路径只读
        for path in etc_paths() {
            policy = policy.add_rule(path, AccessFs::ReadOnly);
        }

        // /proc/self 只读（进程信息：status, fd, cmdline 等）
        for path in proc_paths() {
            policy = policy.add_rule(path, AccessFs::ReadOnly);
        }

        policy
    }

    pub fn add_rule(mut self, path: impl Into<PathBuf>, access: AccessFs) -> Self {
        self.rules.push(AccessRule::new(path, access));
        self
    }

    pub fn rules(&self) -> &[AccessRule] {
        &self.rules
    }
}

fn system_exec_paths() -> Vec<&'static str> {
    vec!["/bin", "/usr/bin"]
}

fn system_lib_paths() -> Vec<&'static str> {
    vec!["/lib", "/lib64", "/usr/lib", "/etc/ld.so.cache"]
}

fn runtime_paths(runtime: RuntimeKind) -> Vec<&'static str> {
    match runtime {
        RuntimeKind::Shell => vec![],
        RuntimeKind::Python => vec![
            "/usr/lib/python3",
            "/usr/local/lib/python3",
        ],
        RuntimeKind::Node => vec![
            "/usr/lib/node_modules",
        ],
        RuntimeKind::Custom => vec![],
    }
}

fn device_paths() -> Vec<&'static str> {
    vec!["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"]
}

fn etc_paths() -> Vec<&'static str> {
    vec![
        "/etc/ssl/certs",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/localtime",
    ]
}

fn proc_paths() -> Vec<&'static str> {
    // cr-017: 不再放行整个 /proc（PathBeneath 整树放行 → 跨任务 pid 泄露）。
    // 仅放行全局无害项；/proc/self 由 PreparedRuleset::apply 在 pre_exec 动态放行（见 ruleset.rs）。
    vec![
        "/proc/cpuinfo",
        "/proc/meminfo",
        "/proc/stat",
        "/proc/version",
        "/proc/filesystems",
        "/proc/uptime",
        "/proc/loadavg",
    ]
}
