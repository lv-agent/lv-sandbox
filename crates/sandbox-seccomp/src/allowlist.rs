//! cr-045: allowlist 模式白名单数据集。
//!
//! 来源:观察法(strace 跑 shell 典型工作流)+ 人工审核去冗余,固化成代码。
//! 漏列 syscall → 任务被 SIGSYS 杀(SeccompDenied,cr-041 可观测);
//! 回归测试(seccomp_tests `default_allowlist_shell_runs_*`)锁完备性,
//! 运行时新增 syscall 会 break 测试,逼此处同步。

use crate::profile::{CompareOperator, SeccompCondition, SeccompProfile};
use crate::syscall::Syscall;

/// shell 运行时(dash/bash + 常见命令)观察到的必要 syscall。
/// 注:这是**起步集**,Phase 1 回归测试驱动补全;后续运行时升级须回归验证。
const SHELL_ALLOWED: &[&str] = &[
    // 进程生命周期
    "execve", "execveat", "exit", "exit_group", "clone", "clone3", "fork", "vfork",
    "wait4", "arch_prctl", "set_tid_address", "set_robust_list", "rseq", "prctl",
    // 内存
    "brk", "mmap", "munmap", "mprotect", "mremap", "madvise",
    // 文件 / IO
    "read", "write", "readv", "writev", "pread64", "pwrite64", "preadv", "pwritev",
    "open", "openat", "openat2", "close", "close_range",
    "stat", "fstat", "lstat", "newfstatat", "statx", "statfs", "fstatfs", "lseek",
    "access", "faccessat", "faccessat2", "readlink", "readlinkat",
    "dup", "dup2", "dup3", "fcntl", "flock", "ftruncate",
    "getcwd", "chdir", "fchdir", "umask",
    "pipe", "pipe2", "socketpair", "ioctl", "getdents64",
    // 扩展属性(coreutils/libselinux 查 SELinux 标签用)
    "getxattr", "lgetxattr", "fgetxattr", "listxattr", "llistxattr", "flistxattr",
    "fadvise64",
    "utimensat", "futimens", "chmod", "fchmod", "fchmodat",
    "chown", "fchown", "lchown", "fchownat",
    "mkdir", "mkdirat", "rmdir", "unlink", "unlinkat",
    "rename", "renameat", "renameat2", "symlink", "symlinkat", "link", "linkat",
    // 信号
    "rt_sigaction", "rt_sigprocmask", "rt_sigreturn", "rt_sigsuspend",
    "sigaltstack", "kill", "tgkill", "tkill", "setitimer", "getitimer",
    // 身份 / 时间 / 随机 / 限制
    "getpid", "getppid", "getuid", "geteuid", "getgid", "getegid", "getgroups",
    "getpgrp", "getsid", "setpgid",
    "clock_gettime", "clock_getres", "gettimeofday",
    "nanosleep", "clock_nanosleep", "times",
    "getrandom", "prlimit64", "getrlimit", "setrlimit", "sysinfo", "uname",
    "sched_getaffinity", "sched_yield", "getrusage",
    // 同步 / 等待(shell job control / poll)
    "pselect6", "ppoll", "poll", "epoll_create1", "epoll_ctl", "epoll_wait",
    "eventfd2", "futex",
];

/// python 运行时(cpython)相对 shell 额外需要的 syscall。
/// 起步集(dmesg 已见 gettid);Phase 2 回归测试驱动补全。
const PYTHON_EXTRA: &[&str] = &[
    "gettid", // cpython 启动查线程 id(dmesg syscall=186)
];

/// node 运行时(V8)相对 shell 额外需要的 syscall。起步集;Phase 3 回归驱动补全。
const NODE_EXTRA: &[&str] = &[
    "gettid", // V8/node 启动查线程 id(同 python)
    "capget", // V8 启动查进程 capabilities(dmesg syscall=125)
];

/// 通用 allowlist 构建(default KillProcess + 白名单 + socket AF_UNIX 条件放行)。
fn build_profile(allowed: &[&'static str]) -> SeccompProfile {
    let mut p = SeccompProfile::allowlist();
    for &name in allowed {
        p = p.allow(Syscall::Custom(name));
    }
    // cr-019 网络基线:只放行 AF_UNIX socket(INET socket 命中 default KillProcess)
    // 注:io_uring ENOSYS 由 filter.rs 手写 BPF splice(cr-047,绕 libseccomp BPF 生成 bug)
    p.allow_with_conditions(
        Syscall::Socket,
        vec![SeccompCondition {
            arg_index: 0,
            operator: CompareOperator::Equal,
            value: libc::AF_UNIX as u64,
            mask: None,
        }],
    )
}

/// 构建 shell 运行时 allowlist profile。
pub(crate) fn shell() -> SeccompProfile {
    build_profile(SHELL_ALLOWED)
}

/// 构建 python 运行时 allowlist profile(shell 基础 + python 额外)。
pub(crate) fn python() -> SeccompProfile {
    let mut all: Vec<&'static str> = SHELL_ALLOWED.to_vec();
    all.extend_from_slice(PYTHON_EXTRA);
    build_profile(&all)
}

/// 构建 node 运行时 allowlist profile(shell 基础 + node 额外)。
pub(crate) fn node() -> SeccompProfile {
    let mut all: Vec<&'static str> = SHELL_ALLOWED.to_vec();
    all.extend_from_slice(NODE_EXTRA);
    build_profile(&all)
}
