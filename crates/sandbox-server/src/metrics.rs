//! Prometheus 风格 metrics。
//!
//! 暴露 `/metrics` 端点，使用 prometheus crate 的默认 registry。

use once_cell::sync::Lazy;
use prometheus::{IntCounter, IntGauge, Histogram, register_int_counter, register_int_gauge, register_histogram};

/// job 启动总数
pub static JOB_STARTED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_started_total", "job 启动总数").unwrap()
});

/// job 完成总数
pub static JOB_FINISHED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_finished_total", "job 完成总数").unwrap()
});

/// job 超时总数
pub static JOB_TIMEOUT_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_timeout_total", "job 超时总数").unwrap()
});

/// 当前运行中的 job 数
pub static RUNNING_JOBS: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("sandbox_running_jobs", "当前运行中的 job 数").unwrap()
});

/// fork→exec 延迟分布（秒）
pub static FORK_EXEC_DURATION: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "sandbox_fork_exec_duration_seconds",
        "fork→exec 延迟",
        vec![0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1]
    ).unwrap()
});
