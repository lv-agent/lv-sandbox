//! 崩溃恢复：启动时扫描残留 workspace，清理孤儿 job。
//!
//! 流程：扫描 base_dir 下所有子目录 → 判断是否残留 → 清理。
//! cgroup 中的残留进程由 cgroup auto-cleanup 处理。

use serde::Serialize;

use crate::error::CoreError;
use crate::sandbox_context::SandboxRunner;

/// 恢复报告
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryReport {
    /// 扫描到的残留 job 数
    pub scanned: usize,
    /// 成功清理的 job 数
    pub cleaned: usize,
    /// 清理失败的 job 数
    pub errors: usize,
}

/// 执行崩溃恢复
///
/// 扫描 base_dir 下所有子目录，清理残留的 workspace。
/// 正常完成的 job 在运行时已被清理，剩余的都是崩溃残留。
pub fn recover(runner: &SandboxRunner) -> Result<RecoveryReport, CoreError> {
    let mgr = runner.workspace_mgr();
    let jobs = mgr.list_jobs()?;

    let mut report = RecoveryReport {
        scanned: jobs.len(),
        cleaned: 0,
        errors: 0,
    };

    for job_id in &jobs {
        match mgr.cleanup_job(job_id) {
            Ok(()) => {
                tracing::info!(job_id = %job_id, "recovery cleanup: stale workspace removed");
                report.cleaned += 1;
            }
            Err(e) => {
                tracing::warn!(job_id = %job_id, error = %e, "recovery cleanup failed");
                report.errors += 1;
            }
        }
    }

    if report.scanned > 0 {
        tracing::info!(
            scanned = report.scanned,
            cleaned = report.cleaned,
            errors = report.errors,
            "crash recovery complete"
        );
    }

    Ok(report)
}
