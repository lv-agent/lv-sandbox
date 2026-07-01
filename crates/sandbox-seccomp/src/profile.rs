use crate::syscall::Syscall;

/// seccomp 动作
#[derive(Debug, Clone, Copy)]
pub enum SeccompAction {
    Allow,
    KillProcess,
    KillThread,
    Trap,
    Errno(u32),
    Log,
}

/// 比较操作符
#[derive(Debug, Clone, Copy)]
pub enum CompareOperator {
    Equal,
    NotEqual,
    MaskedEqual,
}

/// seccomp 条件（用于 clone flag 过滤）
#[derive(Debug, Clone)]
pub struct SeccompCondition {
    pub arg_index: usize,
    pub operator: CompareOperator,
    pub value: u64,
    pub mask: Option<u64>,
}

/// 单条 seccomp 规则
#[derive(Debug, Clone)]
pub struct SeccompRule {
    pub syscall: Syscall,
    pub action: SeccompAction,
    pub conditions: Vec<SeccompCondition>,
}

/// seccomp profile 构建器
#[derive(Debug, Clone)]
pub struct SeccompProfile {
    pub(crate) default_action: SeccompAction,
    pub(crate) rules: Vec<SeccompRule>,
}

impl SeccompProfile {
    /// 创建 denylist profile：默认允许，拒绝指定 syscall
    pub fn denylist() -> Self {
        Self {
            default_action: SeccompAction::Allow,
            rules: Vec::new(),
        }
    }

    /// 创建 allowlist profile：默认拒绝，允许指定 syscall
    pub fn allowlist() -> Self {
        Self {
            default_action: SeccompAction::KillProcess,
            rules: Vec::new(),
        }
    }

    /// 沙箱默认 denylist：允许所有 syscall，拒绝已知的危险 syscall。
    ///
    /// 覆盖：文件系统挂载、进程追踪、BPF、keyring、内核模块、
    /// 系统控制、namespace 操作、I/O 端口、perf、io_uring 等。
    pub fn default_denylist() -> Self {
        Self::denylist()
            // 文件系统挂载
            .deny(Syscall::Mount)
            .deny(Syscall::Umount2)
            // 进程追踪 / 内存读写
            .deny(Syscall::Ptrace)
            .deny(Syscall::ProcessVmReadv)
            .deny(Syscall::ProcessVmWritev)
            // BPF / eBPF
            .deny(Syscall::Bpf)
            // Keyring
            .deny(Syscall::Keyctl)
            .deny(Syscall::AddKey)
            .deny(Syscall::RequestKey)
            // 系统控制
            .deny(Syscall::Reboot)
            .deny(Syscall::KexecLoad)
            // 内核模块
            .deny(Syscall::InitModule)
            .deny(Syscall::FinitModule)
            .deny(Syscall::DeleteModule)
            // 交换空间
            .deny(Syscall::Swapon)
            .deny(Syscall::Swapoff)
            // 网络标识
            .deny(Syscall::Sethostname)
            .deny(Syscall::Setdomainname)
            // Namespace
            .deny(Syscall::Setns)
            .deny(Syscall::Unshare)
            // 杂项危险
            .deny(Syscall::Personality)
            .deny(Syscall::Iopl)
            .deny(Syscall::Ioperm)
            .deny(Syscall::PerfEventOpen)
            .deny(Syscall::Userfaultfd)
            // io_uring
            .deny(Syscall::IoUringSetup)
            .deny(Syscall::IoUringEnter)
            .deny(Syscall::IoUringRegister)
            // 网络 socket API（cr-016 默认禁网）
            .deny_network()
    }

    /// cr-045: shell 运行时 allowlist profile(default KillProcess + 白名单)。
    /// 与 `default_denylist()` 并存,opt-in(由 ProfileConfig.seccomp_mode 选择)。
    pub fn default_allowlist_shell() -> Self {
        crate::allowlist::shell()
    }

    /// cr-045 Phase 2: python 运行时 allowlist profile(default KillProcess + 白名单)。
    pub fn default_allowlist_python() -> Self {
        crate::allowlist::python()
    }

    /// cr-045 Phase 3: node 运行时 allowlist profile(default KillProcess + 白名单)。
    pub fn default_allowlist_node() -> Self {
        crate::allowlist::node()
    }

    /// 添加拒绝规则
    pub fn deny(mut self, syscall: Syscall) -> Self {
        self.rules.push(SeccompRule {
            syscall,
            action: SeccompAction::KillProcess,
            conditions: Vec::new(),
        });
        self
    }

    /// 添加带条件的拒绝规则（用于 clone namespace flag 过滤）
    pub fn deny_with_conditions(
        mut self,
        syscall: Syscall,
        conditions: Vec<SeccompCondition>,
    ) -> Self {
        self.rules.push(SeccompRule {
            syscall,
            action: SeccompAction::KillProcess,
            conditions,
        });
        self
    }

    /// 添加带条件的允许规则（allowlist 模式用，如 socket(AF_UNIX) 放行）。
    /// cr-045：对称于 `deny_with_conditions`，action = Allow。
    pub fn allow_with_conditions(
        mut self,
        syscall: Syscall,
        conditions: Vec<SeccompCondition>,
    ) -> Self {
        self.rules.push(SeccompRule {
            syscall,
            action: SeccompAction::Allow,
            conditions,
        });
        self
    }

    /// cr-045: 覆盖默认 action(builder,诊断/高级用)。
    pub fn with_default_action(mut self, action: SeccompAction) -> Self {
        self.default_action = action;
        self
    }

    /// 添加 errno 规则(syscall 返回 -1/errno 不杀进程;用于能力探测如 io_uring,
    /// 让 libuv/glibc 探测失败回退,而非 KILL)。cr-045 P3。
    pub fn errno(mut self, syscall: Syscall, errno: u32) -> Self {
        self.rules.push(SeccompRule {
            syscall,
            action: SeccompAction::Errno(errno),
            conditions: Vec::new(),
        });
        self
    }

    /// 拒绝一切非 AF_UNIX 的网络（cr-019：AF_UNIX-only 受控出口基线）。
    ///
    /// 只对 `socket(domain != AF_UNIX)` KILL——任务物理上建不出 INET/RAW socket，
    /// 只能建 UDS。其余 socket API（connect/bind/send/...）放行：它们只能作用于
    /// 任务自建的 AF_UNIX fd，叠加 pre_exec 的 close_fds 杜绝继承来的 INET fd。
    ///
    /// 强制点必须在 socket() 创建处：经典 seccomp 无法解引用 sockaddr 指针，
    /// 故不能按目标地址过滤 connect()。堵住 INET fd 的诞生即足够。
    pub fn deny_network(mut self) -> Self {
        self.rules.push(SeccompRule {
            syscall: Syscall::Socket,
            action: SeccompAction::KillProcess,
            conditions: vec![SeccompCondition {
                arg_index: 0,
                operator: CompareOperator::NotEqual,
                value: libc::AF_UNIX as u64,
                mask: None,
            }],
        });
        self
    }

    /// 允许特定 syscall（用于 allowlist 模式）
    pub fn allow(mut self, syscall: Syscall) -> Self {
        self.rules.push(SeccompRule {
            syscall,
            action: SeccompAction::Allow,
            conditions: Vec::new(),
        });
        self
    }

    /// 获取默认动作
    pub fn default_action(&self) -> SeccompAction {
        self.default_action
    }

    /// 获取所有规则
    pub fn rules(&self) -> &[SeccompRule] {
        &self.rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syscall::Syscall;

    /// cr-019: deny_network 只对 socket(domain != AF_UNIX) KILL,其余 socket API 放行
    #[test]
    fn deny_network_blocks_only_non_af_unix_socket() {
        let p = SeccompProfile::default_denylist();

        // 找 socket 规则
        let socket_rules: Vec<&SeccompRule> =
            p.rules().iter().filter(|r| r.syscall == Syscall::Socket).collect();
        assert_eq!(socket_rules.len(), 1, "socket should have one rule");
        assert!(matches!(socket_rules[0].action, SeccompAction::KillProcess));
        assert_eq!(socket_rules[0].conditions.len(), 1, "socket rule should have one condition");
        let cond = &socket_rules[0].conditions[0];
        assert_eq!(cond.arg_index, 0, "condition should target arg0 (domain)");
        assert!(matches!(cond.operator, CompareOperator::NotEqual));
        assert_eq!(cond.value, libc::AF_UNIX as u64, "should allow AF_UNIX");

        // 其余网络 socket API 不再出现在 deny 列表(即默认允许)
        for sc in [
            Syscall::Connect, Syscall::Bind, Syscall::Listen,
            Syscall::Accept, Syscall::Accept4, Syscall::Sendto,
            Syscall::Recvfrom, Syscall::Sendmsg, Syscall::Recvmsg,
            Syscall::Sendmmsg, Syscall::Recvmmsg, Syscall::Getsockopt,
            Syscall::Setsockopt, Syscall::Shutdown, Syscall::Getsockname,
            Syscall::Getpeername,
        ] {
            assert!(
                !p.rules().iter().any(|r| r.syscall == sc),
                "{:?} should not be in the deny list (operations on AF_UNIX fd should be allowed)",
                sc
            );
        }

        // socketpair 仍不在 deny 列表(本地 IPC)
        assert!(!p.rules().iter().any(|r| r.syscall == Syscall::Socketpair));
    }
}
