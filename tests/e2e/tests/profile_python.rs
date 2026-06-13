//! Python profile E2E 测试
//!
//! 通过 HTTP 使用 python profile 执行命令，验证完整链路

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;

#[tokio::test]
async fn python_profile_echo_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("py-001", &["/bin/echo", "python_works"], "python"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"].as_str().unwrap().contains("python_works"));
}

#[tokio::test]
async fn python_profile_环境变量隔离() {
    let (_tmp, app) = create_test_app().await;
    let mut env = std::collections::HashMap::new();
    env.insert("MY_TEST_VAR".to_string(), "secret_value".to_string());

    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_env(
            "py-env-001",
            &["/bin/echo", "done"],
            "python",
            env,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
}

#[tokio::test]
async fn python_profile_stderr被正确捕获() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("py-err-001", &["/bin/sh", "-c", "echo stderr_msg >&2"], "python"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let stderr = result["stderr"].as_str().unwrap();
    assert!(stderr.contains("stderr_msg"), "stderr 应被捕获, 实际: {}", stderr);
}

#[tokio::test]
async fn python_profile_非零退出码() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("py-exit-001", &["/bin/sh", "-c", "exit 42"], "python"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["exit_code"], 42);
}
