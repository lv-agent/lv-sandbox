//! Prometheus 风格 metrics。
//!
//! 暴露 `/metrics` 端点，使用 prometheus crate 的默认 registry。

use once_cell::sync::Lazy;
use prometheus::{IntCounter, IntGauge, Histogram, register_int_counter, register_int_gauge, register_histogram};

/// job 启动总数
pub static JOB_STARTED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_started_total", "total jobs started").unwrap()
});

/// job 完成总数
pub static JOB_FINISHED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_finished_total", "total jobs finished").unwrap()
});

/// job 超时总数
pub static JOB_TIMEOUT_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("sandbox_job_timeout_total", "total jobs timed out").unwrap()
});

/// 当前运行中的 job 数
pub static RUNNING_JOBS: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("sandbox_running_jobs", "currently running jobs").unwrap()
});

/// fork→exec 延迟分布（秒）
pub static FORK_EXEC_DURATION: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "sandbox_fork_exec_duration_seconds",
        "fork→exec latency",
        vec![0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1]
    ).unwrap()
});

/// cr-041: seccomp 违规总数（SIGSYS 信号 kill）
pub static JOB_SECCOMP_DENIED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "sandbox_job_seccomp_denied_total",
        "total jobs killed by seccomp (SIGSYS)"
    )
    .unwrap()
});

/// cr-041: cgroup OOM 被杀总数
pub static JOB_OOM_KILLED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "sandbox_job_oom_killed_total",
        "total jobs killed by cgroup OOM killer"
    )
    .unwrap()
});

/// cr-041: 排队等待中的 job 数（semaphore 等待者）
pub static JOB_QUEUE_DEPTH: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "sandbox_job_queue_depth",
        "jobs waiting for semaphore permit"
    )
    .unwrap()
});
