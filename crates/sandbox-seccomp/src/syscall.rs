/// Linux syscall 抽象
#[derive(Debug, Clone, Copy)]
pub enum Syscall {
    // 文件系统
    Mount,
    Umount2,

    // 进程追踪
    Ptrace,
    ProcessVmReadv,
    ProcessVmWritev,

    // BPF / eBPF
    Bpf,

    // Keyring
    Keyctl,
    AddKey,
    RequestKey,

    // 系统
    Reboot,
    KexecLoad,

    // 内核模块
    InitModule,
    FinitModule,
    DeleteModule,

    // 交换
    Swapon,
    Swapoff,

    // 网络/主机名
    Sethostname,
    Setdomainname,

    // Namespace
    Setns,
    Unshare,

    // 杂项危险
    Personality,
    Iopl,
    Ioperm,
    PerfEventOpen,
    Userfaultfd,

    // io_uring
    IoUringSetup,
    IoUringEnter,
    IoUringRegister,

    // Clone (需要条件过滤)
    Clone,
    Clone3,

    /// 自定义 syscall（按名称）
    Custom(&'static str),
}
