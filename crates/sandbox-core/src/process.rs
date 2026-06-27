//! 进程生命周期管理。
//!
//! 核心：fork/exec/wait/timeout/killpg，组合所有安全机制到 pre_exec 闭包中。

use std::os::unix::io::RawFd;
use std::path::Path;

use crate::capability::CapabilityReport;
use crate::error::CoreError;
use crate::profile::LandlockTemplate;
use crate::profile::SandboxProfile;
use crate::rlimit::RlimitConfig;
use crate::workspace::WorkspaceManager;

/// pre_exec 阶段错误码。通过 pipe 传回父进程。
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PreExecError {
    SetSid = 1,
    CloseFds = 2,
    SetRlimit = 3,
    NoNewPrivs = 4,
    LandlockApply = 5,
    SeccompApply = 6,
    Chdir = 7,
}

impl PreExecError {
    /// 从 pipe 读取的字节还原
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::SetSid),
            2 => Some(Self::CloseFds),
            3 => Some(Self::SetRlimit),
            4 => Some(Self::NoNewPrivs),
            5 => Some(Self::LandlockApply),
            6 => Some(Self::SeccompApply),
            7 => Some(Self::Chdir),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

/// fork 前编译的沙箱上下文。
/// 包含所有在 pre_exec 中需要 apply 的预编译产物。
pub struct PreparedSandboxContext {
    landlock: Option<sandbox_landlock::PreparedRuleset>,
    seccomp: Option<sandbox_seccomp::PreparedFilter>,
    rlimit: RlimitConfig,
    cgroup: Option<sandbox_cgroup::JobCgroup>,
    /// cr-033: PTY slave fd（tty 模式：setsid 后设为 controlling terminal）。None = 非 tty。
    pub tty_slave_fd: Option<RawFd>,
}

impl PreparedSandboxContext {
    /// 从 SandboxProfile 编译。fork 前调用，所有内存分配在此完成。
    pub fn prepare(
        profile: &SandboxProfile,
        workspace: &Path,
        job_id: &str,
        capability: &CapabilityReport,
        workspace_mgr: &WorkspaceManager,
    ) -> Result<Self, CoreError> {
        let _ = workspace_mgr; // workspace_mgr 在 prepare 阶段暂不直接使用

        // 编译 Landlock ruleset
        let landlock = if capability.landlock.supported {
            let runtime_kind = match &profile.landlock_template {
                LandlockTemplate::Shell => Some(sandbox_landlock::RuntimeKind::Shell),
                LandlockTemplate::Python => Some(sandbox_landlock::RuntimeKind::Python),
                LandlockTemplate::Node => Some(sandbox_landlock::RuntimeKind::Node),
                LandlockTemplate::Custom { .. } => Some(sandbox_landlock::RuntimeKind::Custom),
                LandlockTemplate::Disabled => None,
            };

            match runtime_kind {
                Some(kind) => {
                    let mut policy = sandbox_landlock::FsPolicy::for_job(workspace, kind);

                    // 注入额外的共享只读路径（包目录等）
                    for path in &profile.extra_readonly_paths {
                        policy = policy.add_rule(path, sandbox_landlock::AccessFs::ReadOnly);
                    }
                    // cr-028: 额外可写路径(卷等)
                    for path in &profile.extra_writable_paths {
                        policy = policy.add_rule(path, sandbox_landlock::AccessFs::ReadWrite);
                    }

                    let prepared = sandbox_landlock::PreparedRuleset::prepare(
                        &policy,
                        &capability.landlock,
                    )?;
                    Some(prepared)
                }
                None => None,
            }
        } else if profile.fail_closed {
            return Err(CoreError::SandboxInit(
                "Landlock unavailable and profile is fail-closed".into(),
            ));
        } else {
            None
        };

        // 编译 seccomp BPF
        let seccomp = match &profile.seccomp_profile {
            Some(seccomp_profile) => {
                let prepared = sandbox_seccomp::PreparedFilter::prepare(seccomp_profile)?;
                Some(prepared)
            }
            None => None,
        };

        // cr-037: IO 限速——检测 workspace 所在块设备,填充 io.max 的 major:minor
        let mut resources = profile.cgroup_resources.clone();
        if let Some(ref mut res) = resources {
            if let Some(ref mut io) = res.io_max {
                if io.major == 0 && io.minor == 0 {
                    // sentinel: 从 workspace 路径探测实际块设备
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = workspace.metadata() {
                        let dev = meta.dev();
                        io.major = (dev >> 8) as u64;
                        io.minor = (dev & 0xff) as u64;
                    }
                }
            }
        }

        // 创建 job cgroup
        let cgroup = if let Some(resources) = &resources {
            if capability.cgroup.available {
                match sandbox_cgroup::JobCgroup::create(
                    job_id,
                    capability
                        .cgroup
                        .cgroup_path
                        .as_deref()
                        .unwrap_or(Path::new("/sys/fs/cgroup")),
                    resources,
                    &capability.cgroup,
                ) {
                    Ok(cg) => Some(cg),
                    Err(e) => {
                        if profile.fail_closed {
                            return Err(CoreError::Cgroup(e));
                        }
                        tracing::warn!(job_id = %job_id, error = %e, "cgroup creation failed, degrading");
                        None
                    }
                }
            } else if profile.fail_closed {
                return Err(CoreError::SandboxInit(
                    "cgroup v2 unavailable and profile is fail-closed".into(),
                ));
            } else {
                None
            }
        } else {
            None
        };

        // rlimit 配置直接复用（无需编译）
        let rlimit = profile.rlimit.clone();

        Ok(Self {
            landlock,
            seccomp,
            rlimit,
            cgroup,
            tty_slave_fd: None,
        })
    }

    /// 在 pre_exec 闭包中调用。只做 syscall，不分配内存。
    /// 失败时通过 error_pipe_fd 写入 PreExecError 字节，然后 _exit(1)。
    ///
    /// 执行顺序：setsid → NoNewPrivs → landlock → seccomp → rlimit → chdir → close_fds
    /// 关键：landlock/seccomp 需要创建内部 fd，必须在 rlimit 限制 nofile 之前完成。
    /// close_fds 放最后：此时所有内部 fd 已使用完毕。
    pub fn apply_in_child(&mut self, workspace: &Path, error_pipe_fd: RawFd) {
        let report_error = |code: PreExecError| {
            let buf = [code.as_byte()];
            unsafe {
                libc::write(error_pipe_fd, buf.as_ptr().cast(), 1);
                libc::_exit(1);
            }
        };

        // setsid
        if unsafe { libc::setsid() } == -1 {
            report_error(PreExecError::SetSid);
        }

        // cr-033: tty 模式——设 PTY slave 为 controlling terminal(在 close_fds 之前)
        if let Some(slave_fd) = self.tty_slave_fd {
            unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY, 0); }
        }

        // no_new_privs（landlock/seccomp 的前提）
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            report_error(PreExecError::NoNewPrivs);
        }

        // landlock（需要内部 fd，必须在 rlimit 之前；apply 会 take inner + 动态放行 /proc/<pid>）
        if let Some(ref mut landlock) = self.landlock {
            if landlock.apply().is_err() {
                report_error(PreExecError::LandlockApply);
            }
        }

        // seccomp（内部已调用 PR_SET_NO_NEW_PRIVS，双重调用无害）
        if let Some(ref seccomp) = self.seccomp {
            if seccomp.apply().is_err() {
                report_error(PreExecError::SeccompApply);
            }
        }

        // rlimit（限制 nofile 等，必须在 landlock/seccomp 之后）
        if self.rlimit.apply().is_err() {
            report_error(PreExecError::SetRlimit);
        }

        // chdir
        if std::env::set_current_dir(workspace).is_err() {
            report_error(PreExecError::Chdir);
        }

        // 关闭非必要 fd（放最后：landlock/seccomp 内部 fd 已在 apply 中使用完毕）
        if crate::fd::close_unneeded_fds(&[error_pipe_fd]).is_err() {
            report_error(PreExecError::CloseFds);
        }
    }

    /// 获取 cgroup 引用（用于父进程迁移子进程）
    pub fn cgroup(&self) -> Option<&sandbox_cgroup::JobCgroup> {
        self.cgroup.as_ref()
    }

    /// 提取 cgroup 所有权（用于父进程迁移 + 销毁）
    pub fn take_cgroup(&mut self) -> Option<sandbox_cgroup::JobCgroup> {
        self.cgroup.take()
    }

    /// 消耗 self，销毁 cgroup（清理时调用）
    pub fn destroy(self) -> Result<(), CoreError> {
        if let Some(cg) = self.cgroup {
            cg.destroy()?;
        }
        Ok(())
    }
}
