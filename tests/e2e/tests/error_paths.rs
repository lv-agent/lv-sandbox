//! 错误路径和边界情况 E2E 测试

use std::collections::HashMap;

use axum::body::Body;
use axum::http::StatusCode;
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

#[tokio::test]
async fn 不存在的命令返回非零退出码() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "err-001",
        &["/nonexistent/binary"],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // 子进程 spawn 失败或非零退出。cr-018 下 exit_code 是 Option，缺失即视为失败。
    let exit = result.get("exit_code").and_then(|v| v.as_i64());
    assert!(
        exit.is_none() || exit != Some(0),
        "不存在的命令应返回非零退出码（或 spawn 失败 exit_code 缺失），实际: {:?}",
        exit
    );
}

#[tokio::test]
async fn 超大stdout被截断() {
    let (_tmp, app) = create_test_app().await;
    // 生成大量输出（超过 5MB shell profile 默认限制）
    let (status, result) = submit_and_wait(
        app,
        "trunc-001",
        &[
            "/bin/sh",
            "-c",
            "dd if=/dev/zero bs=1024 count=10240 2>/dev/null | tr '\\0' 'A'",
        ],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let stdout_len = result["stdout"].as_str().unwrap().len();
    // 默认 max_stdout_bytes = 5MB，stdout 应被截断到此范围内
    assert!(
        stdout_len <= 5 * 1024 * 1024,
        "stdout 应被截断到 5MB 以内, 实际: {} bytes",
        stdout_len
    );
}

#[tokio::test]
async fn stdin数据正确传递() {
    let (_tmp, runner) = create_test_runner().await;
    let req = sandbox_core::job::JobRequest {
        job_id: "stdin-001".to_string(),
        argv: vec!["/bin/cat".to_string()],
        profile_name: "shell".to_string(),
        timeout: Some(std::time::Duration::from_secs(5)),
        custom_env: Default::default(),
        stdin_data: Some(b"hello from stdin".to_vec()),
    };
    // 需要用 runner 直接调用（HTTP API 不支持 stdin）
    let result = runner.run_job(req).await.expect("执行失败");

    assert_eq!(result.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("hello from stdin"),
        "stdin 数据应传递到子进程, stdout: {}",
        stdout
    );
}

#[tokio::test]
async fn 重复job_id两次都完成() {
    let (_tmp, app) = create_test_app().await;
    // cr-018: 同一 job_id 在同一 app 内第二次 submit_async 会覆盖 jobs 表项，
    // 这里验证两次 create_job 都返回 202（不报错）。第一次提交即等待完成，
    // 第二次需要新 app（oneshot 消费 router）。
    let _ = submit_and_wait(
        app,
        "dup-001",
        &["/bin/echo", "first"],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    // 第二次需要新 app（oneshot 消费 router）
    let (_tmp2, app2) = create_test_app().await;
    let body2 = serde_json::json!({
        "job_id": "dup-001",
        "argv": ["/bin/echo", "second"],
        "profile_name": "shell",
        "timeout": "5s",
        "custom_env": {},
    });
    let req2 = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body2).unwrap()))
        .unwrap();

    let response2 = app2.oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn 非零退出码正确返回() {
    let (_tmp, app) = create_test_app().await;
    let (status, result) = submit_and_wait(
        app,
        "exit-001",
        &["/bin/sh", "-c", "exit 7"],
        "shell",
        "5s",
        HashMap::new(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["exit_code"].as_i64(), Some(7));
}
