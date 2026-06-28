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
    #[error("runner error: {0}")]
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
    #[error("job not found")]
    NotFound,
    #[error("job already finished, cannot cancel")]
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
    /// cr-021: 审计日志(默认 noop)
    audit: Arc<crate::audit::AuditLogger>,
    /// cr-031: 生命周期 webhook(默认 noop)
    webhooks: Arc<crate::webhook::WebhookDispatcher>,
}

impl Scheduler {
    pub fn new(runner: Arc<SandboxRunner>, max_concurrent: usize) -> Self {
        Self {
            runner: Arc::new(RwLock::new(runner)),
            max_concurrent_jobs: max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            start_time: Instant::now(),
            jobs: Arc::new(RwLock::new(HashMap::new())),
            audit: Arc::new(crate::audit::AuditLogger::noop()),
            webhooks: Arc::new(crate::webhook::WebhookDispatcher::noop()),
        }
    }

    /// cr-021: 注入审计 logger(builder,main 用)。cr-026: 收 Arc 以便与 SessionManager 共享。
    pub fn with_audit(mut self, logger: Arc<crate::audit::AuditLogger>) -> Self {
        self.audit = logger;
        self
    }

    /// cr-031: 注入 webhook 分发器(builder,main 用)。
    pub fn with_webhooks(mut self, w: Arc<crate::webhook::WebhookDispatcher>) -> Self {
        self.webhooks = w;
        self
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
        crate::metrics::JOB_QUEUE_DEPTH.inc();
        let _permit = self.semaphore.acquire().await.expect("semaphore closed");
        crate::metrics::JOB_QUEUE_DEPTH.dec();

        // Metrics: job 启动 + 运行中
        crate::metrics::JOB_STARTED_TOTAL.inc();
        crate::metrics::RUNNING_JOBS.inc();

        let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

        // 克隆 runner 快照，释放读锁后执行（不阻塞 reload）
        let runner = { self.runner.read().expect("runner lock poisoned").clone() };
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
            // cr-041: 违规计数
            for v in &res.sandbox_violations {
                match v {
                    sandbox_core::job::SandboxViolation::SeccompDenied { .. } => {
                        crate::metrics::JOB_SECCOMP_DENIED_TOTAL.inc();
                    }
                    sandbox_core::job::SandboxViolation::OomKill => {
                        crate::metrics::JOB_OOM_KILLED_TOTAL.inc();
                    }
                    _ => {}
                }
            }
        }

        result.map_err(SchedulerError::Runner)
    }

    /// 热重载：替换内部 Runner。
    ///
    /// 持写锁替换内层 `Arc<SandboxRunner>`。正在执行的 job
    /// 使用的是已 clone 的快照，不受影响。
    pub fn reload(&self, new_runner: Arc<SandboxRunner>) {
        let mut guard = self.runner.write().expect("runner lock poisoned");
        *guard = new_runner;
    }

    /// cr-018: 异步提交。注册 job 表项 + 后台执行。立即返回 job_id。
    pub async fn submit_async(&self, request: JobRequest) -> String {
        let job_id = request.job_id.clone();
        let profile = request.profile_name.clone();
        let argv = request.argv.clone();
        let cancel = CancellationToken::new();
        self.jobs.write().expect("jobs lock poisoned").insert(
            job_id.clone(),
            JobEntry {
                cancel: cancel.clone(),
                state: JobState::Running,
                created_at: Instant::now(),
            },
        );

        // cr-021: 审计 Started(提交即记)
        self.audit.log(crate::audit::AuditEvent::new(
            crate::audit::AuditEventType::JobStarted,
            &job_id,
            &profile,
            argv.clone(),
            None,
            None,
            None,
            None,
        ));

        // cr-018: 后台执行（runner 快照 + 共享 jobs 表 + semaphore）
        let runner = {
            self.runner.read().expect("runner lock poisoned").clone()
        };
        let semaphore = self.semaphore.clone();
        let jobs = self.jobs.clone();
        let audit = self.audit.clone();
        let webhooks = self.webhooks.clone();
        let jid = job_id.clone();
        tokio::spawn(async move {
            crate::metrics::JOB_STARTED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.inc();
            crate::metrics::JOB_QUEUE_DEPTH.inc();
            let _permit = semaphore.acquire().await.expect("semaphore closed");
            crate::metrics::JOB_QUEUE_DEPTH.dec();
            let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

            let result = runner.run_job_with_cancel(request, cancel, None).await;

            timer.observe_duration();
            crate::metrics::JOB_FINISHED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.dec();
            if let Ok(ref res) = result {
                if res.timed_out {
                    crate::metrics::JOB_TIMEOUT_TOTAL.inc();
                }
                // cr-041: 违规计数
                for v in &res.sandbox_violations {
                    match v {
                        sandbox_core::job::SandboxViolation::SeccompDenied { .. } => {
                            crate::metrics::JOB_SECCOMP_DENIED_TOTAL.inc();
                        }
                        sandbox_core::job::SandboxViolation::OomKill => {
                            crate::metrics::JOB_OOM_KILLED_TOTAL.inc();
                        }
                        _ => {}
                    }
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

            // cr-021: 审计终态
            // cr-021 审计终态 + cr-031 webhook(同一终态事件)
            let ev = crate::audit::AuditEvent::new(
                crate::audit::status_to_event_type(&result.status),
                &jid,
                &profile,
                argv.clone(),
                result.exit_code,
                result.signal,
                Some(result.duration.as_millis() as u64),
                crate::audit::status_detail(&result.status),
            );
            webhooks.dispatch(&ev);
            audit.log(ev);

            if let Some(entry) = jobs.write().expect("jobs lock poisoned").get_mut(&jid) {
                entry.state = JobState::Done(result);
            }
        });

        job_id
    }

    /// cr-024: 流式提交。返回 stdout/事件 receiver;job 仍入 jobs 表(可 cancel/get_job)。
    ///
    /// 后台任务与 submit_async 同(semaphore/metrics/audit),差别仅传 `Some(tx)` 给
    /// run_job_with_cancel。result 同时经 channel(Result 事件)与 jobs 表(Done)两路给到客户端。
    pub async fn submit_streaming(
        &self,
        request: JobRequest,
    ) -> tokio::sync::mpsc::Receiver<sandbox_core::job::StreamEvent> {
        let job_id = request.job_id.clone();
        let profile = request.profile_name.clone();
        let argv = request.argv.clone();
        let cancel = CancellationToken::new();
        let (tx, rx) =
            tokio::sync::mpsc::channel::<sandbox_core::job::StreamEvent>(64);
        self.jobs.write().expect("jobs lock poisoned").insert(
            job_id.clone(),
            JobEntry {
                cancel: cancel.clone(),
                state: JobState::Running,
                created_at: Instant::now(),
            },
        );
        self.audit.log(crate::audit::AuditEvent::new(
            crate::audit::AuditEventType::JobStarted,
            &job_id,
            &profile,
            argv.clone(),
            None,
            None,
            None,
            None,
        ));

        let runner = {
            self.runner.read().expect("runner lock poisoned").clone()
        };
        let semaphore = self.semaphore.clone();
        let jobs = self.jobs.clone();
        let audit = self.audit.clone();
        let webhooks = self.webhooks.clone();
        let jid = job_id.clone();
        tokio::spawn(async move {
            crate::metrics::JOB_STARTED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.inc();
            crate::metrics::JOB_QUEUE_DEPTH.inc();
            let _permit = semaphore.acquire().await.expect("semaphore closed");
            crate::metrics::JOB_QUEUE_DEPTH.dec();
            let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

            // cr-024: 传 Some(tx),run_job 边读边推 stdout + 终态 Result
            let result = runner.run_job_with_cancel(request, cancel, Some(tx)).await;

            timer.observe_duration();
            crate::metrics::JOB_FINISHED_TOTAL.inc();
            crate::metrics::RUNNING_JOBS.dec();
            if let Ok(ref res) = result {
                if res.timed_out {
                    crate::metrics::JOB_TIMEOUT_TOTAL.inc();
                }
                // cr-041: 违规计数
                for v in &res.sandbox_violations {
                    match v {
                        sandbox_core::job::SandboxViolation::SeccompDenied { .. } => {
                            crate::metrics::JOB_SECCOMP_DENIED_TOTAL.inc();
                        }
                        sandbox_core::job::SandboxViolation::OomKill => {
                            crate::metrics::JOB_OOM_KILLED_TOTAL.inc();
                        }
                        _ => {}
                    }
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

            // cr-021 审计终态 + cr-031 webhook(同一终态事件)
            let ev = crate::audit::AuditEvent::new(
                crate::audit::status_to_event_type(&result.status),
                &jid,
                &profile,
                argv.clone(),
                result.exit_code,
                result.signal,
                Some(result.duration.as_millis() as u64),
                crate::audit::status_detail(&result.status),
            );
            webhooks.dispatch(&ev);
            audit.log(ev);

            if let Some(entry) = jobs.write().expect("jobs lock poisoned").get_mut(&jid) {
                entry.state = JobState::Done(result);
            }
            // tx 随任务结束 drop → channel 关 → receiver 流尾
        });

        rx
    }

    /// cr-018: 查询 job 状态/结果
    pub fn get_job(&self, job_id: &str) -> Option<JobState> {
        self.jobs
            .read()
            .expect("jobs lock poisoned")
            .get(job_id)
            .map(|e| e.state.clone())
    }

    /// cr-018: 取消 job
    pub fn cancel_job(&self, job_id: &str) -> Result<(), CancelError> {
        let jobs = self.jobs.read().expect("jobs lock poisoned");
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
        let runner = self.runner.read().expect("runner lock poisoned");
        runner.profile_registry().names()
    }

    /// cr-018+#77: 查询某个 profile（dry-run 用，返回克隆）
    pub fn get_profile(&self, name: &str) -> Option<sandbox_core::profile::SandboxProfile> {
        let runner = self.runner.read().expect("runner lock poisoned");
        runner.profile_registry().get(name).cloned()
    }

    /// Worker 状态
    pub fn worker_status(&self) -> WorkerStatus {
        let runner = self.runner.read().expect("runner lock poisoned");
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
    async fn profile_names_returns_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let mut names = scheduler.profile_names();
        names.sort();
        assert_eq!(names, vec!["node", "python", "shell"]);
    }

    #[tokio::test]
    async fn reload_swaps_runner_new_profile_visible() {
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
    async fn submit_async_done_after_complete() {
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
            "job should finish -> Done, actual: {:?}",
            state
        );
    }

    #[tokio::test]
    async fn cancel_job_stops_running_as_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let jid = scheduler
            .submit_async(make_async_request("cancel-001", &["/bin/sleep", "30"]))
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        scheduler.cancel_job(&jid).expect("cancel should succeed");

        let state = wait_until_done(&scheduler, &jid).await;
        assert!(
            matches!(
                state,
                Some(JobState::Done(ref r)) if matches!(r.status, sandbox_core::job::JobStatus::Cancelled)
            ),
            "after cancel should be Done(Cancelled), actual: {:?}",
            state
        );
    }

    #[tokio::test]
    async fn cancel_job_done_returns_already_done() {
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

    /// cr-019 gap2(回归覆盖): 带 egress allowlist 的 job 被 cancel 时,
    /// 代理随 cleanup 正确停止——job 应在轮询窗口内到达 Cancelled(若
    /// cleanup/proxy.stop 挂起,wait_until_done 会超时返回 Running → 断言失败)。
    /// 注:gap1 的 JobProxy Drop 是泄漏兜底;本测试锁定 cancel+proxy 集成路径。
    #[tokio::test]
    async fn cancel_job_with_allowlist_stops_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let mut runner = make_runner(tmp.path()).await;
        runner.register_profile(SandboxProfile {
            name: "egress_shell".to_string(),
            egress_allowlist: vec![sandbox_core::egress::EgressRule {
                host: "localhost".to_string(),
                port: None,
            }],
            ..SandboxProfile::shell()
        });
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let req = JobRequest {
            job_id: "cancel-egress-001".to_string(),
            argv: vec!["/bin/sleep".to_string(), "30".to_string()],
            profile_name: "egress_shell".to_string(),
            timeout: Some(Duration::from_secs(30)),
            custom_env: HashMap::new(),
            stdin_data: None,
        };
        let jid = scheduler.submit_async(req).await;
        // 让代理起好 + 子进程跑起来
        tokio::time::sleep(Duration::from_millis(300)).await;
        scheduler.cancel_job(&jid).expect("cancel should succeed");

        let state = wait_until_done(&scheduler, &jid).await;
        assert!(
            matches!(
                state,
                Some(JobState::Done(ref r)) if matches!(r.status, sandbox_core::job::JobStatus::Cancelled)
            ),
            "after cancel should be Done(Cancelled), actual: {:?}",
            state
        );
    }

    /// cr-021: 接线后,file logger 记录 Started + Completed 两条 JSONL。
    #[tokio::test]
    async fn audit_records_started_and_completed_events() {
        let tmp = tempfile::tempdir().unwrap();
        let audit_path = tmp.path().join("audit.jsonl");
        let logger = Arc::new(crate::audit::AuditLogger::file(&audit_path).unwrap());

        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10).with_audit(logger);

        scheduler
            .submit_async(make_async_request("audit-001", &["/bin/echo", "hello"]))
            .await;
        let state = wait_until_done(&scheduler, "audit-001").await;
        assert!(matches!(state, Some(JobState::Done(_))));

        let content = std::fs::read_to_string(&audit_path).unwrap();
        let events: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(events.len(), 2, "should have Started + Completed: {content}");
        assert_eq!(events[0]["event_type"], "JobStarted");
        assert_eq!(events[1]["event_type"], "JobCompleted");
        assert_eq!(events[1]["job_id"], "audit-001");
        assert!(
            events[1]["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "hello"),
            "argv should contain hello: {content}"
        );
    }

    // ==================== cr-022 gap: 异步路径 + 审计映射覆盖 ====================

    /// 构造带 1MB 配额的 runner(nproc 放开 + fsize 抬高,见 disk_quota_tests 注释)。
    async fn make_quota_runner(base: &Path) -> SandboxRunner {
        let mut runner = make_runner(base).await;
        let mut rlimit = sandbox_core::profile::SandboxProfile::shell().rlimit;
        rlimit.nproc = None;
        rlimit.fsize_bytes = Some(1024 * 1024 * 1024);
        runner.register_profile(sandbox_core::profile::SandboxProfile {
            name: "quota".to_string(),
            disk_quota_mb: Some(1),
            rlimit,
            ..sandbox_core::profile::SandboxProfile::shell()
        });
        runner
    }

    fn quota_write_request(job_id: &str) -> JobRequest {
        JobRequest {
            job_id: job_id.to_string(),
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "yes | head -c 200000000 > big; /bin/sleep 5".into(),
            ],
            profile_name: "quota".into(),
            timeout: Some(Duration::from_secs(10)),
            custom_env: HashMap::new(),
            stdin_data: None,
        }
    }

    /// cr-022 gap: scheduler 异步路径(submit_async → run_job_with_cancel)透出
    /// DiskQuotaExceeded。核心 disk_quota_tests 直跑 run_job,绕过调度器;本测试补全该路径。
    #[tokio::test]
    async fn quota_exceeded_surfaces_via_async_path() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_quota_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let jid = scheduler.submit_async(quota_write_request("async-quota-001")).await;
        let state = wait_until_done(&scheduler, &jid).await;
        assert!(
            matches!(
                state,
                Some(JobState::Done(ref r)) if matches!(
                    r.status,
                    sandbox_core::job::JobStatus::DiskQuotaExceeded
                )
            ),
            "async quota-exceeded job should be Done(DiskQuotaExceeded), actual: {:?}",
            state
        );
    }

    /// cr-022 gap: 审计映射 DiskQuotaExceeded → JobKilled + detail="disk quota exceeded"
    /// (cr-021 的 audit 测试只覆盖 Started/Completed;新映射分支在此锁定)。
    #[tokio::test]
    async fn audit_logs_disk_quota_exceeded_as_jobkilled() {
        let tmp = tempfile::tempdir().unwrap();
        let audit_path = tmp.path().join("audit.jsonl");
        let logger = Arc::new(crate::audit::AuditLogger::file(&audit_path).unwrap());

        let runner = make_quota_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10).with_audit(logger);

        scheduler
            .submit_async(quota_write_request("audit-quota-001"))
            .await;
        let state = wait_until_done(&scheduler, "audit-quota-001").await;
        assert!(matches!(state, Some(JobState::Done(_))));

        let content = std::fs::read_to_string(&audit_path).unwrap();
        let events: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(events.len(), 2, "Started + terminal: {content}");
        assert_eq!(events[0]["event_type"], "JobStarted");
        assert_eq!(
            events[1]["event_type"], "JobKilled",
            "DiskQuotaExceeded should map to JobKilled: {content}"
        );
        assert_eq!(events[1]["detail"], "disk quota exceeded");
    }

    // ==================== cr-024: 流式 submit_streaming ====================

    /// cr-024: submit_streaming 的 receiver 依序收 Started + stdout + Result,且 job 跑完 get_job=Done。
    #[tokio::test]
    async fn submit_streaming_emits_events_and_registers_job() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        let mut rx = scheduler
            .submit_streaming(make_async_request("stream-001", &["/bin/echo", "hi"]))
            .await;
        let mut got_started = false;
        let mut got_result = false;
        let mut stdout = String::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                sandbox_core::job::StreamEvent::Started { job_id } => {
                    assert_eq!(job_id, "stream-001");
                    got_started = true;
                }
                sandbox_core::job::StreamEvent::Stdout { data } => stdout.push_str(&data),
                sandbox_core::job::StreamEvent::Result(_) => got_result = true,
            }
        }
        assert!(got_started, "missing Started");
        assert!(got_result, "missing Result");
        assert!(stdout.contains("hi"), "stdout: {stdout:?}");

        let state = wait_until_done(&scheduler, "stream-001").await;
        assert!(
            matches!(state, Some(JobState::Done(_))),
            "streamed job should register as Done, got {state:?}"
        );
    }

    /// cr-031: 配 webhook 的 scheduler,job 终态时 POST 事件到 webhook URL。
    #[tokio::test]
    async fn webhook_fires_on_job_terminal() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_partial_json(serde_json::json!({"event_type": "JobCompleted"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let wh = crate::webhook::WebhookDispatcher::new(vec![format!("{}/hook", server.uri())]);
        let scheduler = Scheduler::new(Arc::new(runner), 10).with_webhooks(Arc::new(wh));

        scheduler
            .submit_async(make_async_request("wh-001", &["/bin/echo", "hi"]))
            .await;
        let _ = wait_until_done(&scheduler, "wh-001").await;
        // fire-and-forget;给投递一点时间再校验
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        server.verify().await;
    }

    // ==================== cr-041: queue depth + violation metrics ====================

    /// 排队时 queue_depth gauge > 0。
    #[tokio::test]
    async fn queue_depth_gauge_positive_while_waiting() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        // max_concurrent=1: 第二个 job 必排队
        let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 1));

        // 先提交长任务占用唯一的槽(sleep 2s, wait_until_done 最多等 5s)
        scheduler
            .submit_async(make_async_request("qd-slow", &["/bin/sleep", "2"]))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 再提交第二个任务(排队中)
        scheduler
            .submit_async(make_async_request("qd-fast", &["/bin/echo", "hi"]))
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 此时第二个任务应仍在队列中(第一个还在 sleep)
        let depth = crate::metrics::JOB_QUEUE_DEPTH.get();
        assert!(depth >= 1, "queue_depth should be >= 1, got {depth}");

        // 等两个任务完成
        wait_until_done(&scheduler, "qd-fast").await;
        wait_until_done(&scheduler, "qd-slow").await;

        // 任务全完成后 queue_depth 应收敛到 0
        let depth = crate::metrics::JOB_QUEUE_DEPTH.get();
        assert_eq!(depth, 0, "queue_depth should be 0 after all done, got {depth}");
    }

    /// seccomp 违规触发 counter 递增。
    #[tokio::test]
    async fn seccomp_denied_counter_increments_via_async_path() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(tmp.path()).await;
        let scheduler = Scheduler::new(Arc::new(runner), 10);

        // unshare(2) 在 seccomp default_denylist → SIGSYS → SeccompDenied 违规
        let req = JobRequest {
            job_id: "sec-sched-001".to_string(),
            argv: vec!["/usr/bin/unshare".into(), "-r".into(), "/bin/true".into()],
            profile_name: "shell".to_string(),
            timeout: Some(Duration::from_secs(5)),
            custom_env: HashMap::new(),
            stdin_data: None,
        };
        scheduler.submit_async(req).await;
        wait_until_done(&scheduler, "sec-sched-001").await;

        // 读 counter
        let metric_families = prometheus::gather();
        for mf in &metric_families {
            if mf.get_name() == "sandbox_job_seccomp_denied_total" {
                let val = mf.get_metric()[0].get_counter().get_value();
                assert!(val >= 1.0, "seccomp_denied counter should be >= 1, got {val}");
                return;
            }
        }
        panic!("sandbox_job_seccomp_denied_total not found in registered metrics");
    }
}
