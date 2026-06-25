//! 审计日志模块(cr-021:接入 job 生命周期)。
//!
//! 记录 job 生命周期事件(started / completed / timed_out / killed / cancelled /
//! failed),JSONL 文件 + tracing 双写。配置驱动,默认 noop(静默)。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

use sandbox_core::job::JobStatus;

/// 审计事件类型(对齐 JobStatus 的 6 终态 + Started)。
#[derive(Debug, Clone, Serialize)]
pub enum AuditEventType {
    JobStarted,
    JobCompleted,
    JobTimedOut,
    JobKilled,
    JobCancelled,
    JobFailed,
}

/// 单条审计事件(JSONL 每行一条,自包含:Started 与终态都带 argv)。
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event_type: AuditEventType,
    pub job_id: String,
    pub profile: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub duration_ms: Option<u64>,
    pub detail: Option<String>,
}

impl AuditEvent {
    /// 构造事件(自动填 ISO8601 UTC timestamp)。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_type: AuditEventType,
        job_id: impl Into<String>,
        profile: impl Into<String>,
        argv: Vec<String>,
        exit_code: Option<i32>,
        signal: Option<i32>,
        duration_ms: Option<u64>,
        detail: Option<String>,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event_type,
            job_id: job_id.into(),
            profile: profile.into(),
            argv,
            exit_code,
            signal,
            duration_ms,
            detail,
        }
    }
}

/// job 终态 → 审计事件类型(无损:6 种 JobStatus 全覆盖)。
pub fn status_to_event_type(status: &JobStatus) -> AuditEventType {
    match status {
        JobStatus::Completed => AuditEventType::JobCompleted,
        JobStatus::TimedOut => AuditEventType::JobTimedOut,
        JobStatus::Killed => AuditEventType::JobKilled,
        JobStatus::Cancelled => AuditEventType::JobCancelled,
        // cr-022: 复用 JobKilled 事件类型(不膨胀);detail 写明 disk quota。
        JobStatus::DiskQuotaExceeded => AuditEventType::JobKilled,
        JobStatus::Error(_) | JobStatus::SandboxInitFailed(_) => AuditEventType::JobFailed,
    }
}

/// 从 JobStatus 提取 detail(Error/SandboxInitFailed 的 message)。
pub fn status_detail(status: &JobStatus) -> Option<String> {
    match status {
        JobStatus::Error(m) | JobStatus::SandboxInitFailed(m) => Some(m.clone()),
        // cr-022
        JobStatus::DiskQuotaExceeded => Some("disk quota exceeded".to_string()),
        _ => None,
    }
}

/// 审计日志记录器。
pub struct AuditLogger {
    writer: Mutex<AuditWriter>,
}

enum AuditWriter {
    File(File),
    Noop,
}

impl AuditLogger {
    /// 文件审计(JSONL,append)。
    pub fn file(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Mutex::new(AuditWriter::File(file)),
        })
    }

    /// 静默审计(不写文件、不 tracing)——默认关时用。
    pub fn noop() -> Self {
        Self {
            writer: Mutex::new(AuditWriter::Noop),
        }
    }

    /// 记录一条事件。File:写 JSONL + tracing;Noop:静默。
    pub fn log(&self, event: AuditEvent) {
        let mut guard = self.writer.lock().unwrap();
        match &mut *guard {
            AuditWriter::File(file) => {
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = writeln!(file, "{}", json);
                }
                tracing::info!(
                    event_type = ?event.event_type,
                    job_id = %event.job_id,
                    profile = %event.profile,
                    duration_ms = ?event.duration_ms,
                    detail = ?event.detail,
                    "audit"
                );
            }
            AuditWriter::Noop => {}
        }
    }
}
