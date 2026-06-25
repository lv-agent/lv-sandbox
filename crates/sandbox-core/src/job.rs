use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// 沙箱违规事件
#[derive(Debug, Clone, Serialize)]
pub enum SandboxViolation {
    SeccompDenied { syscall: String },
    LandlockDenied { path: String, access: String },
    OomKill,
    FileSizeExceeded,
    ProcessLimitExceeded,
}

/// job 执行状态
#[derive(Debug, Clone, Serialize)]
pub enum JobStatus {
    Completed,
    TimedOut,
    Killed,
    /// cr-018: 被取消（用户主动 POST /jobs/{id}/cancel）
    Cancelled,
    /// cr-022: 工作区聚合写入超 disk_quota_mb，被看门狗收割
    DiskQuotaExceeded,
    SandboxInitFailed(String),
    Error(String),
}

/// job 执行请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub job_id: String,
    pub argv: Vec<String>,
    pub profile_name: String,
    /// None 使用 profile 中的 default_timeout
    pub timeout: Option<Duration>,
    pub custom_env: HashMap<String, String>,
    pub stdin_data: Option<Vec<u8>>,
}

/// 资源使用摘要
#[derive(Debug, Clone, Serialize)]
pub struct ResourceSummary {
    pub memory_peak_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub pids_peak: Option<u64>,
}

/// job 执行结果
#[derive(Debug, Clone, Serialize)]
pub struct JobResult {
    pub job_id: String,
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration: Duration,
    pub timed_out: bool,
    pub sandbox_violations: Vec<SandboxViolation>,
    pub resource_usage: Option<ResourceSummary>,
}

/// cr-024: 流式事件(run_job_with_cancel 在 streaming 模式经 channel 推送)。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 进程已启动(首个事件)
    Started { job_id: String },
    /// stdout 数据块(UTF-8 字符串;二进制 lossy,与现有 stdout 字符串化一致)
    Stdout { data: String },
    /// 终态结果(末事件,发完关流)
    Result(JobResult),
}
