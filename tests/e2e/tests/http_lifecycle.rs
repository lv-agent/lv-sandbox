//! HTTP 生命周期 E2E 测试
//!
//! 覆盖：health、submit（正常/profile不存在/无效JSON/无效timeout）、status

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
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request("e2e-001", &["/bin/echo", "hello"]),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["job_id"], "e2e-001");
    assert_eq!(result["status"], "Completed");
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"].as_str().unwrap().contains("hello"));
}

#[tokio::test]
async fn submit_job_profile不存在返回400() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("e2e-002", &["/bin/echo", "test"], "nonexistent"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(result["error"].as_str().unwrap().contains("nonexistent"));
}

#[tokio::test]
async fn submit_job_无效json返回400() {
    let (_tmp, app) = create_test_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/submit")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_job_无效timeout返回400() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_timeout("e2e-003", &["/bin/echo", "test"], "shell", "bad_timeout"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(result["error"].as_str().unwrap().contains("timeout"));
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
