//! HTTP 生命周期 E2E 测试
//!
//! 覆盖：health、submit（正常/profile不存在/无效JSON/无效timeout）、status

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

#[tokio::test]
async fn health检查返回200() {
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
async fn submit_job正常执行() {
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
async fn submit_job带stdin_任务能读到输入() {
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
        "cat 应把 stdin 原样输出"
    );
}

#[tokio::test]
async fn submit_job_profile不存在进入error终态() {
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
        "不存在 profile 应进入 Error 终态，实际 status: {}",
        status_str
    );
}

#[tokio::test]
async fn submit_job_无效json返回400() {
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
async fn submit_job_无效timeout返回400() {
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
        "错误信息应说明 timeout，实际: {}",
        json["error"]
    );
}

#[tokio::test]
async fn status返回worker信息() {
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
