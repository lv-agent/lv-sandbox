//! 超时 + Prometheus 指标 E2E 测试

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

#[tokio::test]
async fn timeout_returns_timed_out_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "timeout-001",
        &["/bin/sh", "-c", "echo before; exec /bin/sleep 30"],
        "shell",
        "1s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "TimedOut");
    assert_eq!(result["timed_out"].as_bool(), Some(true));
}

#[tokio::test]
async fn stdout_before_timeout_is_captured() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "timeout-002",
        &["/bin/sh", "-c", "echo captured_output; exec /bin/sleep 30"],
        "shell",
        "1s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let stdout = result["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("captured_output"),
        "stdout before timeout should be captured"
    );
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_format() {
    let (_tmp, app) = create_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);

    assert!(text.contains("sandbox_job_started_total"));
    assert!(text.contains("sandbox_running_jobs"));
    assert!(text.contains("sandbox_fork_exec_duration_seconds"));
    // cr-041: 违规 + 队列深度指标
    assert!(text.contains("sandbox_job_seccomp_denied_total"));
    assert!(text.contains("sandbox_job_oom_killed_total"));
    assert!(text.contains("sandbox_job_queue_depth"));
}

#[tokio::test]
async fn metrics_started_counter_increments_after_job() {
    // 直接从 Prometheus 默认 registry 读取 baseline
    let baseline = prometheus_metric_value("sandbox_job_started_total");

    let (_tmp, app) = create_test_app().await;
    // cr-018: 异步接口下 started counter 在 spawn 任务内递增，
    // submit_and_wait 等到 job 完成，保证 counter 已更新。
    let _ = submit_and_wait(
        app,
        "metric-001",
        &["/bin/echo", "test"],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    let after = prometheus_metric_value("sandbox_job_started_total");
    assert!(
        after > baseline,
        "started counter should increment: before={}, after={}",
        baseline,
        after
    );
}

#[tokio::test]
async fn metrics_timeout_counter_increments_after_timeout_job() {
    let baseline = prometheus_metric_value("sandbox_job_timeout_total");

    let (_tmp, app) = create_test_app().await;
    let _ = submit_and_wait(
        app,
        "metric-timeout",
        &["/bin/sh", "-c", "exec /bin/sleep 30"],
        "shell",
        "1s",
        HashMap::new(),
    )
    .await;

    let after = prometheus_metric_value("sandbox_job_timeout_total");
    assert!(
        after > baseline,
        "timeout counter should increment: before={}, after={}",
        baseline,
        after
    );
}

/// cr-041: seccomp 违规触发 counter 递增(end-to-end)。
#[tokio::test]
async fn seccomp_denied_counter_increments_via_http() {
    let baseline = prometheus_metric_value("sandbox_job_seccomp_denied_total");

    let (_tmp, app) = create_test_app().await;
    let _ = submit_and_wait(
        app,
        "sec-e2e-001",
        &["/usr/bin/unshare", "-r", "/bin/true"],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    let after = prometheus_metric_value("sandbox_job_seccomp_denied_total");
    assert!(
        after > baseline,
        "seccomp_denied counter should increment: before={baseline}, after={after}"
    );
}

/// 从 Prometheus 默认 registry 读取指标值
fn prometheus_metric_value(name: &str) -> f64 {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let families = prometheus::gather();
    let mut buf = Vec::new();
    encoder.encode(&families, &mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);

    for line in text.lines() {
        if line.starts_with(name) && !line.starts_with('#') {
            if let Some(val) = line.split_whitespace().nth(1) {
                return val.parse().unwrap_or(0.0);
            }
        }
    }
    0.0
}
