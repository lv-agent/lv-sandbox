//! Worker 容量与状态管理。

use std::time::Duration;

use sandbox_core::CapabilityReport;

/// Worker 状态
pub struct WorkerStatus {
    pub running_jobs: usize,
    pub max_concurrent: usize,
    pub disk_watermark_ok: bool,
    pub capability_report: CapabilityReport,
    pub uptime: Duration,
}
