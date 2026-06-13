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
