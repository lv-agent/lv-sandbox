//! Node profile E2E 测试
//!
//! 通过 HTTP 使用 node profile 执行命令，验证完整链路

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;

#[tokio::test]
async fn node_profile_echo_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("node-001", &["/bin/echo", "node_works"], "node"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"].as_str().unwrap().contains("node_works"));
}

#[tokio::test]
async fn node_profile_环境变量隔离() {
    let (_tmp, app) = create_test_app().await;
    let mut env = std::collections::HashMap::new();
    env.insert("NODE_TEST".to_string(), "value".to_string());

    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_env("node-env-001", &["/bin/echo", "ok"], "node", env),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
}

#[tokio::test]
async fn node_profile_超时kill进程() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_timeout(
            "node-timeout-001",
            &["/bin/sh", "-c", "exec /bin/sleep 30"],
            "node",
            "1s",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["timed_out"], true);
}

#[tokio::test]
async fn node_profile_workspace被清理() {
    let (tmp, app) = create_test_app().await;
    let base_dir = tmp.path().to_path_buf();

    let job_dir = base_dir.join("node-ws-001");
    assert!(!job_dir.exists(), "执行前 workspace 不应存在");

    let _ = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("node-ws-001", &["/bin/echo", "cleanup"], "node"),
    )
    .await;

    assert!(!job_dir.exists(), "执行后 workspace 应被清理");
}
