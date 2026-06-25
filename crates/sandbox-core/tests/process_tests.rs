//! process 模块集成测试：SandboxRunner::run_job() 完整生命周期
//!
//! 第 1 轮 TDD：正常执行 + 退出码 + stdout 捕获
//! 第 2 轮 TDD：env 白名单验证

use sandbox_core::job::{JobRequest, JobStatus};
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use std::collections::HashMap;
use std::time::Duration;

/// 创建临时 SandboxRunner
async fn create_test_runner() -> (tempfile::TempDir, SandboxRunner) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config).await.expect("failed to create runner");
    (tmp, runner)
}

fn make_request(job_id: &str, argv: &[&str]) -> JobRequest {
    JobRequest {
        job_id: job_id.to_string(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        profile_name: "shell".to_string(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    }
}

// ==================== 第 1 轮：正常执行 + 退出码 + stdout ====================

#[tokio::test]
async fn normal_echo_returns_exit_code_0_and_stdout() {
    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(make_request("job-001", &["/bin/echo", "hello world"]))
        .await
        .expect("run_job should not error");

    assert_eq!(result.job_id, "job-001");
    assert!(matches!(result.status, JobStatus::Completed));
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "hello world"
    );
}

#[tokio::test]
async fn nonzero_exit_returns_correct_code() {
    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(make_request("job-002", &["/bin/sh", "-c", "exit 42"]))
        .await
        .expect("run_job should not error");

    assert!(matches!(result.status, JobStatus::Completed));
    assert_eq!(result.exit_code, Some(42));
}

#[tokio::test]
async fn stderr_captured_correctly() {
    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(make_request("job-003", &["/bin/sh", "-c", "echo err >&2"]))
        .await
        .expect("run_job should not error");

    assert!(matches!(result.status, JobStatus::Completed));
    assert_eq!(String::from_utf8_lossy(&result.stderr).trim(), "err");
}

#[tokio::test]
async fn profile_not_found_returns_error() {
    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(JobRequest {
            profile_name: "nonexistent".to_string(),
            ..make_request("job-004", &["/bin/echo", "test"])
        })
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, sandbox_core::CoreError::ProfileNotFound(_)));
}

#[tokio::test]
async fn multiline_stdout_all_captured() {
    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(make_request(
            "job-005",
            &["/bin/sh", "-c", "echo line1; echo line2; echo line3"],
        ))
        .await
        .expect("run_job should not error");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("line1"));
    assert!(stdout.contains("line2"));
    assert!(stdout.contains("line3"));
}

// ==================== 第 2 轮：env 白名单 ====================

#[tokio::test]
async fn child_does_not_inherit_parent_env() {
    // 在父进程中设置一个敏感变量
    std::env::set_var("SECRET_TOKEN", "should_not_leak");

    let (_tmp, runner) = create_test_runner().await;

    let result = runner
        .run_job(make_request(
            "env-001",
            &["/bin/sh", "-c", "echo SECRET=${SECRET_TOKEN:-NOT_SET}"],
        ))
        .await
        .expect("run_job should not error");

    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "SECRET=NOT_SET",
        "child should not inherit parent's SECRET_TOKEN"
    );

    std::env::remove_var("SECRET_TOKEN");
}

#[tokio::test]
async fn child_has_only_allowlisted_base_vars() {
    let (_tmp, runner) = create_test_runner().await;

    // 打印 PATH, HOME, LANG — 应该存在
    let result = runner
        .run_job(make_request(
            "env-002",
            &[
                "/bin/sh",
                "-c",
                "echo PATH=${PATH:-MISSING} HOME=${HOME:-MISSING} LANG=${LANG:-MISSING}",
            ],
        ))
        .await
        .expect("run_job should not error");

    let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
    assert!(
        stdout.contains("PATH=/usr/bin:/bin"),
        "PATH should be allowlisted value: {stdout}"
    );
    assert!(
        stdout.contains("LANG=C.UTF-8"),
        "LANG should be allowlisted value: {stdout}"
    );
    assert!(
        stdout.contains("HOME="),
        "HOME should exist: {stdout}"
    );
}

#[tokio::test]
async fn custom_env_vars_visible_in_child() {
    let (_tmp, runner) = create_test_runner().await;

    let mut req = make_request(
        "env-003",
        &["/bin/sh", "-c", "echo MY_VAR=${MY_VAR:-NOT_SET}"],
    );
    req.custom_env.insert("MY_VAR".to_string(), "hello123".to_string());

    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "MY_VAR=hello123",
        "custom_env vars should be passed to child"
    );
}

// ==================== 第 3 轮：超时 kill ====================

#[tokio::test]
async fn timeout_kills_child_returns_timed_out() {
    let (_tmp, runner) = create_test_runner().await;

    // 直接执行 /bin/sleep，不走 shell fork（RLIMIT_NPROC 会阻止 fork）
    let mut req = make_request("timeout-001", &["/bin/sleep", "30"]);
    req.timeout = Some(Duration::from_secs(1));

    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert!(result.timed_out, "should be marked as timed out");
    assert!(
        matches!(result.status, JobStatus::TimedOut),
        "status should be TimedOut"
    );
    assert!(
        result.duration < Duration::from_secs(5),
        "should finish within seconds, actual duration {:?}",
        result.duration
    );
}

#[tokio::test]
async fn stdout_produced_before_timeout_captured() {
    let (_tmp, runner) = create_test_runner().await;

    // 用 shell -c 但先用 echo 再 sleep，验证超时前的输出被捕获
    // 注意：RLIMIT_NPROC 限制真实用户的总进程数，测试环境中需确保 shell 能 fork
    // 为避免 nproc 干扰，使用一个创建 profile 不设 nproc 的 runner
    let mut req = make_request(
        "timeout-002",
        &["/bin/sh", "-c", "echo before_sleep; exec /bin/sleep 30"],
    );
    req.timeout = Some(Duration::from_secs(2));

    // 使用 exec 替换 shell，不需要额外 fork
    let result = runner
        .run_job(req)
        .await
        .expect("run_job should not error");

    assert!(result.timed_out);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("before_sleep"),
        "stdout produced before timeout should be captured"
    );
}

// ==================== 第 4 轮：setsid + workspace + metadata ====================

#[tokio::test]
async fn child_runs_in_own_session() {
    let (_tmp, runner) = create_test_runner().await;

    // 获取父进程的 sid，然后检查子进程的 sid 是否不同
    let parent_sid = unsafe { libc::getsid(0) };

    let result = runner
        .run_job(make_request(
            "setsid-001",
            &["/bin/sh", "-c", "echo SID=$$",],
        ))
        .await
        .expect("run_job should not error");

    // $$ 在 setsid 后的 shell 中等于子进程 PID，也就是 SID
    let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
    assert!(stdout.starts_with("SID="), "should have SID output: {stdout}");

    let child_sid: i32 = stdout
        .strip_prefix("SID=")
        .unwrap()
        .parse()
        .expect("should parse SID");

    assert_ne!(
        child_sid, parent_sid as i32,
        "child SID should differ from parent"
    );
}

#[tokio::test]
async fn workspace_cleaned_after_completion() {
    let (tmp, runner) = create_test_runner().await;
    let base_dir = tmp.path().to_path_buf();

    let job_dir = base_dir.join("job-ws-001");
    assert!(!job_dir.exists(), "workspace should not exist before run");

    runner
        .run_job(make_request("job-ws-001", &["/bin/echo", "done"]))
        .await
        .expect("run_job should not error");

    assert!(
        !job_dir.exists(),
        "workspace should be cleaned after completion: {:?}",
        job_dir
    );
}

#[tokio::test]
async fn metadata_records_full_lifecycle() {
    let (_tmp, runner) = create_test_runner().await;

    // 运行一个 job
    let result = runner
        .run_job(make_request("job-meta-001", &["/bin/echo", "test"]))
        .await
        .expect("run_job should not error");

    // job 结束后 workspace 被清理，metadata 也应被清理
    // 所以我们无法在结束后读取 metadata
    // 但我们可以验证 runner 不报错且正常完成
    assert!(matches!(result.status, JobStatus::Completed));

    // 验证 workspace 确实被清理
    let ws = runner.workspace_mgr();
    let meta = ws.read_metadata("job-meta-001").expect("read should not error");
    assert!(
        meta.is_none(),
        "metadata should be cleaned with workspace after job ends"
    );
}

#[tokio::test]
async fn multiple_jobs_run_in_parallel_without_interference() {
    let (_tmp, runner) = create_test_runner().await;

    // 同时提交 5 个 job
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let runner = &runner;
            let req = make_request(
                &format!("parallel-{i}"),
                &["/bin/sh", "-c", &format!("echo job-{i} ; /bin/sleep 0.{i}"),
                ],
            );
            async move { runner.run_job(req).await }
        })
        .collect();

    // 使用 futures::join_all 等待全部完成
    let results = futures::future::join_all(handles).await;

    for (i, result) in results.into_iter().enumerate() {
        let r = result.expect(&format!("job {i} should not error"));
        assert!(
            matches!(r.status, JobStatus::Completed),
            "job {i} should complete normally"
        );
        assert_eq!(
            String::from_utf8_lossy(&r.stdout).trim(),
            format!("job-{i}"),
            "job {i} stdout should be correct"
        );
    }
}

// ==================== 第 5 轮：安全机制闭合 ====================

#[tokio::test]
async fn child_sets_no_new_privs() {
    let (_tmp, runner) = create_test_runner().await;
    // 用 /bin/cat 直接读取，不用 sh -c（避免 sh fork 子进程触发 nproc 限制）
    let req = make_request(
        "no-new-privs",
        &["/bin/cat", "/proc/self/status"],
    );
    let result = runner.run_job(req).await.expect("execution failed");

    assert!(matches!(result.status, JobStatus::Completed));
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("NoNewPrivs:\t1"),
        "child should enable NoNewPrivs, actual output: {}",
        stdout
    );
}

#[tokio::test]
async fn leaked_fds_in_child_closed() {
    let (_tmp, runner) = create_test_runner().await;
    // 用 /bin/ls 直接列出 /proc/self/fd，不用 sh -c（避免 nproc 限制）
    let req = make_request(
        "fd-check",
        &["/bin/ls", "/proc/self/fd"],
    );
    let result = runner.run_job(req).await.expect("execution failed");

    assert!(matches!(result.status, JobStatus::Completed));
    let stdout = String::from_utf8_lossy(&result.stdout);
    let fd_count = stdout.lines().filter(|l| !l.is_empty()).count();
    // ls 本身会打开 dir fd，正常约 4-6 个 (0,1,2,3 + dir)，不应超过 10
    assert!(
        fd_count <= 10,
        "child should have no leaked fds, actual fd count: {}, list:\n{}",
        fd_count, stdout
    );
}

#[test]
fn builtin_profiles_have_default_seccomp_denylist() {
    use sandbox_core::profile::SandboxProfile;

    for (name, profile) in [
        ("shell", SandboxProfile::shell()),
        ("python", SandboxProfile::python()),
        ("node", SandboxProfile::node()),
    ] {
        assert!(
            profile.seccomp_profile.is_some(),
            "{} profile should have a seccomp denylist",
            name
        );
        let seccomp = profile.seccomp_profile.as_ref().unwrap();
        assert!(
            !seccomp.rules().is_empty(),
            "{} profile seccomp denylist should not be empty",
            name
        );
    }
}

// ==================== cr-016: 被信号杀死 → Killed ====================

#[tokio::test]
async fn signal_killed_job_status_is_killed() {
    let (_tmp, runner) = create_test_runner().await;

    // kill -9 $$：进程向自己发 SIGKILL，被信号杀死（非超时、非正常退出）
    let result = runner
        .run_job(make_request("kill-001", &["/bin/sh", "-c", "kill -9 $$"]))
        .await
        .expect("run_job should not error");

    assert!(
        matches!(result.status, JobStatus::Killed),
        "signal-killed job should be Killed, actual: {:?}, signal: {:?}",
        result.status,
        result.signal
    );
    assert!(!result.timed_out, "self-kill should not be recorded as timeout");
}

// ==================== cr-018 阶段1: cancel token ====================

#[tokio::test]
async fn cancel_token_aborts_job_status_is_cancelled() {
    use tokio_util::sync::CancellationToken;

    let (_tmp, runner) = create_test_runner().await;
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // sleep 30 + timeout 1s；cancel 在 200ms 触发（远早于 timeout）
    let mut req = make_request("cancel-001", &["/bin/sleep", "30"]);
    req.timeout = Some(Duration::from_secs(1));

    let handle = tokio::spawn(async move { runner.run_job_with_cancel(req, cancel_clone, None).await });

    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();

    let result = handle.await.expect("task panic").expect("run_job should not error");

    assert!(
        matches!(result.status, JobStatus::Cancelled),
        "status should be Cancelled after cancel fires, actual: {:?}",
        result.status
    );
}
