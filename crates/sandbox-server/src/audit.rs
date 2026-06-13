//! 审计日志模块
//!
//! 记录 job 生命周期关键事件（启动、完成、超时、失败），
//! 以 JSONL 格式写入文件或通过 tracing 输出。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

/// 审计事件类型
#[derive(Debug, Clone, Serialize)]
pub enum AuditEventType {
    JobStarted,
    JobCompleted,
    JobTimeout,
    JobFailed,
}

/// 单条审计事件
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event_type: AuditEventType,
    pub job_id: String,
    pub profile: String,
    pub detail: Option<String>,
    pub duration_ms: Option<u64>,
}

/// 审计日志记录器
pub struct AuditLogger {
    writer: Mutex<AuditWriter>,
}

enum AuditWriter {
    File(File),
    Noop,
}

impl AuditLogger {
    /// 创建文件审计日志（JSONL 格式，每行一条 JSON）
    pub fn file(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: Mutex::new(AuditWriter::File(file)),
        })
    }

    /// 空日志（丢弃所有事件，用于测试或禁用审计）
    pub fn noop() -> Self {
        Self {
            writer: Mutex::new(AuditWriter::Noop),
        }
    }

    /// 记录一条审计事件
    pub fn log(&self, event: AuditEvent) {
        let mut guard = self.writer.lock().unwrap();
        match &mut *guard {
            AuditWriter::File(file) => {
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = writeln!(file, "{}", json);
                }
            }
            AuditWriter::Noop => {}
        }

        // 同时通过 tracing 输出（structured log）
        tracing::info!(
            event_type = ?event.event_type,
            job_id = %event.job_id,
            profile = %event.profile,
            duration_ms = ?event.duration_ms,
            detail = ?event.detail,
            "audit"
        );
    }
}
