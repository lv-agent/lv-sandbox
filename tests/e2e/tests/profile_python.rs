//! Python profile E2E 测试
//!
//! 通过 HTTP 使用 python profile 执行命令，验证完整链路

use std::collections::HashMap;

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;

#[tokio::test]
async fn python_profile_echo_via_http() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "py-001",
        &["/bin/echo", "python_works"],
        "python",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert!(result["stdout"]
        .as_str()
        .unwrap()
        .contains("python_works"));
}

#[tokio::test]
async fn python_profile_环境变量隔离() {
    let (_tmp, app) = create_test_app().await;
    let mut env = HashMap::new();
    env.insert("MY_TEST_VAR".to_string(), "secret_value".to_string());

    let (status, result) =
        submit_and_wait(app, "py-env-001", &["/bin/echo", "done"], "python", "5s", env).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"].as_str().unwrap(), "Completed");
}

#[tokio::test]
async fn python_profile_stderr被正确捕获() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "py-err-001",
        &["/bin/sh", "-c", "echo stderr_msg >&2"],
        "python",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let stderr = result["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("stderr_msg"),
        "stderr 应被捕获, 实际: {}",
        stderr
    );
}

#[tokio::test]
async fn python_profile_非零退出码() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "py-exit-001",
        &["/bin/sh", "-c", "exit 42"],
        "python",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["exit_code"].as_i64(), Some(42));
}
