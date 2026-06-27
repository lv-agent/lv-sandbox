//! 沙箱运行器：组合所有安全机制，管理 job 生命周期。

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::capability::CapabilityReport;
use crate::env::build_sanitized_env;
use crate::error::CoreError;
use crate::job::{JobRequest, JobResult, JobStatus, ResourceSummary, SandboxViolation, StreamEvent};
use crate::process::PreparedSandboxContext;
use crate::profile::{ProfileRegistry, SandboxProfile};
use crate::workspace::WorkspaceManager;

/// cr-022: 磁盘配额看门狗轮询间隔。两次轮询间的突发写最多超出 interval × 写速,
/// 由 rlimit fsize(单文件封顶)收窄该窗口。
const DISK_QUOTA_POLL: Duration = Duration::from_millis(250);

/// 沙箱运行器配置
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub sandbox_base_dir: PathBuf,
    pub disk_watermark_bytes: u64,
}

/// 沙箱运行器
pub struct SandboxRunner {
    workspace_mgr: WorkspaceManager,
    capability: CapabilityReport,
    profile_registry: ProfileRegistry,
}

impl SandboxRunner {
    /// 初始化：检测能力，创建工作空间管理器，加载默认 profile。
    pub async fn new(config: &SandboxConfig) -> Result<Self, CoreError> {
        let capability = CapabilityReport::detect();
        let workspace_mgr =
            WorkspaceManager::new(&config.sandbox_base_dir, config.disk_watermark_bytes);

        // 确保基础目录存在
        std::fs::create_dir_all(&config.sandbox_base_dir)?;

        let profile_registry = ProfileRegistry::with_defaults();

        Ok(Self {
            workspace_mgr,
            capability,
            profile_registry,
        })
    }

    /// 执行单个 job。完整生命周期管理。（cr-018: 委托 run_job_with_cancel，cancel 永不触发）
    pub async fn run_job(&self, request: JobRequest) -> Result<JobResult, CoreError> {
        self.run_job_with_cancel(request, tokio_util::sync::CancellationToken::new(), None)
            .await
    }

    /// cr-018: 带 cancel 的任务执行(一次性:水位准入 → 查 profile → 建工作区 → 执行 → 清理)。
    pub async fn run_job_with_cancel(
        &self,
        request: JobRequest,
        cancel: tokio_util::sync::CancellationToken,
        // cr-024: 流式 stdout sink。None = 不流式(默认,read_to_end)。
        stdout_sink: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<JobResult, CoreError> {
        // 0. 磁盘水位准入
        if !self.workspace_mgr.check_disk_watermark()? {
            return Err(CoreError::Workspace(
                "disk free below watermark, rejecting new job".to_string(),
            ));
        }
        // 1. 查找 profile
        let profile = self
            .profile_registry
            .get(&request.profile_name)
            .ok_or_else(|| CoreError::ProfileNotFound(request.profile_name.clone()))?
            .clone();
        // 2. 建一次性工作区
        let job_id = request.job_id.clone();
        let workspace = self.workspace_mgr.create_job_workspace(&request.job_id)?;
        // 3. 执行(cr-026: 提取的原语 run_in_workspace,不建/不清理工作区)
        let result = self
            .run_in_workspace(&workspace, &profile, request, cancel, stdout_sink)
            .await;
        // 4. 清理(无论成败)
        let _ = self.workspace_mgr.cleanup_job(&job_id);
        result
    }

    /// cr-026: 执行原语——在**给定工作区**里跑一条命令,套全套安全约束(landlock/seccomp/
    /// cgroup/timeout/cancel/quota/stream),但**不创建/不清理工作区**(工作区生命周期由调用者
    /// 管:一次性 run_job_with_cancel 建与清;会话 exec 用持久工作区)。profile 由参数传入(会话建时绑)。
    pub async fn run_in_workspace(
        &self,
        job_workspace: &crate::workspace::JobWorkspace,
        profile: &SandboxProfile,
        request: JobRequest,
        cancel: tokio_util::sync::CancellationToken,
        stdout_sink: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<JobResult, CoreError> {
        let start = tokio::time::Instant::now();
        let timeout = request.timeout.unwrap_or(profile.default_timeout);

        // sanitized env(profile.env baseline)
        let mut env = build_sanitized_env(
            &request.job_id,
            &job_workspace.root,
            &profile.env,
            &request.custom_env,
        );

        // cr-019: 若 profile 有出站白名单,起 per-job SOCKS5h 代理(在 workspace 内 bind UDS)
        let job_proxy = if !profile.egress_allowlist.is_empty() {
            let matcher = crate::egress::AllowlistMatcher::new(profile.egress_allowlist.clone());
            match crate::proxy::JobProxy::start(&job_workspace.workspace, matcher).await {
                Ok((proxy, sock_path)) => {
                    env.insert(
                        "SANDBOX_PROXY_SOCK".to_string(),
                        sock_path.to_string_lossy().to_string(),
                    );
                    Some(proxy)
                }
                Err(e) => {
                    tracing::warn!(
                        job_id = %request.job_id,
                        error = %e,
                        "proxy start failed, job will have zero egress"
                    );
                    None
                }
            }
        } else {
            None
        };

        // 3. 准备安全上下文（编译 landlock/seccomp/cgroup）
        let mut prepared_ctx = PreparedSandboxContext::prepare(
            &profile,
            &job_workspace.workspace,
            &request.job_id,
            &self.capability,
            &self.workspace_mgr,
        )?;

        // 提取 cgroup（父进程负责迁移 + 销毁，不进 pre_exec 闭包）
        let cgroup = prepared_ctx.take_cgroup();

        let workspace_path = job_workspace.workspace.clone();
        let max_stdout = profile.max_stdout_bytes as usize;
        let max_stderr = profile.max_stderr_bytes as usize;

        // 4. 创建 error pipe（CLOEXEC：exec 成功后写端自动关闭）
        let mut pipe_fds: [libc::c_int; 2] = [-1, -1];
        if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(CoreError::Process("pipe2 creation failed".into()));
        }
        let error_read_fd = pipe_fds[0];
        let error_write_fd = pipe_fds[1];

        // 5. 构建子进程命令
        let mut cmd = tokio::process::Command::new(&request.argv[0]);
        if request.argv.len() > 1 {
            cmd.args(&request.argv[1..]);
        }
        cmd.env_clear()
            .envs(&env)
            .stdin(if request.stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // pre_exec: 完整安全管道（setsid + fd清理 + rlimit + NoNewPrivs + landlock + seccomp + chdir）
        unsafe {
            cmd.pre_exec(move || {
                prepared_ctx.apply_in_child(&workspace_path, error_write_fd);
                Ok(())
            });
        }

        // 6. 启动子进程
        let mut child = cmd.spawn().map_err(|e| CoreError::Process(e.to_string()))?;
        let child_pid = child.id().unwrap_or(0) as i32;

        // 7. 关闭 error pipe 写端（父进程不需要）
        unsafe {
            libc::close(error_write_fd);
        }

        // 8. 检查 pre_exec 错误
        let mut error_buf = [0u8; 1];
        let bytes_read = unsafe { libc::read(error_read_fd, error_buf.as_mut_ptr().cast(), 1) };
        unsafe {
            libc::close(error_read_fd);
        }

        if bytes_read > 0 {
            // pre_exec 失败：子进程已 _exit（工作区由调用者清理:一次性 wrapper / 会话 destroy）
            let error = crate::process::PreExecError::from_byte(error_buf[0]);
            let _ = child.wait().await;
            if let Some(cg) = cgroup {
                let _ = cg.destroy();
            }
            return Err(CoreError::Process(format!(
                "pre_exec failed: {:?}",
                error
            )));
        }

        // 9. 迁移进程到 cgroup
        if let Some(ref cg) = cgroup {
            if let Err(e) = cg.migrate_process(child_pid as u32) {
                tracing::warn!(job_id = %request.job_id, error = %e, "cgroup migration failed");
            }
        }

        // 10. 写入 stdin 数据（如果有）
        if let Some(stdin_data) = request.stdin_data {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                let _ = stdin_pipe.write_all(&stdin_data).await;
                let _ = stdin_pipe.shutdown().await;
            }
        }

        // cr-024: 流式模式首个事件(Started)。sink 为 None 时此 if 不执行。
        if let Some(ref sink) = stdout_sink {
            let _ = sink
                .send(StreamEvent::Started {
                    job_id: request.job_id.clone(),
                })
                .await;
        }

        // 11. 取出 stdout/stderr pipe，并发读取（stderr 不流式,只进终态结果）
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let task_sink = stdout_sink.clone();
        let stdout_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout_pipe {
                match task_sink {
                    Some(sink) => {
                        // cr-024: 分块读,每块发 sink + 累计(累计仍用于 max_stdout 截断 + 终态 stdout)
                        let mut chunk = vec![0u8; 8192];
                        loop {
                            match out.read(&mut chunk).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let data = chunk[..n].to_vec();
                                    let room = max_stdout.saturating_sub(buf.len());
                                    if room > 0 {
                                        buf.extend_from_slice(&data[..room.min(data.len())]);
                                    }
                                    let _ = sink
                                        .send(StreamEvent::Stdout {
                                            data: String::from_utf8_lossy(&data).into_owned(),
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                    None => {
                        let _ = out.read_to_end(&mut buf).await;
                        if buf.len() > max_stdout {
                            buf.truncate(max_stdout);
                        }
                    }
                }
            }
            buf
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut err) = stderr_pipe {
                let _ = err.read_to_end(&mut buf).await;
            }
            if buf.len() > max_stderr {
                buf.truncate(max_stderr);
            }
            buf
        });

        // 12. 等待子进程退出（超时 / cancel / cr-022 磁盘配额超限）
        let quota_bytes = profile.disk_quota_mb.map(|mb| mb * 1024 * 1024);

        // cr-022: 配额看门狗。disk_quota_mb 为 None 时 pending 永不触发(零开销、零行为变化)。
        // 有值时每 DISK_QUOTA_POLL 测一次工作区聚合大小(spawn_blocking,不阻塞 executor),
        // 超限即返回,触发 select! 的收割分支。
        let quota_watch = async {
            let quota = match quota_bytes {
                Some(q) => q,
                None => {
                    std::future::pending::<()>().await;
                    return;
                }
            };
            let mut interval = tokio::time::interval(DISK_QUOTA_POLL);
            loop {
                interval.tick().await;
                let dir = job_workspace.root.clone();
                let size = tokio::task::spawn_blocking(move || crate::workspace::dir_size(&dir))
                    .await
                    .unwrap_or(0);
                if size > quota {
                    return;
                }
            }
        };

        let (timed_out, cancelled, disk_quota_exceeded, exit_status) = tokio::select! {
            r = tokio::time::timeout(timeout, child.wait()) => match r {
                Ok(Ok(status)) => (false, false, false, Some(status)),
                Ok(Err(e)) => return Err(CoreError::Process(e.to_string())),
                Err(_) => {
                    // 超时：SIGTERM → 500ms → SIGKILL
                    unsafe { libc::killpg(child_pid, libc::SIGTERM); }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let _ = child.kill().await;
                    (true, false, false, child.wait().await.ok())
                }
            },
            _ = cancel.cancelled() => {
                // cr-018 cancel：SIGTERM → 500ms → SIGKILL（整组，无孤儿）
                unsafe { libc::killpg(child_pid, libc::SIGTERM); }
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = child.kill().await;
                (false, true, false, child.wait().await.ok())
            }
            // cr-022: 配额超限 → SIGTERM → 500ms → SIGKILL（整组，无孤儿）
            _ = quota_watch => {
                unsafe { libc::killpg(child_pid, libc::SIGTERM); }
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = child.kill().await;
                (false, false, true, child.wait().await.ok())
            }
        };

        let duration = start.elapsed();

        // 13. 收集输出
        let stdout = stdout_handle.await.unwrap_or_default();
        let stderr = stderr_handle.await.unwrap_or_default();

        // 14. 确定状态
        // - cancel → Cancelled（cr-018，用户主动）
        // - 超时 → TimedOut
        // - cr-022 磁盘配额超限 → DiskQuotaExceeded
        // - 被信号杀死（如 seccomp SIGSYS 违规、外部信号）→ Killed
        // - 正常退出（含非零退出码）→ Completed
        let status = if cancelled {
            JobStatus::Cancelled
        } else if timed_out {
            JobStatus::TimedOut
        } else if disk_quota_exceeded {
            JobStatus::DiskQuotaExceeded
        } else if exit_status.as_ref().and_then(|s| s.signal()).is_some() {
            JobStatus::Killed
        } else {
            JobStatus::Completed
        };

        // cr-022: 配额超限标注原因（启用预埋的 FileSizeExceeded 钩子）
        let sandbox_violations = if disk_quota_exceeded {
            vec![SandboxViolation::FileSizeExceeded]
        } else {
            vec![]
        };

        // 15. 读资源使用 + 清理 cgroup + 代理
        let resource_usage = if let Some(ref cg) = cgroup {
            cg.resource_usage().ok().map(|u| ResourceSummary {
                memory_peak_bytes: u.memory_peak,
                cpu_usage_usec: u.cpu_usage_usec,
                pids_peak: u.pids_current,
            })
        } else {
            None
        };
        if let Some(cg) = cgroup {
            let _ = cg.destroy();
        }
        // cr-019: 停止代理(cancel + 清理 socket 文件)
        if let Some(proxy) = job_proxy {
            proxy.stop().await;
        }

        // 16. 构建结果
        let result = JobResult {
            job_id: request.job_id,
            status,
            exit_code: exit_status.as_ref().and_then(|s| s.code()),
            signal: exit_status.as_ref().and_then(|s| s.signal()),
            stdout,
            stderr,
            duration,
            timed_out,
            sandbox_violations,
            resource_usage,
        };
        // cr-024: 流式模式末事件(Result),发完 sender drop → channel 关 → handler 流尾
        if let Some(sink) = stdout_sink {
            let _ = sink.send(StreamEvent::Result(result.clone())).await;
        }
        Ok(result)
    }

    /// 注册自定义 profile
    pub fn register_profile(&mut self, profile: SandboxProfile) {
        self.profile_registry.register(profile);
    }

    /// 查询能力
    pub fn capability(&self) -> &CapabilityReport {
        &self.capability
    }

    /// 查询 workspace 管理器
    pub fn workspace_mgr(&self) -> &WorkspaceManager {
        &self.workspace_mgr
    }

    /// 查询 profile 注册表
    pub fn profile_registry(&self) -> &ProfileRegistry {
        &self.profile_registry
    }
}
