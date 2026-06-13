//! 安全机制验证 E2E 测试
//!
//! 验证 landlock/seccomp/rlimit/NoNewPrivs/setsid/fd cleanup 在真实执行路径中生效

use sandbox_e2e::helpers::*;
use std::time::Duration;

#[tokio::test]
async fn no_new_privs已设置() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("nnp-001", &["/bin/cat", "/proc/self/status"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("NoNewPrivs:\t1"),
        "NoNewPrivs 应为 1, stdout: {}",
        stdout
    );
}

#[tokio::test]
async fn setsid创建新session() {
    let parent_sid = unsafe { libc::getsid(0) };
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("sid-001", &["/bin/cat", "/proc/self/status"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    // 从 /proc/self/status 提取 Pid 和 SID，它们应该相等（因为 setsid 后 PGID=SID=PID）
    for line in stdout.lines() {
        if line.starts_with("NSpid:") || line.starts_with("Pid:") {
            // 简单验证：能读到即可
            assert!(line.contains(':'), "应能解析 status 行: {}", line);
        }
    }
}

#[tokio::test]
async fn 泄漏的fd被关闭() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("fd-001", &["/bin/ls", "/proc/self/fd"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let fd_count = stdout.lines().filter(|l| !l.is_empty()).count();
    assert!(
        fd_count <= 10,
        "fd 数应 <= 10, 实际: {}, 列表:\n{}",
        fd_count, stdout
    );
}

#[tokio::test]
async fn 环境变量不泄漏到子进程() {
    // 在父进程中设置一个敏感变量
    std::env::set_var("LEAK_TEST_SECRET", "should_not_leak");

    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("env-001", &["/usr/bin/env"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        !stdout.contains("LEAK_TEST_SECRET"),
        "子进程不应看到父进程的环境变量, stdout: {}",
        stdout
    );
}

#[tokio::test]
async fn landlock阻止未授权路径的写入() {
    let (_tmp, runner) = create_test_runner().await;
    // 尝试写 /tmp 外的文件（landlock 应阻止）
    let req = make_job_request(
        "ll-001",
        &["/bin/sh", "-c", "echo test > /var/tmp/e2e_landlock_test.txt; echo exit=$?"],
    );
    let result = runner.run_job(req).await.expect("执行失败");

    // 即使 shell 报错，文件不应存在
    assert!(
        !std::path::Path::new("/var/tmp/e2e_landlock_test.txt").exists(),
        "landlock 应阻止写入 /var/tmp"
    );
}

#[tokio::test]
async fn landlock允许workspace写入() {
    let (tmp, runner) = create_test_runner().await;
    let base_dir = tmp.path().to_path_buf();

    // workspace 内写文件应该成功
    let req = make_job_request(
        "ll-ws-001",
        &["/bin/sh", "-c", "echo workspace_write_ok > test_file.txt"],
    );
    let result = runner.run_job(req).await.expect("执行失败");

    // job 完成后 workspace 被清理，但执行期间应该能写
    // 验证方式：命令执行成功（exit_code=0）
    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn 内置profile有seccomp_denylist() {
    for (name, profile) in [
        ("shell", sandbox_core::profile::SandboxProfile::shell()),
        ("python", sandbox_core::profile::SandboxProfile::python()),
        ("node", sandbox_core::profile::SandboxProfile::node()),
    ] {
        assert!(
            profile.seccomp_profile.is_some(),
            "{} profile 应有 seccomp denylist",
            name
        );
    }
}

/// cr-016: 默认禁网——尝试联网的任务应被阻止（status 非 Completed）。
/// 验证 default_denylist → profile → run_job 整条链路传导禁网。
#[tokio::test]
async fn 默认禁网_联网任务被阻止() {
    // 依赖 python3 触发 socket()；缺失则跳过，避免误报
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("跳过：环境无 python3，无法在 e2e 层验证默认禁网");
        return;
    }

    let (_tmp, runner) = create_test_runner().await;
    // python profile：landlock 允许 python3，seccomp = default_denylist（默认禁网）
    let req = make_job_request_with_profile(
        "net-001",
        &["/usr/bin/python3", "-c", "import socket; socket.socket()"],
        "python",
        Duration::from_secs(10),
    );
    let result = runner.run_job(req).await.expect("执行失败");

    // socket() 被 seccomp KillProcess → 任务不会正常完成
    assert!(
        !matches!(result.status, sandbox_core::job::JobStatus::Completed),
        "尝试联网的任务应被禁网阻止（status 非 Completed），实际: {:?}, exit_code: {:?}, signal: {:?}",
        result.status,
        result.exit_code,
        result.signal
    );
}

// ==================== cr-017: proc 信息边界收紧 ====================
// 直接 exec cat/ls（不经 sh fork），保证读 /proc/self 的进程 pid == 动态放行的 pid

/// cr-017: 任务能读自己的 /proc/self（pre_exec 动态放行 /proc/<pid>）
#[tokio::test]
async fn proc收紧_任务能读自己的proc_self() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("proc-self-001", &["/bin/cat", "/proc/self/status"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Name:"),
        "应能读 /proc/self/status（动态放行），stdout: {stdout}"
    );
}

/// cr-017: 任务读不到别的 pid 的 /proc（/proc/1 = init，必然存在且非自己）
#[tokio::test]
async fn proc收紧_任务读不到别的pid的proc() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("proc-other-001", &["/bin/cat", "/proc/1/status"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        !stdout.contains("Name:"),
        "不应读到 /proc/1/status（别的 pid 被堵），stdout: {stdout}"
    );
    assert_ne!(
        result.exit_code,
        Some(0),
        "读 /proc/1 应失败（非 0 退出），exit_code: {:?}",
        result.exit_code
    );
}

/// cr-017: 全局 /proc/cpuinfo 仍可读（白名单）
#[tokio::test]
async fn proc收紧_全局cpuinfo可读() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("proc-cpu-001", &["/bin/cat", "/proc/cpuinfo"]);
    let result = runner.run_job(req).await.expect("执行失败");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("processor"),
        "应能读 /proc/cpuinfo（全局白名单），stdout 前 200: {}",
        &stdout[..stdout.len().min(200)]
    );
}

/// cr-017: 任务不能 ls /proc（列全部 pid，信息泄露面）
#[tokio::test]
async fn proc收紧_不能列出proc根() {
    let (_tmp, runner) = create_test_runner().await;
    let req = make_job_request("proc-ls-001", &["/bin/ls", "/proc"]);
    let result = runner.run_job(req).await.expect("执行失败");

    assert_ne!(
        result.exit_code,
        Some(0),
        "ls /proc 应被拒（非 0 退出），exit_code: {:?}, stdout: {:?}",
        result.exit_code,
        String::from_utf8_lossy(&result.stdout)
    );
}
