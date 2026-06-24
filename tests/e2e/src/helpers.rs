//! E2E 测试共享工具函数

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use tower::ServiceExt;

use sandbox_core::job::JobRequest;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use sandbox_server::api::{app, AppState};
use sandbox_server::scheduler::Scheduler;

/// 创建完整的 HTTP 测试 app（默认 max_concurrent=10）
pub async fn create_test_app() -> (tempfile::TempDir, Router) {
    create_test_app_with_concurrency(10).await
}

/// 创建指定并发数的 HTTP 测试 app
pub async fn create_test_app_with_concurrency(max: usize) -> (tempfile::TempDir, Router) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("failed to create runner");
    let scheduler = Arc::new(Scheduler::new(Arc::new(runner), max));
    let state = AppState {
        scheduler,
        config_path: std::path::PathBuf::new(),
    };
    (tmp, app(state))
}

/// 创建带自定义 profile 的 HTTP 测试 app
pub async fn create_test_app_with_profiles(
    profiles: Vec<sandbox_core::profile::SandboxProfile>,
) -> (tempfile::TempDir, Router) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let mut runner = SandboxRunner::new(&config)
        .await
        .expect("failed to create runner");
    for profile in profiles {
        runner.register_profile(profile);
    }
    let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 10));
    let state = AppState {
        scheduler,
        config_path: std::path::PathBuf::new(),
    };
    (tmp, app(state))
}

/// 创建原始 SandboxRunner（无 HTTP 层）
pub async fn create_test_runner() -> (tempfile::TempDir, SandboxRunner) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("failed to create runner");
    (tmp, runner)
}

/// 构建默认的 HTTP submit 请求（shell profile）
pub fn make_submit_request(job_id: &str, argv: &[&str]) -> Request<Body> {
    make_submit_request_with_profile(job_id, argv, "shell")
}

/// 构建指定 profile 的 HTTP submit 请求
pub fn make_submit_request_with_profile(
    job_id: &str,
    argv: &[&str],
    profile: &str,
) -> Request<Body> {
    let body = serde_json::json!({
        "job_id": job_id,
        "argv": argv,
        "profile_name": profile,
        "timeout": "5s",
        "custom_env": {},
    });
    Request::builder()
        .method("POST")
        .uri("/api/v1/submit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

/// 构建带超时的 HTTP submit 请求
pub fn make_submit_request_with_timeout(
    job_id: &str,
    argv: &[&str],
    profile: &str,
    timeout: &str,
) -> Request<Body> {
    let body = serde_json::json!({
        "job_id": job_id,
        "argv": argv,
        "profile_name": profile,
        "timeout": timeout,
        "custom_env": {},
    });
    Request::builder()
        .method("POST")
        .uri("/api/v1/submit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

/// 构建带自定义环境的 HTTP submit 请求
pub fn make_submit_request_with_env(
    job_id: &str,
    argv: &[&str],
    profile: &str,
    env: HashMap<String, String>,
) -> Request<Body> {
    let body = serde_json::json!({
        "job_id": job_id,
        "argv": argv,
        "profile_name": profile,
        "timeout": "5s",
        "custom_env": env,
    });
    Request::builder()
        .method("POST")
        .uri("/api/v1/submit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

/// 构建直接 JobRequest（不经过 HTTP）
pub fn make_job_request(job_id: &str, argv: &[&str]) -> JobRequest {
    make_job_request_with_profile(job_id, argv, "shell", Duration::from_secs(10))
}

/// 构建指定 profile 的 JobRequest
pub fn make_job_request_with_profile(
    job_id: &str,
    argv: &[&str],
    profile: &str,
    timeout: Duration,
) -> JobRequest {
    JobRequest {
        job_id: job_id.to_string(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        profile_name: profile.to_string(),
        timeout: Some(timeout),
        custom_env: HashMap::new(),
        stdin_data: None,
    }
}

/// 发送 oneshot 请求并解析 JSON 响应
pub async fn send_and_parse<T: serde::de::DeserializeOwned>(
    app: Router,
    req: Request<Body>,
) -> (axum::http::StatusCode, T) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let parsed: T = serde_json::from_slice(&body).unwrap();
    (status, parsed)
}

/// cr-018: 提交并等待完成（无 stdin）。委托 submit_and_wait_with_input。
pub async fn submit_and_wait(
    app: Router,
    job_id: &str,
    argv: &[&str],
    profile: &str,
    timeout: &str,
    env: HashMap<String, String>,
) -> (axum::http::StatusCode, serde_json::Value) {
    submit_and_wait_with_input(app, job_id, argv, profile, timeout, env, None).await
}

/// cr-018+#72: 提交并等待完成，可带 stdin（POST /jobs + 轮询 GET /jobs/{id}）。
/// 返回 (最终 HTTP 状态码, job JSON)。Done 时含 stdout/stderr/exit_code 等。
pub async fn submit_and_wait_with_input(
    app: Router,
    job_id: &str,
    argv: &[&str],
    profile: &str,
    timeout: &str,
    env: HashMap<String, String>,
    stdin: Option<&str>,
) -> (axum::http::StatusCode, serde_json::Value) {
    // POST /jobs（create）
    let body = serde_json::json!({
        "job_id": job_id,
        "argv": argv,
        "profile_name": profile,
        "timeout": timeout,
        "custom_env": env,
        "stdin": stdin,
    });
    let create_req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert!(
        create_resp.status() == axum::http::StatusCode::ACCEPTED
            || create_resp.status() == axum::http::StatusCode::OK,
        "create should return 202/200, actual: {}",
        create_resp.status()
    );

    // 轮询 GET /jobs/{id}（上限 10s）
    for _ in 0..200 {
        let get_req = Request::builder()
            .method("GET")
            .uri(format!("/api/v1/jobs/{}", job_id))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(get_req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if json["status"] != "Running" {
            return (status, json);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("submit_and_wait timeout: job {} did not finish within 10s", job_id);
}
