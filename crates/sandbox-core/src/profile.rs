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
            io_max: Some(Self::default_io_max()),
        }
    }

    /// cr-037: 默认 IO 限速——宽松(防失控,不限正常业务)。major=0 minor=0 = 运行时自动探测。
    fn default_io_max() -> sandbox_cgroup::IoMax {
        sandbox_cgroup::IoMax {
            major: 0,
            minor: 0,
            read_bps: Some(200 * 1024 * 1024),    // 200 MB/s 读
            write_bps: Some(100 * 1024 * 1024),   // 100 MB/s 写
            read_iops: None,
            write_iops: None,
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
                io_max: Some(Self::default_io_max()),
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
                io_max: Some(Self::default_io_max()),
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

    /// cr-012: git 代码-dev profile —— 开 allowlist 出口 + 放宽 rlimit/超时。
    /// egress 目标默认 github.com:443,可由 env `FIXUS_GIT_EGRESS_HOST` 覆盖(指向凭据出口代理)。
    /// CA 信任:operator 经 `FIXUS_GIT_CA_FILE` 指向 PEM 文件 → 以 `SANDBOX_CA_PEM`
    /// 注入 jail env(helper dialer 据此信任自签/内网 CA 上游,如自托管 GitLab)。
    pub fn git() -> Self {
        let mut p = Self::shell();
        p.name = "git".into();
        p.rlimit = RlimitConfig::new()
            .cpu_seconds(120)
            .nofile(256)
            .nproc(256)
            .fsize_mb(1024)
            .core_disabled()
            .stack_mb(16)
            .memlock_disabled();
        p.default_timeout = Duration::from_secs(300);
        p.egress_allowlist = vec![crate::egress::EgressRule {
            host: std::env::var("FIXUS_GIT_EGRESS_HOST")
                .unwrap_or_else(|_| "github.com".to_string()),
            // 默认 443;operator 可经 FIXUS_GIT_EGRESS_PORT 覆盖(凭据出口代理跑非 443 时)。
            port: Some(
                std::env::var("FIXUS_GIT_EGRESS_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(443),
            ),
        }];
        // cr-12 CA 注入(见 read_git_ca_pem)。env 通道免 jail fs 依赖。
        if let Some(pem) = read_git_ca_pem() {
            p.env.insert("SANDBOX_CA_PEM".to_string(), pem);
        }
        // cr-12: git 以 O_RDWR 打开 /dev/null(plumbing / 默认空对象),而 landlock
        // device_paths() 只授 /dev/null ReadOnly → 拒写 → "could not open '/dev/null'
        // for reading and writing"。写 /dev/null 是丢弃,零安全风险,显式补 ReadWrite。
        p.extra_writable_paths.push(PathBuf::from("/dev/null"));
        p
    }
}

/// cr-12: 读 operator 提供的 git CA(env `FIXUS_GIT_CA_FILE` 指向 PEM 文件)→ PEM 内容。
/// 供 [`SandboxProfile::git`] 注入 `SANDBOX_CA_PEM`(helper dialer 据此信任自签/内网 CA)。
/// env 未设 / 路径空 / 文件不可读 / 内容空 → None(helper 回退 webpki 内置根)。
/// 失败时 warn,便于 operator 排错(TLS 失败时能看到 CA 未加载)。
fn read_git_ca_pem() -> Option<String> {
    let path = std::env::var("FIXUS_GIT_CA_FILE")
        .ok()
        .filter(|s| !s.is_empty())?;
    match std::fs::read_to_string(&path) {
        Ok(pem) if !pem.trim().is_empty() => Some(pem),
        Ok(_) => {
            tracing::warn!(path = %path, "FIXUS_GIT_CA_FILE empty; git jail uses builtin CA roots");
            None
        }
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "FIXUS_GIT_CA_FILE unreadable; git jail uses builtin CA roots");
            None
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

    /// 内置默认 profile 的注册表（shell/python/node/git）
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(SandboxProfile::shell());
        registry.register(SandboxProfile::python());
        registry.register(SandboxProfile::node());
        registry.register(SandboxProfile::git());
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
        assert_eq!(names, vec!["git", "node", "python", "shell"]);
    }

    #[test]
    fn names_empty_registry_returns_empty() {
        let registry = ProfileRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn git_profile_has_egress_and_relaxed_rlimits() {
        let g = SandboxProfile::git();
        let s = SandboxProfile::shell();
        assert_eq!(g.name, "git");
        assert!(!g.egress_allowlist.is_empty(), "git profile must allow egress");
        assert!(g.egress_allowlist.iter().any(|r| r.port == Some(443)), "must allow :443");
        // rlimits strictly looser than shell (clone/push need CPU time, fs for repo+pack, fds)
        assert!(g.rlimit.fsize_bytes > s.rlimit.fsize_bytes, "fsize must be larger");
        assert!(g.rlimit.cpu_seconds > s.rlimit.cpu_seconds, "cpu_seconds must be larger");
        assert!(g.rlimit.nofile > s.rlimit.nofile, "nofile must be larger");
    }

    // cr-12 CA 注入测试。注意:本组测试改进程级 env,须串行跑(--test-threads=1)。
    fn scratch_ca_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fixus-git-ca-test-{}.pem", std::process::id()))
    }

    #[test]
    fn git_profile_injects_ca_pem_from_fixus_git_ca_file() {
        let ca_path = scratch_ca_path();
        let _ = std::fs::remove_file(&ca_path);
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBmockPEM\n-----END CERTIFICATE-----\n";
        std::fs::write(&ca_path, pem).unwrap();
        std::env::set_var("FIXUS_GIT_CA_FILE", &ca_path);
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_CA_FILE");
        let _ = std::fs::remove_file(&ca_path);
        assert_eq!(
            g.env.get("SANDBOX_CA_PEM").map(String::as_str),
            Some(pem),
            "FIXUS_GIT_CA_FILE set → SANDBOX_CA_PEM injected verbatim"
        );
    }

    #[test]
    fn git_profile_no_ca_when_unset_or_unreadable() {
        // 未设 → 不注入
        std::env::remove_var("FIXUS_GIT_CA_FILE");
        let g = SandboxProfile::git();
        assert!(!g.env.contains_key("SANDBOX_CA_PEM"), "unset → no injection");
        // 指向不存在的文件 → 跳过(不注入;helper 回退 webpki 根)
        std::env::set_var("FIXUS_GIT_CA_FILE", "/nonexistent/fixus-git-ca.pem");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_CA_FILE");
        assert!(!g.env.contains_key("SANDBOX_CA_PEM"), "unreadable CA → skip injection");
    }

    #[test]
    fn git_profile_grants_dev_null_writable() {
        // cr-12: git O_RDWR 打开 /dev/null,需 ReadWrite(写 /dev/null = 丢弃,无害)。
        let g = SandboxProfile::git();
        assert!(
            g.extra_writable_paths
                .iter()
                .any(|p| p == std::path::Path::new("/dev/null")),
            "git profile must grant /dev/null ReadWrite (git opens it O_RDWR; device_paths only grants ReadOnly)"
        );
    }

    #[test]
    fn git_profile_egress_port_env_overridable() {
        // FIXUS_GIT_EGRESS_PORT 覆盖默认 443(凭据代理跑非 443)。env 测试,串行。
        std::env::set_var("FIXUS_GIT_EGRESS_PORT", "8443");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_EGRESS_PORT");
        assert_eq!(
            g.egress_allowlist[0].port,
            Some(8443),
            "FIXUS_GIT_EGRESS_PORT must override default 443"
        );
        // 非法值 → 回退默认 443
        std::env::set_var("FIXUS_GIT_EGRESS_PORT", "not-a-port");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_EGRESS_PORT");
        assert_eq!(g.egress_allowlist[0].port, Some(443), "invalid port → fallback 443");
    }
}
