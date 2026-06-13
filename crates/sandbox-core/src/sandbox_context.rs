//! 沙箱运行器：组合所有安全机制，管理 job 生命周期。

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::capability::CapabilityReport;
use crate::env::build_sanitized_env;
use crate::error::CoreError;
use crate::job::{JobRequest, JobResult, JobStatus};
use crate::process::PreparedSandboxContext;
use crate::profile::{ProfileRegistry, SandboxProfile};
use crate::workspace::WorkspaceManager;

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

    /// 执行单个 job。完整生命周期管理。
    pub async fn run_job(&self, request: JobRequest) -> Result<JobResult, CoreError> {
        let start = tokio::time::Instant::now();

        // 0. 磁盘水位检查
        if !self.workspace_mgr.check_disk_watermark()? {
            return Err(CoreError::Workspace(
                "磁盘剩余空间低于水位线，拒绝新 job".to_string(),
            ));
        }

        // 1. 查找 profile
        let profile = self
            .profile_registry
            .get(&request.profile_name)
            .ok_or_else(|| CoreError::ProfileNotFound(request.profile_name.clone()))?
            .clone();
        let timeout = request.timeout.unwrap_or(profile.default_timeout);

        // 2. 构建 sanitized env
        let job_workspace = self.workspace_mgr.create_job_workspace(&request.job_id)?;
        let env = build_sanitized_env(
            &request.job_id,
            &job_workspace.root,
            &request.custom_env,
        );

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
            return Err(CoreError::Process("pipe2 创建失败".into()));
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
            // pre_exec 失败：子进程已 _exit
            let error = crate::process::PreExecError::from_byte(error_buf[0]);
            let _ = child.wait().await;
            let _ = self.workspace_mgr.cleanup_job(&request.job_id);
            if let Some(cg) = cgroup {
                let _ = cg.destroy();
            }
            return Err(CoreError::Process(format!(
                "pre_exec 失败: {:?}",
                error
            )));
        }

        // 9. 迁移进程到 cgroup
        if let Some(ref cg) = cgroup {
            if let Err(e) = cg.migrate_process(child_pid as u32) {
                tracing::warn!(job_id = %request.job_id, error = %e, "cgroup 迁移失败");
            }
        }

        // 10. 写入 stdin 数据（如果有）
        if let Some(stdin_data) = request.stdin_data {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                let _ = stdin_pipe.write_all(&stdin_data).await;
                let _ = stdin_pipe.shutdown().await;
            }
        }

        // 11. 取出 stdout/stderr pipe，并发读取
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout_pipe {
                let _ = out.read_to_end(&mut buf).await;
            }
            if buf.len() > max_stdout {
                buf.truncate(max_stdout);
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

        // 12. 等待子进程退出（带超时）
        let (timed_out, exit_status) = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => (false, Some(status)),
            Ok(Err(e)) => return Err(CoreError::Process(e.to_string())),
            Err(_) => {
                // 超时：先 SIGTERM 整个进程组
                unsafe {
                    libc::killpg(child_pid, libc::SIGTERM);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                // 仍然没退出则 SIGKILL
                let _ = child.kill().await;
                let status = child.wait().await.ok();
                (true, status)
            }
        };

        let duration = start.elapsed();

        // 13. 收集输出
        let stdout = stdout_handle.await.unwrap_or_default();
        let stderr = stderr_handle.await.unwrap_or_default();

        // 14. 确定状态
        let status = if timed_out {
            JobStatus::TimedOut
        } else {
            JobStatus::Completed
        };

        // 15. 清理 cgroup + workspace
        if let Some(cg) = cgroup {
            let _ = cg.destroy();
        }
        let _ = self.workspace_mgr.cleanup_job(&request.job_id);

        // 16. 构建结果
        Ok(JobResult {
            job_id: request.job_id,
            status,
            exit_code: exit_status.as_ref().and_then(|s| s.code()),
            signal: exit_status.as_ref().and_then(|s| s.signal()),
            stdout,
            stderr,
            duration,
            timed_out,
            sandbox_violations: vec![],
            resource_usage: None,
        })
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
