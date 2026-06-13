use std::io::Read;

use libseccomp::{ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall};

use crate::error::SeccompError;
use crate::profile::{CompareOperator, SeccompAction as OurAction, SeccompProfile};

/// 预编译的 seccomp BPF 程序。
///
/// `prepare()` 阶段使用 libseccomp 编译 filter 并导出 BPF 字节码。
/// `apply()` 阶段通过 `prctl(PR_SET_NO_NEW_PRIVS)` + `seccomp(SECCOMP_SET_MODE_FILTER)`
/// 加载字节码，只做 syscall，不依赖 libseccomp 上下文对象（Send + Sync 安全）。
pub struct PreparedFilter {
    bpf_program: Vec<libc::sock_filter>,
}

unsafe impl Send for PreparedFilter {}
unsafe impl Sync for PreparedFilter {}

impl PreparedFilter {
    /// 从 SeccompProfile 编译 BPF 程序。
    /// 此方法会进行内存分配，必须在 fork 前调用。
    pub fn prepare(profile: &SeccompProfile) -> Result<Self, SeccompError> {
        let default_action = our_action_to_scmp(profile.default_action());

        let mut ctx = ScmpFilterContext::new_filter(default_action)
            .map_err(|e| SeccompError::FilterCreate(format!("{e}")))?;

        // 设置 NO_NEW_PRIVS
        ctx.set_ctl_nnp(true)
            .map_err(|e| SeccompError::FilterCreate(format!("set_ctl_nnp: {e}")))?;

        // 添加每条规则
        for rule in profile.rules() {
            let syscall = match syscall_to_scmp(&rule.syscall) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("跳过未知 syscall {:?}: {}", rule.syscall, e);
                    continue;
                }
            };

            let action = our_action_to_scmp(rule.action);

            if rule.conditions.is_empty() {
                ctx.add_rule(action, syscall).map_err(|e| {
                    SeccompError::RuleAdd(format!(
                        "添加规则 {:?} -> {:?} 失败: {}",
                        rule.syscall, rule.action, e
                    ))
                })?;
            } else {
                let comparators: Vec<ScmpArgCompare> = rule
                    .conditions
                    .iter()
                    .map(|c| condition_to_comparator(c))
                    .collect();

                ctx.add_rule_conditional(action, syscall, &comparators)
                    .map_err(|e| {
                        SeccompError::RuleAdd(format!(
                            "添加条件规则 {:?} -> {:?} 失败: {}",
                            rule.syscall, rule.action, e
                        ))
                    })?;
            }
        }

        // 导出 BPF 字节码（通过 pipe）
        let (read_fd, write_fd) = nix::unistd::pipe()
            .map_err(|e| SeccompError::FilterCreate(format!("pipe: {e}")))?;

        // 写端：导出 BPF
        {
            let mut write_file = std::fs::File::from(write_fd);
            ctx.export_bpf(&mut write_file)
                .map_err(|e| SeccompError::FilterCreate(format!("export_bpf: {e}")))?;
        }

        // 读端：读取字节码
        let mut bpf_bytes = Vec::new();
        {
            let mut read_file = std::fs::File::from(read_fd);
            read_file
                .read_to_end(&mut bpf_bytes)
                .map_err(|e| SeccompError::FilterCreate(format!("read bpf: {e}")))?;
        }

        let bpf_program = bytes_to_sock_filters(&bpf_bytes)?;

        Ok(Self { bpf_program })
    }

    /// 应用 seccomp filter 到当前进程。
    /// 必须在 pre_exec 闭包中调用（fork 后、exec 前）。
    /// 只做 syscall：prctl(PR_SET_NO_NEW_PRIVS) + seccomp(SECCOMP_SET_MODE_FILTER)。
    pub fn apply(&self) -> Result<(), SeccompError> {
        let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if ret < 0 {
            return Err(SeccompError::Load(
                "prctl(PR_SET_NO_NEW_PRIVS) 失败".into(),
            ));
        }

        let prog = libc::sock_fprog {
            len: self.bpf_program.len() as u16,
            filter: self.bpf_program.as_ptr() as *mut libc::sock_filter,
        };

        let ret = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                1u32, // SECCOMP_SET_MODE_FILTER
                0u32,
                &prog,
            )
        };

        if ret < 0 {
            return Err(SeccompError::Load(format!(
                "seccomp(SECCOMP_SET_MODE_FILTER) 失败: {}",
                std::io::Error::last_os_error()
            )));
        }

        Ok(())
    }
}

/// 将导出的 BPF 字节码转为 `sock_filter` 结构体数组
fn bytes_to_sock_filters(bytes: &[u8]) -> Result<Vec<libc::sock_filter>, SeccompError> {
    // sock_filter: code(u16) + jt(u8) + jf(u8) + k(u32) = 8 字节
    const SF_SIZE: usize = 8;
    if bytes.len() % SF_SIZE != 0 {
        return Err(SeccompError::FilterCreate(format!(
            "BPF 字节码长度 {} 不是 {} 的倍数",
            bytes.len(),
            SF_SIZE
        )));
    }

    let count = bytes.len() / SF_SIZE;
    let mut filters = Vec::with_capacity(count);

    for chunk in bytes.chunks_exact(SF_SIZE) {
        let sf = libc::sock_filter {
            code: u16::from_ne_bytes([chunk[0], chunk[1]]),
            jt: chunk[2],
            jf: chunk[3],
            k: u32::from_ne_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
        };
        filters.push(sf);
    }

    Ok(filters)
}

/// 我们的 SeccompAction → libseccomp ScmpAction
fn our_action_to_scmp(action: OurAction) -> ScmpAction {
    match action {
        OurAction::Allow => ScmpAction::Allow,
        OurAction::KillProcess => ScmpAction::KillProcess,
        OurAction::KillThread => ScmpAction::KillThread,
        OurAction::Trap => ScmpAction::Trap,
        OurAction::Errno(e) => ScmpAction::Errno(e as i32),
        OurAction::Log => ScmpAction::Log,
    }
}

/// 我们的 Syscall → libseccomp ScmpSyscall
fn syscall_to_scmp(syscall: &crate::syscall::Syscall) -> Result<ScmpSyscall, String> {
    let name = match syscall {
        crate::syscall::Syscall::Mount => "mount",
        crate::syscall::Syscall::Umount2 => "umount2",
        crate::syscall::Syscall::Ptrace => "ptrace",
        crate::syscall::Syscall::ProcessVmReadv => "process_vm_readv",
        crate::syscall::Syscall::ProcessVmWritev => "process_vm_writev",
        crate::syscall::Syscall::Bpf => "bpf",
        crate::syscall::Syscall::Keyctl => "keyctl",
        crate::syscall::Syscall::AddKey => "add_key",
        crate::syscall::Syscall::RequestKey => "request_key",
        crate::syscall::Syscall::Reboot => "reboot",
        crate::syscall::Syscall::KexecLoad => "kexec_load",
        crate::syscall::Syscall::InitModule => "init_module",
        crate::syscall::Syscall::FinitModule => "finit_module",
        crate::syscall::Syscall::DeleteModule => "delete_module",
        crate::syscall::Syscall::Swapon => "swapon",
        crate::syscall::Syscall::Swapoff => "swapoff",
        crate::syscall::Syscall::Sethostname => "sethostname",
        crate::syscall::Syscall::Setdomainname => "setdomainname",
        crate::syscall::Syscall::Setns => "setns",
        crate::syscall::Syscall::Unshare => "unshare",
        crate::syscall::Syscall::Personality => "personality",
        crate::syscall::Syscall::Iopl => "iopl",
        crate::syscall::Syscall::Ioperm => "ioperm",
        crate::syscall::Syscall::PerfEventOpen => "perf_event_open",
        crate::syscall::Syscall::Userfaultfd => "userfaultfd",
        crate::syscall::Syscall::IoUringSetup => "io_uring_setup",
        crate::syscall::Syscall::IoUringEnter => "io_uring_enter",
        crate::syscall::Syscall::IoUringRegister => "io_uring_register",
        crate::syscall::Syscall::Clone => "clone",
        crate::syscall::Syscall::Clone3 => "clone3",
        crate::syscall::Syscall::Custom(name) => name,
    };

    ScmpSyscall::from_name(name).map_err(|e| format!("syscall '{name}': {e}"))
}

/// 我们的 SeccompCondition → libseccomp ScmpArgCompare
fn condition_to_comparator(cond: &crate::profile::SeccompCondition) -> ScmpArgCompare {
    let op = match cond.operator {
        CompareOperator::Equal => ScmpCompareOp::Equal,
        CompareOperator::NotEqual => ScmpCompareOp::NotEqual,
        CompareOperator::MaskedEqual => ScmpCompareOp::MaskedEqual(cond.mask.unwrap_or(0)),
    };

    ScmpArgCompare::new(cond.arg_index as u32, op, cond.value)
}
