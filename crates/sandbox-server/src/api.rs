//! HTTP API 路由（cr-018 异步化）
//!
//! POST /api/v1/jobs             — 提交 job（异步，返回 job_id）
//! GET  /api/v1/jobs/{id}        — 查询 job 状态/结果
//! POST /api/v1/jobs/{id}/cancel — 取消 job
//! GET  /api/v1/status           — worker 状态
//! GET  /metrics                 — Prometheus 指标
//! GET  /health                  — 健康检查

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use sandbox_core::job::StreamEvent;

use crate::scheduler::Scheduler;

/// 应用共享状态
pub struct AppState {
    pub scheduler: Arc<Scheduler>,
    /// 配置文件路径，供 /api/v1/reload 重新加载
    pub config_path: PathBuf,
    /// cr-023: Bearer API key(None = 鉴权关)
    pub api_key: Option<String>,
}

/// 构建路由
pub fn app(state: AppState) -> Router {
    // cr-023: 全量挂鉴权中间件(api_key 作为中间件独立 state 传入);/health 在中间件内按路径放行。
    let api_key = state.api_key.clone();
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/api/v1/jobs", post(create_job))
        .route("/api/v1/jobs/{job_id}", get(get_job))
        .route("/api/v1/jobs/{job_id}/cancel", post(cancel_job))
        .route("/api/v1/status", get(status))
        .route("/api/v1/profiles", get(profiles))
        .route("/api/v1/reload", post(reload))
        .layer(middleware::from_fn_with_state(api_key, require_api_key))
        .with_state(Arc::new(state))
}

/// cr-023: 常量时间字节比较(防时序侧信道)。长度不同直接返回 false。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// cr-023: Bearer API key 鉴权中间件(state = api_key: Option<String>)。
/// - /health 放行(探活);
/// - api_key 为 None 时透传(鉴权关);
/// - 否则校验 `Authorization: Bearer <key>`(常量时间比较),不匹配 → 401。
async fn require_api_key(
    State(api_key): State<Option<String>>,
    req: Request,
    next: Next,
) -> Response {
    // /health 放行
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let Some(expected) = api_key.as_deref() else {
        return next.run(req).await;
    };
    let authed = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|tok| ct_eq(tok.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);
    if authed {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "unauthorized".into(),
            }),
        )
            .into_response()
    }
}

// ==================== Handlers ====================

/// cr-018+#76: /health 报告安全机制就绪等级（landlock/cgroup/seccomp + 磁盘水位）
#[derive(Debug, Serialize)]
struct HealthReport {
    status: String,
    landlock: LandlockHealth,
    cgroup: CgroupHealth,
    seccomp: bool,
    disk_watermark_ok: bool,
}

#[derive(Debug, Serialize)]
struct LandlockHealth {
    supported: bool,
    abi_version: u32,
}

#[derive(Debug, Serialize)]
struct CgroupHealth {
    available: bool,
    controllers: Vec<String>,
}

async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ws = state.scheduler.worker_status();
    Json(HealthReport {
        status: "ok".into(),
        landlock: LandlockHealth {
            supported: ws.capability_report.landlock.supported,
            abi_version: ws.capability_report.landlock.abi_version,
        },
        cgroup: CgroupHealth {
            available: ws.capability_report.cgroup.available,
            controllers: ws
                .capability_report
                .cgroup
                .controllers
                .iter()
                .map(|c| format!("{c:?}"))
                .collect(),
        },
        seccomp: ws.capability_report.seccomp_available,
        disk_watermark_ok: ws.disk_watermark_ok,
    })
}

/// Prometheus 指标端点
async fn metrics() -> impl IntoResponse {
    use prometheus::Encoder;
    // 确保所有 Lazy static 被触发（注册到默认 registry）
    let _ = &*crate::metrics::JOB_STARTED_TOTAL;
    let _ = &*crate::metrics::JOB_FINISHED_TOTAL;
    let _ = &*crate::metrics::JOB_TIMEOUT_TOTAL;
    let _ = &*crate::metrics::RUNNING_JOBS;
    let _ = &*crate::metrics::FORK_EXEC_DURATION;

    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        buffer,
    )
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    job_id: String,
    argv: Vec<String>,
    profile_name: String,
    timeout: Option<String>,
    custom_env: Option<std::collections::HashMap<String, String>>,
    /// cr-018+#72: 传递给子进程的 stdin（UTF-8 文本）
    stdin: Option<String>,
    /// cr-018+#77: dry-run 模式，只返回 profile 摘要不执行
    dry_run: Option<bool>,
}

/// cr-018+#77: dry-run 响应——展示 profile 将应用哪些限制（不执行）
#[derive(Debug, Serialize)]
struct DryRunSummary {
    profile: String,
    dry_run: bool,
    default_timeout_secs: u64,
    max_stdout_mb: u64,
    landlock: String,
    fail_closed: bool,
    /// cr-019: 出站白名单（空 = 零出站）
    egress_allowlist: Vec<EgressRuleView>,
    /// cr-022: 工作区聚合上限（MB）。None = 不限。
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_quota_mb: Option<u64>,
}

/// cr-019: dry-run / 响应中的单条出站规则视图
#[derive(Debug, Serialize)]
struct EgressRuleView {
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

#[derive(Debug, Serialize)]
struct CreateJobResponse {
    job_id: String,
    status: String,
}

/// cr-018: GET /jobs/{id} 查询响应。Running 时仅 job_id+status；Done 时含完整结果。
#[derive(Debug, Serialize)]
struct JobResponse {
    job_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timed_out: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// cr-024: POST /jobs?stream=true 的 query flag。
#[derive(Debug, Deserialize)]
struct StreamQuery {
    stream: Option<bool>,
}

/// cr-018: POST /jobs — 异步提交，立即返回 job_id（cr-024: ?stream=true → SSE 流式 stdout）
async fn create_job(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StreamQuery>,
    Json(req): Json<CreateJobRequest>,
) -> Response {
    // cr-018+#77: dry-run 模式——返回 profile 摘要，不执行任务
    if req.dry_run.unwrap_or(false) {
        return match state.scheduler.get_profile(&req.profile_name) {
            Some(profile) => (
                StatusCode::OK,
                Json(DryRunSummary {
                    profile: profile.name.clone(),
                    dry_run: true,
                    default_timeout_secs: profile.default_timeout.as_secs(),
                    max_stdout_mb: profile.max_stdout_bytes / 1024 / 1024,
                    landlock: format!("{:?}", profile.landlock_template),
                    fail_closed: profile.fail_closed,
                    egress_allowlist: profile
                        .egress_allowlist
                        .iter()
                        .map(|r| EgressRuleView {
                            host: r.host.clone(),
                            port: r.port,
                        })
                        .collect(),
                    disk_quota_mb: profile.disk_quota_mb,
                }),
            )
                .into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("profile not found: {}", req.profile_name),
                }),
            )
                .into_response(),
        };
    }

    let timeout = match req.timeout.as_deref() {
        Some(t) => match parse_duration(t) {
            Some(d) => Some(d),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("invalid timeout format: {t}"),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    let job_req = sandbox_core::job::JobRequest {
        job_id: req.job_id,
        argv: req.argv,
        profile_name: req.profile_name,
        timeout,
        custom_env: req.custom_env.unwrap_or_default(),
        stdin_data: req.stdin.map(|s| s.into_bytes()),
    };

    // cr-024: 流式模式 → text/event-stream(started → stdout… → result)
    if q.stream.unwrap_or(false) {
        let rx = state.scheduler.submit_streaming(job_req).await;
        let stream = ReceiverStream::new(rx)
            .map(|ev| -> Result<Event, std::convert::Infallible> {
                Ok(match ev {
                    StreamEvent::Started { job_id } => Event::default()
                        .event("started")
                        .json_data(serde_json::json!({ "job_id": job_id }))
                        .unwrap_or_default(),
                    StreamEvent::Stdout { data } => Event::default()
                        .event("stdout")
                        .json_data(serde_json::json!({ "data": data }))
                        .unwrap_or_default(),
                    StreamEvent::Result(r) => Event::default()
                        .event("result")
                        .json_data(&r)
                        .unwrap_or_default(),
                })
            });
        return Sse::new(stream).into_response();
    }

    let job_id = state.scheduler.submit_async(job_req).await;
    (
        StatusCode::ACCEPTED,
        Json(CreateJobResponse {
            job_id,
            status: "Running".into(),
        }),
    )
        .into_response()
}

/// cr-018: GET /jobs/{id} — 查询状态/结果
async fn get_job(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    use crate::scheduler::JobState;
    match state.scheduler.get_job(&job_id) {
        Some(JobState::Running) => (
            StatusCode::OK,
            Json(JobResponse {
                job_id,
                status: "Running".into(),
                exit_code: None,
                signal: None,
                stdout: None,
                stderr: None,
                duration_ms: None,
                timed_out: None,
            }),
        )
            .into_response(),
        Some(JobState::Done(result)) => (
            StatusCode::OK,
            Json(JobResponse {
                job_id: result.job_id,
                status: format!("{:?}", result.status),
                exit_code: result.exit_code,
                signal: result.signal,
                stdout: Some(crate::redact::redact(
                    &String::from_utf8_lossy(&result.stdout),
                )),
                stderr: Some(crate::redact::redact(
                    &String::from_utf8_lossy(&result.stderr),
                )),
                duration_ms: Some(result.duration.as_millis() as u64),
                timed_out: Some(result.timed_out),
            }),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "job not found or expired".into(),
            }),
        )
            .into_response(),
    }
}

/// cr-018: POST /jobs/{id}/cancel — 取消
async fn cancel_job(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    use crate::scheduler::CancelError;
    match state.scheduler.cancel_job(&job_id) {
        Ok(()) => (
            StatusCode::OK,
            Json(CreateJobResponse {
                job_id,
                status: "Cancelled".into(),
            }),
        )
            .into_response(),
        Err(CancelError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "job not found".into(),
            }),
        )
            .into_response(),
        Err(CancelError::AlreadyDone) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "job already finished, cannot cancel".into(),
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    running_jobs: usize,
    max_concurrent: usize,
    uptime_secs: u64,
}

async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ws = state.scheduler.worker_status();
    Json(StatusResponse {
        running_jobs: ws.running_jobs,
        max_concurrent: ws.max_concurrent,
        uptime_secs: ws.uptime.as_secs(),
    })
}

#[derive(Debug, Serialize)]
struct ProfilesResponse {
    profiles: Vec<String>,
}

async fn profiles(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(ProfilesResponse {
        profiles: state.scheduler.profile_names(),
    })
}

#[derive(Debug, Serialize)]
struct ReloadResponse {
    success: bool,
    profiles_loaded: Vec<String>,
    message: String,
}

/// 重新加载配置文件并热替换 Runner。
async fn reload(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let path = state.config_path.to_string_lossy().to_string();
    match crate::config::build_runner_from_config(&path).await {
        Ok((runner, profiles)) => {
            state.scheduler.reload(Arc::new(runner));
            (
                StatusCode::OK,
                Json(ReloadResponse {
                    success: true,
                    message: format!("reloaded {} profiles from {}", profiles.len(), path),
                    profiles_loaded: profiles,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ReloadResponse {
                success: false,
                message: format!("reload failed: {e}"),
                profiles_loaded: vec![],
            }),
        )
            .into_response(),
    }
}

/// 简易 duration 解析（支持 "5s", "100ms", "1m"）
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        let ms: u64 = num.parse().ok()?;
        return Some(Duration::from_millis(ms));
    }
    if let Some(num) = s.strip_suffix('s') {
        let secs: u64 = num.parse().ok()?;
        return Some(Duration::from_secs(secs));
    }
    if let Some(num) = s.strip_suffix('m') {
        let mins: u64 = num.parse().ok()?;
        return Some(Duration::from_secs(mins * 60));
    }
    let secs: u64 = s.parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
    use tower::ServiceExt;

    async fn make_app(tmp: &std::path::Path) -> Router {
        let config = SandboxConfig {
            sandbox_base_dir: tmp.to_path_buf(),
            disk_watermark_bytes: 0,
        };
        let runner = Arc::new(SandboxRunner::new(&config).await.unwrap());
        let scheduler = Arc::new(Scheduler::new(runner, 10));
        app(AppState {
            scheduler,
            config_path: std::path::PathBuf::new(),
            api_key: None,
        })
    }

    /// cr-023: 带可选 api_key 的测试 app。
    async fn make_app_with_key(tmp: &std::path::Path, key: Option<&str>) -> Router {
        let config = SandboxConfig {
            sandbox_base_dir: tmp.to_path_buf(),
            disk_watermark_bytes: 0,
        };
        let runner = Arc::new(SandboxRunner::new(&config).await.unwrap());
        let scheduler = Arc::new(Scheduler::new(runner, 10));
        app(AppState {
            scheduler,
            config_path: std::path::PathBuf::new(),
            api_key: key.map(String::from),
        })
    }

    // ==================== cr-023: 鉴权 ====================

    #[tokio::test]
    async fn auth_off_all_routes_accessible() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), None).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_on_missing_header_returns_401() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), Some("secret")).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_on_wrong_key_returns_401() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), Some("secret")).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/profiles")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_on_correct_key_returns_200() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), Some("secret")).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/profiles")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_on_health_open_without_header() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), Some("secret")).await;
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
    async fn auth_on_metrics_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_key(tmp.path(), Some("secret")).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// cr-024: POST /jobs?stream=true → text/event-stream,事件序 started/stdout/result。
    #[tokio::test]
    async fn create_job_stream_returns_sse_events() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let body = serde_json::json!({
            "job_id": "sse-1",
            "argv": ["/bin/echo", "line1"],
            "profile_name": "shell",
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs?stream=true")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.contains("text/event-stream"), "content-type={ct}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("event:started") || text.contains("event: started"),
            "no started event: {text}"
        );
        assert!(
            text.contains("event:stdout") || text.contains("event: stdout"),
            "no stdout event: {text}"
        );
        assert!(
            text.contains("event:result") || text.contains("event: result"),
            "no result event: {text}"
        );
        assert!(text.contains("line1"), "stdout missing line1: {text}");
    }

    #[tokio::test]
    async fn profiles_returns_builtin_list() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let mut profiles = json["profiles"]
            .as_array()
            .expect("profiles should be an array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        profiles.sort();
        assert_eq!(profiles, vec!["node", "python", "shell"]);
    }

    /// cr-018+#76: /health 报告安全机制就绪等级（landlock/cgroup/seccomp）
    #[tokio::test]
    async fn health_reports_security_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["landlock"]["supported"].is_boolean());
        assert!(json["landlock"]["abi_version"].is_number());
        assert!(json["cgroup"]["available"].is_boolean());
        assert!(json["cgroup"]["controllers"].is_array());
        assert!(json["seccomp"].is_boolean());
    }

    /// cr-018+#77: dry-run 返回 profile 摘要，不执行任务
    #[tokio::test]
    async fn create_job_dry_run_returns_profile_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let body = serde_json::json!({
            "job_id": "dry-1",
            "argv": ["/bin/echo", "x"],
            "profile_name": "shell",
            "dry_run": true,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["dry_run"], true);
        assert_eq!(json["profile"], "shell");
        assert!(json["default_timeout_secs"].as_u64().is_some());
        assert!(json["landlock"].as_str().unwrap().contains("Shell"));
    }

    /// cr-022 gap: dry_run 暴露 profile 的 disk_quota_mb。
    async fn make_app_with_quota_profile(tmp: &std::path::Path) -> Router {
        let config = SandboxConfig {
            sandbox_base_dir: tmp.to_path_buf(),
            disk_watermark_bytes: 0,
        };
        let mut runner = SandboxRunner::new(&config).await.unwrap();
        runner.register_profile(sandbox_core::profile::SandboxProfile {
            name: "quota_dry".to_string(),
            disk_quota_mb: Some(50),
            ..sandbox_core::profile::SandboxProfile::shell()
        });
        let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 10));
        app(AppState {
            scheduler,
            config_path: std::path::PathBuf::new(),
            api_key: None,
        })
    }

    #[tokio::test]
    async fn dry_run_surfaces_disk_quota_mb() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app_with_quota_profile(tmp.path()).await;
        let body = serde_json::json!({
            "job_id": "dry-quota",
            "argv": ["/bin/echo", "x"],
            "profile_name": "quota_dry",
            "dry_run": true,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["disk_quota_mb"], 50,
            "dry_run should surface disk_quota_mb: {json}"
        );
    }

    #[tokio::test]
    async fn reload_returns_new_profile_list() {
        let tmp = tempfile::tempdir().unwrap();
        let base_dir = tmp.path().join("sandboxes");
        let config_path = tmp.path().join("config.yaml");
        let yaml = format!(
            r#"
sandbox:
  base_dir: "{}"
  disk_watermark_mb: 0
profiles:
  custom_task:
    default_timeout: "10s"
"#,
            base_dir.display()
        );
        std::fs::write(&config_path, &yaml).unwrap();

        let runner = SandboxRunner::new(&SandboxConfig {
            sandbox_base_dir: base_dir.clone(),
            disk_watermark_bytes: 0,
        })
        .await
        .unwrap();
        let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 10));
        let app = app(AppState {
            scheduler,
            config_path: config_path.clone(),
            api_key: None,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/reload")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        let profiles: Vec<String> = json["profiles_loaded"]
            .as_array()
            .expect("profiles_loaded should be an array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            profiles.contains(&"custom_task".to_string()),
            "reload should include custom_task, actual: {:?}",
            profiles
        );
    }

    /// cr-018+#77: reload 时 profile 无效（timeout 格式错）应失败，不静默跳过
    #[tokio::test]
    async fn reload_invalid_profile_returns_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("bad.yaml");
        let yaml = r#"
sandbox:
  base_dir: "/tmp/sandbox-test-bad"
profiles:
  bad_task:
    default_timeout: "not-a-duration"
"#;
        std::fs::write(&config_path, yaml).unwrap();

        let runner = SandboxRunner::new(&SandboxConfig {
            sandbox_base_dir: tmp.path().join("s"),
            disk_watermark_bytes: 0,
        })
        .await
        .unwrap();
        let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 10));
        let app = app(AppState {
            scheduler,
            config_path: config_path.clone(),
            api_key: None,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/reload")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid profile should fail reload"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], false);
    }

    /// cr-018: GET /jobs/{不存在} → 404
    #[tokio::test]
    async fn get_job_missing_returns_404() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/jobs/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// cr-018: POST /jobs/{不存在}/cancel → 404
    #[tokio::test]
    async fn cancel_job_missing_returns_404() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs/nonexistent/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// cr-018+#77: dry-run profile 不存在 → 404
    #[tokio::test]
    async fn dry_run_missing_profile_returns_404() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let body = serde_json::json!({
            "job_id": "d",
            "argv": ["/bin/echo", "x"],
            "profile_name": "nonexistent",
            "dry_run": true,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// cr-019: dry-run 返回 profile 的 egress allowlist
    #[tokio::test]
    async fn dry_run_returns_egress_allowlist() {
        use sandbox_core::egress::EgressRule;
        let tmp = tempfile::tempdir().unwrap();
        let config = SandboxConfig {
            sandbox_base_dir: tmp.path().to_path_buf(),
            disk_watermark_bytes: 0,
        };
        let mut runner = SandboxRunner::new(&config).await.unwrap();
        runner.register_profile(sandbox_core::profile::SandboxProfile {
            name: "net_profile".into(),
            egress_allowlist: vec![EgressRule {
                host: "api.openai.com".into(),
                port: Some(443),
            }],
            ..sandbox_core::profile::SandboxProfile::python()
        });
        let scheduler = Arc::new(Scheduler::new(Arc::new(runner), 10));
        let app = app(AppState {
            scheduler,
            config_path: std::path::PathBuf::new(),
            api_key: None,
        });

        let body = serde_json::json!({
            "job_id": "d",
            "argv": ["/bin/echo", "x"],
            "profile_name": "net_profile",
            "dry_run": true,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let b = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&b).unwrap();
        let allowlist = j["egress_allowlist"]
            .as_array()
            .expect("should have egress_allowlist");
        assert_eq!(allowlist.len(), 1);
        assert_eq!(allowlist[0]["host"], "api.openai.com");
        assert_eq!(allowlist[0]["port"], 443);
    }
}
