//! HTTP 生命周期 E2E 测试
//!
//! 覆盖：health、submit（正常/profile不存在/无效JSON/无效timeout）、status

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

#[tokio::test]
async fn health_check_returns_200() {
    let (_tmp, app) = create_test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn submit_job_executes_normally() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) =
        submit_and_wait(app, "e2e-001", &["/bin/echo", "hello"], "shell", "5s", HashMap::new())
            .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["job_id"].as_str().unwrap(), "e2e-001");
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("hello"));
}

/// cr-018+#72: 提交带 stdin 的任务，子进程（cat）应读到 stdin 并输出
#[tokio::test]
async fn submit_job_with_stdin_can_read_input() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait_with_input(
        app,
        "stdin-001",
        &["/bin/cat"],
        "shell",
        "5s",
        HashMap::new(),
        Some("hello via stdin\n"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
    assert_eq!(
        result["stdout"].as_str().unwrap(),
        "hello via stdin\n",
        "cat should echo stdin verbatim"
    );
}

/// cr-018+#78: 任务输出含敏感信息（Bearer token）应在 GET /jobs/{id} 被脱敏
#[tokio::test]
async fn sensitive_job_output_is_redacted() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "redact-001",
        &[
            "/bin/sh",
            "-c",
            "echo Authorization: Bearer secret123token",
        ],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stdout = result["stdout"].as_str().unwrap();
    assert!(stdout.contains("REDACTED"), "Bearer should be redacted: {stdout}");
    assert!(
        !stdout.contains("secret123token"),
        "token should not leak: {stdout}"
    );
}

#[tokio::test]
async fn submit_job_nonexistent_profile_enters_error_terminal() {
    // cr-018 后 create_job 不再预校验 profile：submit_async 会注册任务，
    // run_job 因 profile 缺失失败 → Done(Error)。
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "e2e-002",
        &["/bin/echo", "test"],
        "nonexistent",
        "5s",
        HashMap::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status_str = result["status"].as_str().unwrap();
    assert!(
        status_str.contains("Error"),
        "nonexistent profile should enter Error terminal state, actual status: {}",
        status_str
    );
}

#[tokio::test]
async fn submit_job_invalid_json_returns_400() {
    let (_tmp, app) = create_test_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_job_invalid_timeout_returns_400() {
    // create_job 在 submit_async 之前校验 timeout 格式，无效 → 直接 400（不会注册 job）。
    let (_tmp, app) = create_test_app().await;
    let body = serde_json::json!({
        "job_id": "e2e-003",
        "argv": ["/bin/echo", "test"],
        "profile_name": "shell",
        "timeout": "bad_timeout",
        "custom_env": {},
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        json["error"].as_str().unwrap().contains("timeout"),
        "error message should mention timeout, actual: {}",
        json["error"]
    );
}

#[tokio::test]
async fn status_returns_worker_info() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        Request::builder()
            .uri("/api/v1/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["max_concurrent"], 10);
    assert_eq!(result["running_jobs"], 0);
}
