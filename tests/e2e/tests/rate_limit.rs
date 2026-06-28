//! cr-042 速率限制 E2E 测试

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use std::sync::Arc;
use tower::ServiceExt;

use sandbox_core::sandbox_context::SandboxConfig;
use sandbox_server::api::{app, AppState};
use sandbox_server::audit::AuditLogger;
use sandbox_server::config::RateLimitConfig;
use sandbox_server::ratelimit::RateLimiter;
use sandbox_server::scheduler::Scheduler;
use sandbox_server::session::SessionManager;

/// 创建带速率限制的测试 app
async fn create_rate_limited_app(
    reqs_per_window: u64,
    window_secs: u64,
) -> (tempfile::TempDir, Router) {
    let tmp = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = Arc::new(sandbox_core::sandbox_context::SandboxRunner::new(&config).await.unwrap());
    let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
    let sessions = Arc::new(SessionManager::new(runner, Arc::new(AuditLogger::noop())));
    let rate_limiter = Some(Arc::new(RateLimiter::new(&RateLimitConfig {
        enabled: true,
        requests_per_window: reqs_per_window,
        window_secs,
    })));
    let state = AppState {
        scheduler,
        sessions,
        config_path: std::path::PathBuf::new(),
        api_key: None,
        rate_limiter,
    };
    (tmp, app(state))
}

/// 发送带 X-Forwarded-For 的 GET 请求到指定路径
async fn send_with_ip(
    app: &Router,
    uri: &str,
    ip: &str,
) -> (StatusCode, axum::body::Bytes) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-forwarded-for", ip)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    (status, body)
}

/// 速率限制启用后超限返回 429(通过 /metrics 端点测试,非 /health)
#[tokio::test]
async fn rate_limit_returns_429_when_exceeded() {
    let (_tmp, app) = create_rate_limited_app(3, 60).await;

    // 前 3 次放行(/metrics 端点不在豁免名单)
    for _ in 0..3 {
        let (status, _) = send_with_ip(&app, "/api/v1/status", "10.0.0.99").await;
        assert_eq!(status, StatusCode::OK, "first 3 requests should be allowed");
    }

    // 第 4 次被限
    let (status, body) = send_with_ip(&app, "/api/v1/status", "10.0.0.99").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(String::from_utf8_lossy(&body).contains("rate limit exceeded"));
}

/// /health 豁免速率限制,但其他端点受限制
#[tokio::test]
async fn health_endpoint_exempt_from_rate_limit() {
    let (_tmp, app) = create_rate_limited_app(1, 60).await;

    // 先打满限额(用 /metrics)
    for _ in 0..2 {
        send_with_ip(&app, "/api/v1/status", "10.0.0.100").await;
    }
    // /metrics 应该被限
    let (status, _) = send_with_ip(&app, "/api/v1/status", "10.0.0.100").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "non-exempt endpoint should be limited");

    // /health 仍然放行(豁免)
    let (status, _) = send_with_ip(&app, "/health", "10.0.0.100").await;
    assert_eq!(status, StatusCode::OK, "/health should be exempt from rate limiting");
}

/// 速率限制计数 per-IP 独立
#[tokio::test]
async fn rate_limit_handles_different_ips_independently() {
    let (_tmp, app) = create_rate_limited_app(2, 60).await;

    // IP A 用满限额
    for _ in 0..2 {
        send_with_ip(&app, "/api/v1/status", "10.0.0.1").await;
    }
    let (status, _) = send_with_ip(&app, "/api/v1/status", "10.0.0.1").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "IP A should be limited after 2");

    // IP B 仍可访问(独立计数)
    let (status, _) = send_with_ip(&app, "/api/v1/status", "10.0.0.2").await;
    assert_eq!(status, StatusCode::OK, "IP B should have independent quota");
}

/// 速率限制默认关时不影响正常请求
#[tokio::test]
async fn rate_limit_disabled_does_not_block() {
    let tmp = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = Arc::new(sandbox_core::sandbox_context::SandboxRunner::new(&config).await.unwrap());
    let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
    let sessions = Arc::new(SessionManager::new(runner, Arc::new(AuditLogger::noop())));
    let state = AppState {
        scheduler,
        sessions,
        config_path: std::path::PathBuf::new(),
        api_key: None,
        rate_limiter: None,
    };
    let app = app(state);

    // 大量请求应全部放行
    for i in 0..10 {
        let (status, _) = send_with_ip(&app, "/api/v1/status", &format!("10.0.0.{}", i % 5)).await;
        assert_eq!(status, StatusCode::OK, "disabled rate limit should allow all");
    }
}
