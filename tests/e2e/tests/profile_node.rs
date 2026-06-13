//! Node profile E2E 测试
//!
//! 通过 HTTP 使用 node profile 执行命令，验证完整链路

use std::collections::HashMap;

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;

#[tokio::test]
async fn node_profile_echo_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "node-001",
        &["/bin/echo", "node_works"],
        "node",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"].as_str().unwrap().contains("node_works"));
}

#[tokio::test]
async fn node_profile_环境变量隔离() {
    let (_tmp, app) = create_test_app().await;
    let mut env = HashMap::new();
    env.insert("NODE_TEST".to_string(), "value".to_string());

    let (status, result) =
        submit_and_wait(app, "node-env-001", &["/bin/echo", "ok"], "node", "5s", env).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
}

#[tokio::test]
async fn node_profile_超时kill进程() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "node-timeout-001",
        &["/bin/sh", "-c", "exec /bin/sleep 30"],
        "node",
        "1s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "TimedOut");
    assert_eq!(result["timed_out"].as_bool(), Some(true));
}

#[tokio::test]
async fn node_profile_workspace被清理() {
    let (tmp, app) = create_test_app().await;
    let base_dir = tmp.path().to_path_buf();

    let job_dir = base_dir.join("node-ws-001");
    assert!(!job_dir.exists(), "执行前 workspace 不应存在");

    let _ = submit_and_wait(
        app,
        "node-ws-001",
        &["/bin/echo", "cleanup"],
        "node",
        "5s",
        HashMap::new(),
    )
    .await;

    assert!(!job_dir.exists(), "执行后 workspace 应被清理");
}
