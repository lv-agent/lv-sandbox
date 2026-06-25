//! cr-024 流式 stdout 单测:run_job_with_cancel 的 sink 模式事件序列。
use sandbox_core::job::{JobRequest, StreamEvent};
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

async fn runner() -> (tempfile::TempDir, SandboxRunner) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    (tmp, SandboxRunner::new(&cfg).await.unwrap())
}

/// sink 模式:依序收到 Started → Stdout(块)→ Result;Result.status 与返回值一致。
#[tokio::test]
async fn streaming_sink_emits_started_stdout_result_in_order() {
    let (_tmp, runner) = runner().await;
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);
    let req = JobRequest {
        job_id: "stream-1".into(),
        argv: vec!["/bin/echo".into(), "hello stream".into()],
        profile_name: "shell".into(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    };
    let result = runner
        .run_job_with_cancel(
            req,
            tokio_util::sync::CancellationToken::new(),
            Some(tx),
        )
        .await
        .expect("run_job should not error");

    let mut got_started = false;
    let mut stdout = String::new();
    let mut got_result = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Started { job_id } => {
                assert_eq!(job_id, "stream-1");
                assert!(!got_result, "started must come before result");
                got_started = true;
            }
            StreamEvent::Stdout { data } => {
                assert!(got_started, "stdout must come after started");
                stdout.push_str(&data);
            }
            StreamEvent::Result(r) => {
                assert_eq!(
                    std::mem::discriminant(&r.status),
                    std::mem::discriminant(&result.status),
                    "result event status should match return"
                );
                got_result = true;
            }
        }
    }
    assert!(got_started, "missing Started");
    assert!(got_result, "missing Result");
    assert!(
        stdout.contains("hello stream"),
        "stdout chunks should contain echo output: {stdout:?}"
    );
}

/// sink=None:行为不变(runner 仍正常返回 Completed)。
#[tokio::test]
async fn no_sink_runs_normally() {
    let (_tmp, runner) = runner().await;
    let req = JobRequest {
        job_id: "nosink-1".into(),
        argv: vec!["/bin/echo".into(), "x".into()],
        profile_name: "shell".into(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    };
    let result = runner
        .run_job_with_cancel(req, tokio_util::sync::CancellationToken::new(), None)
        .await
        .expect("run_job should not error");
    use sandbox_core::job::JobStatus;
    assert!(matches!(result.status, JobStatus::Completed));
}
