//! #70 可行性实验：pre_exec 里动态 landlock add_rule("/proc/<getpid()>")
//!
//! 一次验证三点：
//! 1. 信号安全 —— pre_exec 里 Ruleset create + PathFd::new + add_rule + restrict_self
//!    （含 format! 分配）不崩、不死锁，exec 能成功
//! 2. /proc/self 放行 —— 子进程能读自己的 /proc/self/status
//! 3. /proc/<别的pid> 堵住 —— 读 /proc/$PPID/status（父进程）被拒
//!
//! 关键点：用 sh 内建 read（不 fork 外部命令），保证读 /proc/self 的进程 pid
//! == pre_exec 时 getpid() 的 pid（模拟 lv-sandbox 直接 exec argv[0] 的场景）。
//!
//! 运行：cargo run -p sandbox-landlock --example proc_preexec

use std::os::unix::process::CommandExt;
use std::process::Command;

use landlock::{Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI};

fn main() {
    eprintln!("=== #70 experiment: pre_exec dynamic landlock /proc/<pid> ===");

    // sh 内建 read 读文件（不 fork），$PPID = 父进程 pid（未放行，应被拒）
    let script = "\
read s < /proc/self/status; echo \"self_status: $s\"; \
read p < /proc/$PPID/status && echo \"ppid_status: $p\" || echo \"ppid_status: BLOCKED\"; \
read c < /proc/cpuinfo && echo \"cpuinfo: OK\" || echo \"cpuinfo: BLOCKED\"; \
echo DONE \
";

    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);

    unsafe {
        cmd.pre_exec(|| {
            let pid = libc::getpid();
            eprintln!("[pre_exec] child pid = {pid}");

            let abi = ABI::V3;
            let all = AccessFs::from_all(abi);
            let read = AccessFs::from_read(abi);
            let rx = read | AccessFs::Execute;

            let mut rs = Ruleset::default()
                .handle_access(all)
                .map_err(|e| std::io::Error::other(format!("handle_access: {e}")))?
                .create()
                .map_err(|e| std::io::Error::other(format!("create: {e}")))?;
            eprintln!("[pre_exec] ruleset created");

            // 基础路径（让 sh 能 exec + 跑内建）
            for p in ["/bin", "/usr/bin", "/sbin", "/usr/sbin"] {
                if let Ok(fd) = PathFd::new(p) {
                    rs = rs
                        .add_rule(PathBeneath::new(fd, rx))
                        .map_err(|e| std::io::Error::other(format!("add {p}: {e}")))?;
                }
            }
            for p in ["/lib", "/lib64", "/usr/lib", "/usr/local/lib", "/etc"] {
                if let Ok(fd) = PathFd::new(p) {
                    rs = rs
                        .add_rule(PathBeneath::new(fd, read))
                        .map_err(|e| std::io::Error::other(format!("add {p}: {e}")))?;
                }
            }
            eprintln!("[pre_exec] base paths allowed");

            // ★ 关键：动态放行 /proc/<自己的 pid>
            let self_proc = format!("/proc/{pid}");
            let fd = PathFd::new(&self_proc)
                .map_err(|e| std::io::Error::other(format!("PathFd {self_proc}: {e}")))?;
            rs = rs
                .add_rule(PathBeneath::new(fd, read))
                .map_err(|e| std::io::Error::other(format!("add {self_proc}: {e}")))?;
            eprintln!("[pre_exec] ★ dynamically granted {self_proc}");

            // 全局 /proc/cpuinfo（验证全局无害项放行）
            if let Ok(fd) = PathFd::new("/proc/cpuinfo") {
                rs = rs
                    .add_rule(PathBeneath::new(fd, read))
                    .map_err(|e| std::io::Error::other(format!("add /proc/cpuinfo: {e}")))?;
            }

            rs.restrict_self()
                .map_err(|e| std::io::Error::other(format!("restrict_self: {e}")))?;
            eprintln!("[pre_exec] ★ restrict_self succeeded (signal-safe)");
            Ok(())
        });
    }

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[result] spawn/exec failed: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("[result] exit code = {:?}", output.status.code());
    print!("[result] stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        eprintln!("[result] stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    let out = String::from_utf8_lossy(&output.stdout);
    let ok = out.contains("DONE")
        && out.contains("self_status:")
        && out.contains("ppid_status: BLOCKED");
    eprintln!();
    if ok {
        eprintln!("✅ experiment B feasible: pre_exec dynamic /proc/<pid> works (signal-safe + self allowed + other pid blocked)");
    } else {
        eprintln!("❌ experiment B infeasible or needs adjustment (see output above)");
    }
}
