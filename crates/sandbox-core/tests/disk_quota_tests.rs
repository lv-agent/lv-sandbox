//! cr-022 看门狗集成测试:disk_quota_mb 超限 → DiskQuotaExceeded。
use sandbox_core::job::{JobRequest, JobStatus, SandboxViolation};
use sandbox_core::profile::SandboxProfile;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use std::collections::HashMap;
use std::time::Duration;

async fn make_runner() -> (tempfile::TempDir, SandboxRunner) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("failed to create runner");
    (tmp, runner)
}

fn req(job_id: &str, argv: &[&str], profile: &str) -> JobRequest {
    JobRequest {
        job_id: job_id.to_string(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        profile_name: profile.to_string(),
        timeout: Some(Duration::from_secs(10)),
        custom_env: HashMap::new(),
        stdin_data: None,
    }
}

/// cr-022: profile 配 disk_quota_mb 后,工作区聚合写入超限 → 看门狗收割 → DiskQuotaExceeded。
#[tokio::test]
async fn disk_quota_exceeded_when_workspace_grows_past_limit() {
    let (_tmp, mut runner) = make_runner().await;
    // 测试 profile:disk_quota_mb=1MB 是唯一紧约束。
    // - nproc=None:RLIMIT_NPROC 是 per-uid,测试机当前用户进程数远超 shell 默认的 32,
    //   会让 `yes|head` 管道的 fork 立刻 EAGAIN("Cannot fork");测试中放开。
    // - fsize=1GB:高于 1MB 配额,确保聚合看门狗先于单文件 fsize(SIGXFSZ)触发。
    let mut rlimit = SandboxProfile::shell().rlimit;
    rlimit.nproc = None;
    rlimit.fsize_bytes = Some(1024 * 1024 * 1024);
    runner.register_profile(SandboxProfile {
        name: "quota".to_string(),
        disk_quota_mb: Some(1),
        rlimit,
        ..SandboxProfile::shell()
    });

    // 写 200MB 到工作区(远超 1MB 配额);head 后接 sleep,保证轮询 tick 命中时进程仍在。
    let result = runner
        .run_job(req(
            "quota-001",
            &["/bin/sh", "-c", "yes | head -c 200000000 > big; /bin/sleep 5"],
            "quota",
        ))
        .await
        .expect("run_job should not error");

    assert!(
        matches!(result.status, JobStatus::DiskQuotaExceeded),
        "expected DiskQuotaExceeded, got {:?}",
        result.status
    );
    assert!(
        result
            .sandbox_violations
            .iter()
            .any(|v| matches!(v, SandboxViolation::FileSizeExceeded)),
        "should tag FileSizeExceeded violation, got {:?}",
        result.sandbox_violations
    );
    assert!(!result.timed_out, "must not be a timeout");
}

/// cr-022 回归:profile 不设 disk_quota_mb → 看门狗不起,正常 Completed。
#[tokio::test]
async fn no_quota_profile_runs_normally() {
    let (_tmp, runner) = make_runner().await;
    let result = runner
        .run_job(req("quota-002", &["/bin/echo", "ok"], "shell"))
        .await
        .expect("run_job should not error");
    assert!(matches!(result.status, JobStatus::Completed));
}
