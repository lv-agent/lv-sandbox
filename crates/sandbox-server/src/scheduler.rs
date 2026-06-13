//! 调度器：容量检查、排队、并发控制。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use sandbox_core::job::{JobRequest, JobResult};
use sandbox_core::SandboxRunner;

use crate::worker::WorkerStatus;

/// 调度错误
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("运行器错误: {0}")]
    Runner(#[from] sandbox_core::CoreError),
}

/// cr-018: job 运行时状态
#[derive(Debug, Clone)]
pub enum JobState {
    Running,
    Done(JobResult),
}

/// cr-018: job 表项
pub struct JobEntry {
    pub cancel: CancellationToken,
    pub state: JobState,
    pub created_at: Instant,
}

/// cr-018: cancel 错误
#[derive(Debug, thiserror::Error)]
pub enum CancelError {
    #[error("任务不存在")]
    NotFound,
    #[error("任务已完成，无法取消")]
    AlreadyDone,
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
    /// cr-018: job 运行时表（job_id → entry）
    jobs: Arc<RwLock<HashMap<String, JobEntry>>>,
}

impl Scheduler {
    pub fn new(runner: Arc<SandboxRunner>, max_concurrent: usize) -> Self {
        Self {
            runner: Arc::new(RwLock::new(runner)),
            max_concurrent_jobs: max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            start_time: Instant::now(),
            jobs: Arc::new(RwLock::new(HashMap::new())),
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

    /// cr-018: 异步提交。注册 job 表项 + 后台执行。立即返回 job_id。
    pub async fn submit_async(&self, request: JobRequest) -> String {
        let job_id = request.job_id.clone();
        let cancel = CancellationToken::new();
        self.jobs.write().expect("jobs 锁中毒").insert(
            job_id.clone(),
            JobEntry {
                cancel: cancel.clone(),
                state: JobState::Running,
                created_at: Instant::now(),
            },
        );

        // cr-018: 后台执行（runner 快照 + 共享 jobs 表 + semaphore）
        let runner = {
            self.runner.read().expect("runner 锁中毒").clone()
        };
        let semaphore = self.semaphore.clone();
        let jobs = self.jobs.clone();
        let jid = job_id.clone();
        tokio::spawn(async move {
            crate::metrics::JOB_STARTED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.inc();
            let _permit = semaphore.acquire().await.expect("semaphore 未关闭");
            let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

            let result = runner.run_job_with_cancel(request, cancel).await;

            timer.observe_duration();
            crate::metrics::JOB_FINISHED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.dec();
            if let Ok(ref res) = result {
                if res.timed_out {
                    crate::metrics::JOB_TIMEOUT_TOTAL.inc();
                }
            }

            let result = result.unwrap_or_else(|e| sandbox_core::job::JobResult {
                job_id: jid.clone(),
                status: sandbox_core::job::JobStatus::Error(e.to_string()),
                exit_code: None,
                signal: None,
                stdout: Vec::new(),
                stderr: e.to_string().into_bytes(),
                duration: std::time::Duration::ZERO,
                timed_out: false,
                sandbox_violations: vec![],
                resource_usage: None,
            });
            if let Some(entry) = jobs.write().expect("jobs 锁中毒").get_mut(&jid) {
                entry.state = JobState::Done(result);
            }
        });

        job_id
    }

    /// cr-018: 查询 job 状态/结果
    pub fn get_job(&self, job_id: &str) -> Option<JobState> {
        self.jobs
            .read()
            .expect("jobs 锁中毒")
            .get(job_id)
            .map(|e| e.state.clone())
    }

    /// cr-018: 取消 job
    pub fn cancel_job(&self, job_id: &str) -> Result<(), CancelError> {
        let jobs = self.jobs.read().expect("jobs 锁中毒");
        match jobs.get(job_id) {
            Some(e) if matches!(e.state, JobState::Running) => {
                e.cancel.cancel();
                Ok(())
            }
            Some(_) => Err(CancelError::AlreadyDone),
            None => Err(CancelError::NotFound),
        }
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
    use sandbox_core::job::JobRequest;
    use std::collections::HashMap;
    use std::time::Duration;

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

    // ==================== cr-018 阶段2: 异步 submit + cancel ====================

    fn make_async_request(job_id: &str, argv: &[&str]) -> JobRequest {
        JobRequest {
            job_id: job_id.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            profile_name: "shell".to_string(),
            timeout: Some(Duration::from_secs(10)),
            custom_env: HashMap::new(),
            stdin_data: None,
        }
    }

    /// 轮询直到 job 进入终态或超时（测试辅助）
    async fn wait_until_done(scheduler: &Scheduler, job_id: &str) -> Option<JobState> {
        for _ in 0..50 {
            if let Some(state) = scheduler.get_job(job_id) {
                if matches!(state, JobState::Done(_)) {
                    return Some(state);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        scheduler.get_job(job_id)
    }

    #[tokio::test]
    async fn submit_async_注册任务_完成后get_job返回done() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let jid = scheduler
            .submit_async(make_async_request("async-001", &["/bin/echo", "hello"]))
            .await;
        assert_eq!(jid, "async-001");
        assert!(matches!(scheduler.get_job(&jid), Some(JobState::Running)));

        let state = wait_until_done(&scheduler, &jid).await;
        assert!(
            matches!(state, Some(JobState::Done(_))),
            "任务应完成 → Done，实际: {:?}",
            state
        );
    }

    #[tokio::test]
    async fn cancel_job_停止运行中任务为cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let jid = scheduler
            .submit_async(make_async_request("cancel-001", &["/bin/sleep", "30"]))
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        scheduler.cancel_job(&jid).expect("cancel 应成功");

        let state = wait_until_done(&scheduler, &jid).await;
        assert!(
            matches!(
                state,
                Some(JobState::Done(ref r)) if matches!(r.status, sandbox_core::job::JobStatus::Cancelled)
            ),
            "cancel 后应 Done(Cancelled)，实际: {:?}",
            state
        );
    }

    #[tokio::test]
    async fn cancel_job_已完成返回alreadydone() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let jid = scheduler
            .submit_async(make_async_request("done-001", &["/bin/echo", "x"]))
            .await;
        wait_until_done(&scheduler, &jid).await;
        assert!(matches!(
            scheduler.cancel_job(&jid),
            Err(CancelError::AlreadyDone)
        ));
    }
}
