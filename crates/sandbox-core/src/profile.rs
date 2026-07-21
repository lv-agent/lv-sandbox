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
        // env 覆盖解析集中在这一层;profile 构造下沉到 git_inner(纯函数,便于并行单测,
        // 不碰进程级 env)。default 见各 env 的 unwrap_or。
        let host = std::env::var("FIXUS_GIT_EGRESS_HOST")
            .unwrap_or_else(|_| "github.com".to_string());
        let port = std::env::var("FIXUS_GIT_EGRESS_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(443);
        let ca_pem = read_git_ca_pem();
        let sentinel = std::env::var("FIXUS_GIT_SENTINEL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self::git_inner(&host, port, ca_pem.as_deref(), sentinel.as_deref())
    }

    /// cr-12: git profile 构造(纯函数,无 env 读取)。把 host/port/CA 作为显式入参,
    /// 使行为可并行单测(避免进程级 env 在并行测试下竞态)。env → 入参胶水在 [`SandboxProfile::git`]。
    fn git_inner(host: &str, port: u16, ca_pem: Option<&str>, sentinel: Option<&str>) -> Self {
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
            host: host.to_string(),
            // 默认 443;operator 可经 FIXUS_GIT_EGRESS_PORT 覆盖(凭据出口代理跑非 443 时)。
            port: Some(port),
        }];
        // cr-12 CA 注入(见 read_git_ca_pem)。env 通道免 jail fs 依赖。
        if let Some(pem) = ca_pem {
            p.env.insert("SANDBOX_CA_PEM".to_string(), pem.to_string());
        }
        // cr-12 G2: sentinel 占位凭据进牢(helper 据此加 Authorization 头;出口代理 fake→real 兑换)。
        // 非密可公开;真 token 只在牢外 swap-proxy 进程内。与 CA 同走 env 接丝。
        if let Some(s) = sentinel {
            p.env.insert("FIXUS_GIT_SENTINEL".to_string(), s.to_string());
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
/// env 未设 / 路径空 → None;文件层(不可读 / 内容空)交给 [`read_ca_pem_from_path`]。
fn read_git_ca_pem() -> Option<String> {
    let path = std::env::var("FIXUS_GIT_CA_FILE")
        .ok()
        .filter(|s| !s.is_empty())?;
    read_ca_pem_from_path(std::path::Path::new(&path))
}

/// cr-12: 从 PEM 文件读 CA 内容。文件不可读 / 内容空 → None(helper 回退 webpki 内置根)。
/// 抽成入参为 path 的纯函数,便于单测(不碰进程级 env)。
/// 失败时 warn,便于 operator 排错(TLS 失败时能看到 CA 未加载)。
fn read_ca_pem_from_path(path: &std::path::Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(pem) if !pem.trim().is_empty() => Some(pem),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "FIXUS_GIT_CA_FILE empty; git jail uses builtin CA roots");
            None
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "FIXUS_GIT_CA_FILE unreadable; git jail uses builtin CA roots");
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

    // cr-12 CA 路径(进程唯一,避免跨 test-binary 碰撞)。仅被 path 测试 + env smoke 用,
    // 同进程内同一时刻至多一个持有(mutex 序列化)→ 无并发竞态。
    fn scratch_ca_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fixus-git-ca-test-{}.pem", std::process::id()))
    }

    // 序列化所有动 FIXUS_GIT_* 进程级 env 的测试:同一进程内至多一个持有,杜绝并行竞态
    // (此前 git_profile_injects_* 与 *_no_ca_* 互改 FIXUS_GIT_CA_FILE 在并行下 flaky)。
    static GIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lock_git_env() -> std::sync::MutexGuard<'static, ()> {
        GIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn git_profile_injects_ca_pem_when_provided() {
        // 纯 seam:行为可并行测,不碰进程 env。
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBmockPEM\n-----END CERTIFICATE-----\n";
        let g = SandboxProfile::git_inner("github.com", 443, Some(pem), None);
        assert_eq!(
            g.env.get("SANDBOX_CA_PEM").map(String::as_str),
            Some(pem),
            "CA provided → SANDBOX_CA_PEM injected verbatim"
        );
    }

    #[test]
    fn git_profile_no_ca_when_absent() {
        let g = SandboxProfile::git_inner("github.com", 443, None, None);
        assert!(!g.env.contains_key("SANDBOX_CA_PEM"), "no CA → no injection");
    }

    #[test]
    fn git_profile_injects_sentinel_when_provided() {
        // 纯 seam:sentinel 走 profile.env,与 SANDBOX_CA_PEM 同接缝。
        let g = SandboxProfile::git_inner("github.com", 443, None, Some("sentinel-XYZ"));
        assert_eq!(
            g.env.get("FIXUS_GIT_SENTINEL").map(String::as_str),
            Some("sentinel-XYZ"),
            "sentinel provided → FIXUS_GIT_SENTINEL injected verbatim"
        );
    }

    #[test]
    fn git_profile_no_sentinel_when_absent() {
        let g = SandboxProfile::git_inner("github.com", 443, None, None);
        assert!(!g.env.contains_key("FIXUS_GIT_SENTINEL"), "no sentinel → no injection");
    }

    #[test]
    fn read_ca_pem_from_path_roundtrip_missing_empty() {
        let p = scratch_ca_path();
        let _ = std::fs::remove_file(&p);
        assert!(read_ca_pem_from_path(&p).is_none(), "missing file → None");
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBmockPEM\n-----END CERTIFICATE-----\n";
        std::fs::write(&p, pem).unwrap();
        assert_eq!(read_ca_pem_from_path(&p).as_deref(), Some(pem), "valid PEM → content");
        std::fs::write(&p, "   \n ").unwrap();
        assert!(read_ca_pem_from_path(&p).is_none(), "empty file → None");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn git_reads_fixus_git_ca_file_env() {
        // env → 入参 胶水冒烟(唯一动 FIXUS_GIT_CA_FILE 的测试;mutex 序列化)。
        let _g = lock_git_env();
        let ca_path = scratch_ca_path();
        let _ = std::fs::remove_file(&ca_path);
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBenvPEM\n-----END CERTIFICATE-----\n";
        std::fs::write(&ca_path, pem).unwrap();
        std::env::set_var("FIXUS_GIT_CA_FILE", &ca_path);
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_CA_FILE");
        let _ = std::fs::remove_file(&ca_path);
        assert_eq!(
            g.env.get("SANDBOX_CA_PEM").map(String::as_str),
            Some(pem),
            "FIXUS_GIT_CA_FILE → SANDBOX_CA_PEM wiring"
        );
    }

    #[test]
    fn git_reads_fixus_git_sentinel_env() {
        // env → 入参 胶水冒烟;与其它 FIXUS_GIT_* env 测试互斥(mutex 序列化)。
        let _g = lock_git_env();
        std::env::set_var("FIXUS_GIT_SENTINEL", "env-sentinel-ABC");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_SENTINEL");
        assert_eq!(
            g.env.get("FIXUS_GIT_SENTINEL").map(String::as_str),
            Some("env-sentinel-ABC"),
            "FIXUS_GIT_SENTINEL env → profile.env wiring"
        );

        std::env::set_var("FIXUS_GIT_SENTINEL", "   ");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_SENTINEL");
        assert!(
            !g.env.contains_key("FIXUS_GIT_SENTINEL"),
            "blank sentinel → not injected"
        );
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
    fn git_profile_egress_port_overridable() {
        // 纯 seam:port 落 allowlist。
        let g = SandboxProfile::git_inner("github.com", 8443, None, None);
        assert_eq!(
            g.egress_allowlist[0].port,
            Some(8443),
            "explicit port lands in allowlist"
        );
        let g = SandboxProfile::git_inner("github.com", 443, None, None);
        assert_eq!(g.egress_allowlist[0].port, Some(443), "default port 443");
    }

    #[test]
    fn git_reads_fixus_git_egress_port_env() {
        // env → 入参 胶水冒烟:合法值覆盖;非法值回退 443。mutex 序列化(与 CA env 测试互斥)。
        let _g = lock_git_env();
        std::env::set_var("FIXUS_GIT_EGRESS_PORT", "8443");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_EGRESS_PORT");
        assert_eq!(g.egress_allowlist[0].port, Some(8443), "valid override");

        std::env::set_var("FIXUS_GIT_EGRESS_PORT", "not-a-port");
        let g = SandboxProfile::git();
        std::env::remove_var("FIXUS_GIT_EGRESS_PORT");
        assert_eq!(g.egress_allowlist[0].port, Some(443), "invalid -> fallback 443");
    }
}
