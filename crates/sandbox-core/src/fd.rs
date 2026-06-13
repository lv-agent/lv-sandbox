use std::os::unix::io::RawFd;

use crate::error::CoreError;

/// 在 pre_exec 闭包中关闭所有非必要 fd。
/// 只保留 stdin(0)、stdout(1)、stderr(2) 和指定的额外 fd。
///
/// 实现策略：获取 RLIMIT_NOFILE 上限，遍历 fd 编号关闭不在 keep 集合中的。
/// 纯 syscall + 栈上数组，不分配堆内存，安全用于 pre_exec。
pub fn close_unneeded_fds(keep_fds: &[RawFd]) -> Result<(), CoreError> {
    // 获取 fd 上限
    let max_fd = get_max_fd();

    // 栈上构建 keep 集合
    const MAX_SCAN: usize = 4096;
    let limit = (max_fd as usize).min(MAX_SCAN);
    let mut keep = [false; MAX_SCAN];

    // 始终保留 stdio
    keep[0] = true;
    keep[1] = true;
    keep[2] = true;
    for &fd in keep_fds {
        if (fd as usize) < MAX_SCAN {
            keep[fd as usize] = true;
        }
    }

    // 从 3 开始关闭（跳过 stdio）
    for fd in 3..limit {
        if !keep[fd] {
            unsafe {
                libc::close(fd as RawFd);
            }
        }
    }

    Ok(())
}

/// 获取当前进程的 fd 上限
fn get_max_fd() -> RawFd {
    let mut rlimit: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlimit) } == 0 {
        (rlimit.rlim_cur as usize).min(4096) as RawFd
    } else {
        1024
    }
}
