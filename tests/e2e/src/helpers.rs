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
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("创建 runner 失败");
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
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let mut runner = SandboxRunner::new(&config)
        .await
        .expect("创建 runner 失败");
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
    let tmp = tempfile::tempdir().expect("创建临时目录失败");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("创建 runner 失败");
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
