//! 配置文件系统
//!
//! YAML 格式配置文件加载、解析、验证。
//! 支持 server / sandbox / profiles 三段配置。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use sandbox_core::profile::{LandlockTemplate, ProfileRegistry, SandboxProfile};
use sandbox_core::rlimit::RlimitConfig;

// ==================== 顶层配置 ====================

/// 应用顶层配置（对应 YAML 文件）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub sandbox: SandboxSection,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
}

// ==================== Server 段 ====================

/// [server] 段配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSection {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_jobs: usize,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_log_format")]
    pub log_format: String,
    /// cr-021: 审计日志(默认关)
    #[serde(default)]
    pub audit: AuditConfig,
    /// cr-023: Bearer API key。None/缺省 = 不鉴权(默认,零行为变化)。
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_listen_addr() -> String {
    "0.0.0.0:8080".to_string()
}
fn default_max_concurrent() -> usize {
    100
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_format() -> String {
    "json".to_string()
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            max_concurrent_jobs: default_max_concurrent(),
            log_level: default_log_level(),
            log_format: default_log_format(),
            audit: AuditConfig::default(),
            api_key: None,
        }
    }
}

/// cr-021: 审计日志配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_audit_path")]
    pub path: String,
}

fn default_audit_path() -> String {
    "/var/log/sandbox/audit.jsonl".to_string()
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_audit_path(),
        }
    }
}

// ==================== Sandbox 段 ====================

/// [sandbox] 段配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSection {
    #[serde(default = "default_base_dir")]
    pub base_dir: String,
    #[serde(default = "default_disk_watermark_mb")]
    pub disk_watermark_mb: u64,
    #[serde(default = "default_default_profile")]
    pub default_profile: String,
    #[serde(default = "default_true")]
    pub fail_closed: bool,
}

fn default_base_dir() -> String {
    "/sandboxes".to_string()
}
fn default_disk_watermark_mb() -> u64 {
    1024
}
fn default_default_profile() -> String {
    "shell".to_string()
}
fn default_true() -> bool {
    true
}

impl Default for SandboxSection {
    fn default() -> Self {
        Self {
            base_dir: default_base_dir(),
            disk_watermark_mb: default_disk_watermark_mb(),
            default_profile: default_default_profile(),
            fail_closed: default_true(),
        }
    }
}

// ==================== Profile 段 ====================

/// [profiles.xxx] 段（文件友好格式，所有字段 Optional）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub rlimit: Option<RlimitFileConfig>,
    pub extra_readonly_paths: Option<Vec<String>>,
    pub max_stdout_mb: Option<u64>,
    pub max_stderr_mb: Option<u64>,
    pub default_timeout: Option<String>,
    /// cr-019: 出站白名单。空/缺省 = 零出站。
    #[serde(default)]
    pub egress_allowlist: Option<Vec<sandbox_core::egress::EgressRule>>,
    /// cr-022: 工作区聚合磁盘上限(MB)。None/缺省 = 不限(看门狗不起)。
    #[serde(default)]
    pub disk_quota_mb: Option<u64>,
}

/// rlimit 配置（文件友好，使用人类可读单位）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlimitFileConfig {
    pub cpu_seconds: Option<u64>,
    pub nofile: Option<u64>,
    pub nproc: Option<u64>,
    pub fsize_mb: Option<u64>,
    pub core: Option<u64>,
    pub stack_mb: Option<u64>,
    pub memlock: Option<u64>,
}

impl RlimitFileConfig {
    /// 转换为运行时 RlimitConfig（MB → 字节）
    pub fn to_rlimit_config(&self) -> RlimitConfig {
        let mut rlimit = RlimitConfig::new();
        if let Some(v) = self.cpu_seconds {
            rlimit = rlimit.cpu_seconds(v);
        }
        if let Some(v) = self.nofile {
            rlimit = rlimit.nofile(v);
        }
        if let Some(v) = self.nproc {
            rlimit = rlimit.nproc(v);
        }
        if let Some(v) = self.fsize_mb {
            rlimit = rlimit.fsize_mb(v);
        }
        if let Some(v) = self.core {
            rlimit = rlimit.core_disabled_with(v);
        }
        if let Some(v) = self.stack_mb {
            rlimit = rlimit.stack_mb(v);
        }
        if let Some(v) = self.memlock {
            rlimit = rlimit.memlock_with(v);
        }
        rlimit
    }
}

impl ProfileConfig {
    /// 转换为运行时 SandboxProfile
    ///
    /// 未设置的字段使用内置默认值（shell profile 为基准）
    pub fn to_profile(&self, name: &str, sandbox_section: &SandboxSection) -> Result<SandboxProfile, String> {
        // 基准默认值
        let default_profile = SandboxProfile::shell();

        // rlimit: 自定义覆盖，未设置字段用默认值
        let rlimit = if let Some(ref rl) = self.rlimit {
            rl.to_rlimit_config()
        } else {
            default_profile.rlimit.clone()
        };

        // timeout 解析
        let default_timeout = match &self.default_timeout {
            Some(t) => parse_duration(t).ok_or_else(|| format!("invalid timeout format: {t}"))?,
            None => default_profile.default_timeout,
        };

        // 输出限制
        let max_stdout_bytes = self
            .max_stdout_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(default_profile.max_stdout_bytes);
        let max_stderr_bytes = self
            .max_stderr_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(default_profile.max_stderr_bytes);

        // extra_readonly_paths
        let extra_readonly_paths: Vec<PathBuf> = self
            .extra_readonly_paths
            .as_ref()
            .map(|paths| paths.iter().map(PathBuf::from).collect())
            .unwrap_or_default();

        // landlock_template: 按名称匹配
        let landlock_template = match name {
            "shell" => LandlockTemplate::Shell,
            "python" => LandlockTemplate::Python,
            "node" => LandlockTemplate::Node,
            _ => LandlockTemplate::Custom {
                extra_readonly_paths: extra_readonly_paths.clone(),
            },
        };

        Ok(SandboxProfile {
            name: name.to_string(),
            rlimit,
            landlock_template,
            seccomp_profile: Some(sandbox_core::seccomp::SeccompProfile::default_denylist()),
            cgroup_resources: None, // 通过配置文件指定 cgroup 后续扩展
            max_stdout_bytes,
            max_stderr_bytes,
            default_timeout,
            fail_closed: sandbox_section.fail_closed,
            extra_readonly_paths,
            egress_allowlist: self.egress_allowlist.clone().unwrap_or_default(),
            disk_quota_mb: self.disk_quota_mb,
        })
    }
}

// ==================== Duration 解析 ====================

/// 解析 "5s", "100ms", "1m" 格式的 duration 字符串
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        let ms: u64 = num.parse().ok()?;
        return Some(Duration::from_millis(ms));
    }
    if let Some(num) = s.strip_suffix('s') {
        let secs: u64 = num.parse().ok()?;
        return Some(Duration::from_secs(secs));
    }
    if let Some(num) = s.strip_suffix('m') {
        let mins: u64 = num.parse().ok()?;
        return Some(Duration::from_secs(mins * 60));
    }
    let secs: u64 = s.parse().ok()?;
    Some(Duration::from_secs(secs))
}

// ==================== 文件加载 ====================

impl AppConfig {
    /// 从文件路径加载配置
    ///
    /// 文件不存在时返回默认配置（不报错）
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let path = path.as_ref();
        if !path.exists() {
            tracing::info!(path = %path.display(), "config file not found, using defaults");
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&content)?;
        tracing::info!(path = %path.display(), "config loaded");
        Ok(config)
    }

    /// 转换为 SandboxCore 的 SandboxConfig
    pub fn to_sandbox_config(&self) -> sandbox_core::sandbox_context::SandboxConfig {
        sandbox_core::sandbox_context::SandboxConfig {
            sandbox_base_dir: PathBuf::from(&self.sandbox.base_dir),
            disk_watermark_bytes: if self.sandbox.disk_watermark_mb > 0 {
                self.sandbox.disk_watermark_mb * 1024 * 1024
            } else {
                0
            },
        }
    }

    /// 将 profiles 配置注册到 ProfileRegistry
    pub fn build_profile_registry(&self) -> ProfileRegistry {
        let mut registry = ProfileRegistry::with_defaults();

        // 文件中定义的 profile 覆盖或新增
        for (name, pc) in &self.profiles {
            match pc.to_profile(name, &self.sandbox) {
                Ok(profile) => registry.register(profile),
                Err(e) => tracing::warn!(profile = %name, error = %e, "invalid profile config, skipping"),
            }
        }

        registry
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerSection::default(),
            sandbox: SandboxSection::default(),
            profiles: HashMap::new(),
        }
    }
}

/// 从配置路径构建 SandboxRunner（含 profile 注册）。
///
/// main.rs 初始化与 `/api/v1/reload` 共用此逻辑，保证两者行为一致。
/// 返回 (runner, profile_names)。
pub async fn build_runner_from_config(
    path: &str,
) -> anyhow::Result<(
    sandbox_core::sandbox_context::SandboxRunner,
    Vec<String>,
)> {
    let config = AppConfig::load_from_path(path)?;
    let sandbox_config = config.to_sandbox_config();
    let mut runner = sandbox_core::sandbox_context::SandboxRunner::new(&sandbox_config).await?;
    // cr-018+#77: 严格校验——任一 profile 无效则整体失败（fail-closed），
    // 避免配错被静默跳过导致生产 profile 缺失
    let mut errors = Vec::new();
    for (name, pc) in &config.profiles {
        match pc.to_profile(name, &config.sandbox) {
            Ok(profile) => {
                tracing::info!(profile = %name, "registered profile");
                runner.register_profile(profile);
            }
            Err(e) => {
                tracing::warn!(profile = %name, error = %e, "invalid profile config");
                errors.push(format!("{name}: {e}"));
            }
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("invalid profile config, refusing to load: {}", errors.join("; "));
    }
    let profiles = runner.profile_registry().names();
    Ok((runner, profiles))
}
