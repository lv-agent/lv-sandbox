//! MCP tool parameter / return types.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_profile() -> String {
    "shell".to_string()
}

/// `sandbox_run` tool parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SandboxRunParams {
    /// Command and arguments, e.g. ["/bin/python3", "-c", "print(42)"]
    pub argv: Vec<String>,
    /// Security profile: shell / python / node / custom
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Timeout, e.g. "30s", "5m". Empty uses the profile default.
    pub timeout: Option<String>,
    /// Child process environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// stdin (UTF-8 text) piped to the child process.
    pub stdin: Option<String>,
    /// Unique job id. Empty auto-generates a UUID.
    pub job_id: Option<String>,
}

/// `sandbox_reload` tool parameters.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SandboxReloadParams {
    /// Config file path. Empty reloads the current config.
    pub config_path: Option<String>,
}

/// Job result (mirrors sandbox-server's SubmitResponse).
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

/// Worker status (mirrors sandbox-server's StatusResponse).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusInfo {
    pub running_jobs: usize,
    pub max_concurrent: usize,
    pub uptime_secs: u64,
}

/// Profile list (mirrors sandbox-server's ProfilesResponse).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfilesInfo {
    pub profiles: Vec<String>,
}

/// Reload result (mirrors sandbox-server's ReloadResponse).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReloadInfo {
    pub success: bool,
    pub profiles_loaded: Vec<String>,
    pub message: String,
}
