//! 超时 + Prometheus 指标 E2E 测试

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

#[tokio::test]
async fn timeout_returns_timed_out_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_timeout(
            "timeout-001",
            &["/bin/sh", "-c", "echo before; exec /bin/sleep 30"],
            "shell",
            "1s",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "TimedOut");
    assert_eq!(result["timed_out"], true);
}

#[tokio::test]
async fn timeout前产生的stdout被捕获() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_timeout(
            "timeout-002",
            &["/bin/sh", "-c", "echo captured_output; exec /bin/sleep 30"],
            "shell",
            "1s",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let stdout = result["stdout"].as_str().unwrap();
    assert!(stdout.contains("captured_output"), "超时前的 stdout 应被捕获");
}

#[tokio::test]
async fn metrics端点返回prometheus格式() {
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
}

#[tokio::test]
async fn 执行job后metrics_started_counter递增() {
    // 直接从 Prometheus 默认 registry 读取 baseline
    let baseline = prometheus_metric_value("sandbox_job_started_total");

    let (_tmp, app) = create_test_app().await;
    let _ = app
        .oneshot(make_submit_request("metric-001", &["/bin/echo", "test"]))
        .await
        .unwrap();

    let after = prometheus_metric_value("sandbox_job_started_total");
    assert!(
        after > baseline,
        "started counter 应递增: before={}, after={}",
        baseline, after
    );
}

#[tokio::test]
async fn 超时job后metrics_timeout_counter递增() {
    let baseline = prometheus_metric_value("sandbox_job_timeout_total");

    let (_tmp, app) = create_test_app().await;
    let _ = app
        .oneshot(make_submit_request_with_timeout(
            "metric-timeout",
            &["/bin/sh", "-c", "exec /bin/sleep 30"],
            "shell",
            "1s",
        ))
        .await
        .unwrap();

    let after = prometheus_metric_value("sandbox_job_timeout_total");
    assert!(
        after > baseline,
        "timeout counter 应递增: before={}, after={}",
        baseline, after
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
