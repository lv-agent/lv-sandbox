//! 调度器：容量检查、排队、并发控制。

use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::Semaphore;

use sandbox_core::job::{JobRequest, JobResult};
use sandbox_core::SandboxRunner;

use crate::worker::WorkerStatus;

/// 调度错误
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("运行器错误: {0}")]
    Runner(#[from] sandbox_core::CoreError),
}

/// job 调度器
///
/// 使用 `tokio::sync::Semaphore` 控制并发。
/// 超容量时 job 排队等待许可释放，而非直接拒绝。
///
/// `runner` 用 `RwLock<Arc<SandboxRunner>>` 包裹，支持热重载：
/// - `submit` 仅持有读锁克隆 `Arc` 快照后立即释放，不阻塞 `reload`。
/// - `reload` 持写锁替换内层 `Arc`，不影响正在执行的 job。
pub struct Scheduler {
    runner: Arc<RwLock<Arc<SandboxRunner>>>,
    max_concurrent_jobs: usize,
    semaphore: Arc<Semaphore>,
    start_time: Instant,
}

impl Scheduler {
    pub fn new(runner: Arc<SandboxRunner>, max_concurrent: usize) -> Self {
        Self {
            runner: Arc::new(RwLock::new(runner)),
            max_concurrent_jobs: max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            start_time: Instant::now(),
        }
    }

    /// 提交 job。
    ///
    /// 通过 Semaphore 控制并发：有空闲许可时立即执行，
    /// 否则排队等待。等待期间不消耗额外资源。
    ///
    /// 接受 `Arc<Self>` 所有权，确保 future 为 `'static`，
    /// 可安全用于 `tokio::spawn`。
    ///
    /// 执行前克隆 runner 快照并立即释放读锁，确保 reload 不会
    /// 因为长 job 而长时间阻塞。
    pub async fn submit(
        self: Arc<Self>,
        request: JobRequest,
    ) -> Result<JobResult, SchedulerError> {
        let _permit = self.semaphore.acquire().await.expect("semaphore 未关闭");

        // Metrics: job 启动 + 运行中
        crate::metrics::JOB_STARTED_TOTAL.inc();
        crate::metrics::RUNNING_JOBS.inc();

        let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

        // 克隆 runner 快照，释放读锁后执行（不阻塞 reload）
        let runner = { self.runner.read().expect("runner 锁中毒").clone() };
        let result = runner.run_job(request).await;

        // Metrics: fork→exec 耗时只计执行阶段
        timer.observe_duration();

        // Metrics: job 完成/超时 + 运行中递减
        crate::metrics::JOB_FINISHED_TOTAL.inc();
        crate::metrics::RUNNING_JOBS.dec();

        if let Ok(ref res) = result {
            if res.timed_out {
                crate::metrics::JOB_TIMEOUT_TOTAL.inc();
            }
        }

        result.map_err(SchedulerError::Runner)
    }

    /// 热重载：替换内部 Runner。
    ///
    /// 持写锁替换内层 `Arc<SandboxRunner>`。正在执行的 job
    /// 使用的是已 clone 的快照，不受影响。
    pub fn reload(&self, new_runner: Arc<SandboxRunner>) {
        let mut guard = self.runner.write().expect("runner 锁中毒");
        *guard = new_runner;
    }

    /// 当前运行 job 数
    pub fn running_count(&self) -> usize {
        self.max_concurrent_jobs - self.semaphore.available_permits()
    }

    /// 最大并发数
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent_jobs
    }

    /// 所有已注册的 profile 名
    pub fn profile_names(&self) -> Vec<String> {
        let runner = self.runner.read().expect("runner 锁中毒");
        runner.profile_registry().names()
    }

    /// Worker 状态
    pub fn worker_status(&self) -> WorkerStatus {
        let runner = self.runner.read().expect("runner 锁中毒");
        let disk_ok = runner
            .workspace_mgr()
            .check_disk_watermark()
            .unwrap_or(false);

        WorkerStatus {
            running_jobs: self.running_count(),
            max_concurrent: self.max_concurrent_jobs,
            disk_watermark_ok: disk_ok,
            capability_report: runner.capability().clone(),
            uptime: self.start_time.elapsed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use sandbox_core::profile::SandboxProfile;
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};

    async fn make_runner(base: &Path) -> SandboxRunner {
        let config = SandboxConfig {
            sandbox_base_dir: base.to_path_buf(),
            disk_watermark_bytes: 0,
        };
        SandboxRunner::new(&config).await.unwrap()
    }

    #[tokio::test]
    async fn profile_names_返回内置profile() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let mut names = scheduler.profile_names();
        names.sort();
        assert_eq!(names, vec!["node", "python", "shell"]);
    }

    #[tokio::test]
    async fn reload_替换runner后能查到新profile() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let mut new_runner = make_runner(tmp.path()).await;
        new_runner.register_profile(SandboxProfile {
            name: "my_task".to_string(),
            ..SandboxProfile::shell()
        });
        scheduler.reload(Arc::new(new_runner));

        assert!(scheduler
            .profile_names()
            .contains(&"my_task".to_string()));
    }
}
