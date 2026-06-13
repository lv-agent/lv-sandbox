//! MCP 工具的参数/返回值类型。

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_profile() -> String {
    "shell".to_string()
}

/// `sandbox_run` 工具入参
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SandboxRunParams {
    /// 命令及参数，如 ["/bin/python3", "-c", "print(42)"]
    pub argv: Vec<String>,
    /// 安全 profile：shell / python / node / 自定义
    #[serde(default = "default_profile")]
    pub profile: String,
    /// 超时，如 "30s"、"5m"。空则用 profile 默认值
    pub timeout: Option<String>,
    /// 子进程环境变量
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 传递给子进程的 stdin（UTF-8 文本）
    pub stdin: Option<String>,
    /// 唯一 job 标识。空则自动生成 UUID
    pub job_id: Option<String>,
}

/// `sandbox_reload` 工具入参
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SandboxReloadParams {
    /// 配置文件路径。空则重新加载当前配置
    pub config_path: Option<String>,
}

/// job 执行结果（对应 sandbox-server 的 SubmitResponse）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobResultInfo {
    pub job_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// worker 状态（对应 sandbox-server 的 StatusResponse）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusInfo {
    pub running_jobs: usize,
    pub max_concurrent: usize,
    pub uptime_secs: u64,
}

/// profile 列表（对应 sandbox-server 的 ProfilesResponse）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfilesInfo {
    pub profiles: Vec<String>,
}

/// reload 结果（对应 sandbox-server 的 ReloadResponse）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReloadInfo {
    pub success: bool,
    pub profiles_loaded: Vec<String>,
    pub message: String,
}
