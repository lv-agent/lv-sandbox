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

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use sandbox_core::job::StreamEvent;

use crate::scheduler::Scheduler;
use crate::session::{SessionManager, VolumeMount};

/// 应用共享状态
pub struct AppState {
    pub scheduler: Arc<Scheduler>,
    /// cr-026: 会话管理器
    pub sessions: Arc<SessionManager>,
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
        // cr-026: 会话 + 文件 I/O
        .route("/api/v1/sessions", post(create_session).get(list_sessions))
        .route(
            "/api/v1/sessions/{id}",
            get(get_session).delete(destroy_session),
        )
        .route("/api/v1/sessions/{id}/exec", post(exec_session))
        .route("/api/v1/sessions/{id}/tty", get(crate::tty::session_tty))
        .route("/api/v1/sessions/{id}/snapshot", post(snapshot_session))
        .route("/api/v1/sessions/{id}/files", get(list_files))
        .route(
            "/api/v1/sessions/{id}/files/{*path}",
            get(get_file).put(put_file).delete(delete_file),
        )
        // cr-027: 快照
        .route("/api/v1/snapshots", get(list_snapshots))
        .route("/api/v1/snapshots/{id}", delete(destroy_snapshot))
        // cr-028: 卷
        .route("/api/v1/volumes", post(create_volume).get(list_volumes))
        .route("/api/v1/volumes/{name}", delete(destroy_volume))
        .layer(middleware::from_fn_with_state(api_key, require_api_key))
        // cr-026: 放宽 body 上限供文件上传(默认 2MB 太小);JSON 端点不受影响(体小)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
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
    /// cr-034: 工作区文件清单(list_files=true 时)
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<FileMeta>>,
}

/// cr-034: 文件元数据(MIME from extension)。
#[derive(Debug, Serialize)]
struct FileMeta {
    path: String,
    size: u64,
    mime: String,
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
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
                files: None,
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
                files: None,
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

// ==================== cr-026: 会话 + 文件 I/O ====================

/// cr-026: CoreError → HTTP 状态(404 not found / 400 路径违规 / 500 其他)。
fn core_err_response(e: &sandbox_core::error::CoreError) -> Response {
    use sandbox_core::error::CoreError;
    let (code, msg) = match e {
        CoreError::Io(ioe) if ioe.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, e.to_string())
        }
        CoreError::Workspace(m) if m.contains("not found") => (StatusCode::NOT_FOUND, m.clone()),
        CoreError::Workspace(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        CoreError::ProfileNotFound(_) => (StatusCode::BAD_REQUEST, e.to_string()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    (code, Json(ErrorResponse { error: msg })).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    profile_name: String,
    #[serde(default)]
    env: Option<std::collections::HashMap<String, String>>,
    /// cr-027: 从快照恢复(fork)。None = 空工作区。
    #[serde(default)]
    from_snapshot: Option<String>,
    /// cr-028: 挂载持久卷。
    #[serde(default)]
    volumes: Option<Vec<VolumeMount>>,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: String,
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Response {
    match state.sessions.create_session(
        &req.profile_name,
        req.env.unwrap_or_default(),
        req.from_snapshot,
        req.volumes.unwrap_or_default(),
    ) {
        Ok(id) => (
            StatusCode::CREATED,
            Json(CreateSessionResponse { session_id: id }),
        )
            .into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "sessions": state.sessions.list_sessions() }))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.sessions.get_session(&id) {
        Some(info) => Json(info).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("session not found: {id}"),
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Serialize)]
struct DestroySessionResponse {
    ok: bool,
    session_id: String,
}

async fn destroy_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.sessions.destroy_session(&id) {
        Ok(()) => Json(DestroySessionResponse { ok: true, session_id: id }).into_response(),
        Err(e) => core_err_response(&e),
    }
}

#[derive(Debug, Deserialize)]
struct ExecSessionRequest {
    argv: Vec<String>,
    timeout: Option<String>,
    stdin: Option<String>,
    #[serde(default)]
    custom_env: Option<std::collections::HashMap<String, String>>,
    /// cr-034: exec 后附带工作区文件清单(MIME 检测)。默认关。
    #[serde(default)]
    list_files: Option<bool>,
}

async fn exec_session(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StreamQuery>,
    Path(id): Path<String>,
    Json(req): Json<ExecSessionRequest>,
) -> Response {
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
    let list_files = req.list_files.unwrap_or(false);
    let job_req = sandbox_core::job::JobRequest {
        job_id: format!("session:{id}"),
        argv: req.argv,
        profile_name: String::new(), // 会话 exec 用绑定 profile,此项忽略
        timeout,
        custom_env: req.custom_env.unwrap_or_default(),
        stdin_data: req.stdin.map(|s| s.into_bytes()),
    };

    // cr-026: 流式 → SSE(复用 cr-024 事件映射)
    if q.stream.unwrap_or(false) {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);
        let sessions = state.sessions.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let sid = id.clone();
        tokio::spawn(async move {
            let _ = sessions.exec_session(&sid, job_req, cancel, Some(tx)).await;
        });
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

    // 非流式 → 等结果(复用 JobResponse 形状 + 脱敏)
    match state
        .sessions
        .exec_session(&id, job_req, tokio_util::sync::CancellationToken::new(), None)
        .await
    {
        Ok(r) => Json(JobResponse {
            job_id: r.job_id,
            status: format!("{:?}", r.status),
            exit_code: r.exit_code,
            signal: r.signal,
            stdout: Some(crate::redact::redact(&String::from_utf8_lossy(&r.stdout))),
            stderr: Some(crate::redact::redact(&String::from_utf8_lossy(&r.stderr))),
            duration_ms: Some(r.duration.as_millis() as u64),
            timed_out: Some(r.timed_out),
            files: if list_files {
                state
                    .sessions
                    .list_files(&id, "")
                    .ok()
                    .map(|entries| {
                        entries
                            .into_iter()
                            .map(|e| {
                                let mime = mime_for(&e.name).to_string();
                                FileMeta { path: e.name, size: e.size, mime }
                            })
                            .collect()
                    })
            } else {
                None
            },
        })
        .into_response(),
        Err(e) => core_err_response(&e),
    }
}

#[derive(Debug, Deserialize)]
struct ListFilesQuery {
    path: Option<String>,
}

async fn list_files(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<ListFilesQuery>,
) -> Response {
    match state
        .sessions
        .list_files(&id, q.path.as_deref().unwrap_or(""))
    {
        Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn put_file(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    bytes: Bytes,
) -> Response {
    match state.sessions.put_file(&id, &path, &bytes) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn get_file(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    match state.sessions.get_file(&id, &path) {
        Ok(data) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            data,
        )
            .into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn delete_file(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    match state.sessions.delete_file(&id, &path) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => core_err_response(&e),
    }
}

// ==================== cr-027: 快照 ====================

#[derive(Debug, Serialize)]
struct SnapshotResponse {
    snapshot_id: String,
}

async fn snapshot_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.sessions.snapshot_session(&id).await {
        Ok(snap_id) => (
            StatusCode::CREATED,
            Json(SnapshotResponse {
                snapshot_id: snap_id,
            }),
        )
            .into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn list_snapshots(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ids = state.sessions.list_snapshots().unwrap_or_default();
    Json(serde_json::json!({ "snapshots": ids }))
}

async fn destroy_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.sessions.destroy_snapshot(&id) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "snapshot_id": id })).into_response(),
        Err(e) => core_err_response(&e),
    }
}

// ==================== cr-028: 卷 ====================

#[derive(Debug, Deserialize)]
struct CreateVolumeRequest {
    name: String,
}

async fn create_volume(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateVolumeRequest>,
) -> Response {
    match state.sessions.create_volume(&req.name) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "volume_name": req.name })),
        )
            .into_response(),
        Err(e) => core_err_response(&e),
    }
}

async fn list_volumes(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "volumes": state.sessions.list_volumes().unwrap_or_default() }))
}

async fn destroy_volume(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.sessions.cleanup_volume(&name) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "volume_name": name })).into_response(),
        Err(e) => core_err_response(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
    use tower::ServiceExt;

    /// cr-026: 测试用——从 runner 建 SessionManager(noop audit)。
    fn build_sessions(runner: Arc<SandboxRunner>) -> Arc<SessionManager> {
        Arc::new(SessionManager::new(
            runner,
            Arc::new(crate::audit::AuditLogger::noop()),
        ))
    }

    async fn make_app(tmp: &std::path::Path) -> Router {
        let config = SandboxConfig {
            sandbox_base_dir: tmp.to_path_buf(),
            disk_watermark_bytes: 0,
        };
        let runner = Arc::new(SandboxRunner::new(&config).await.unwrap());
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        app(AppState {
            scheduler,
            sessions,
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
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        app(AppState {
            scheduler,
            sessions,
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

    // ==================== cr-026: 会话 + 文件 I/O 端到端 ====================

    async fn create_test_session(app: Router) -> String {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"profile_name":"shell"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bod = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        serde_json::from_slice::<serde_json::Value>(&bod)
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn session_roundtrip_upload_exec_download_destroy() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let sid = create_test_session(app.clone()).await;

        // 上传
        let put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/sessions/{sid}/files/hello.txt"))
                    .header("content-type", "application/octet-stream")
                    .body(Body::from(&b"from file"[..]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);

        // exec 读同一文件(证明持久工作区)
        let exec = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/sessions/{sid}/exec"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"argv":["/bin/cat","hello.txt"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exec.status(), StatusCode::OK);
        let eb = axum::body::to_bytes(exec.into_body(), 8192).await.unwrap();
        let ej: serde_json::Value = serde_json::from_slice(&eb).unwrap();
        assert!(
            ej["stdout"].as_str().unwrap().contains("from file"),
            "exec stdout: {ej}"
        );

        // 下载
        let get = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/sessions/{sid}/files/hello.txt"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let gb = axum::body::to_bytes(get.into_body(), 8192).await.unwrap();
        assert_eq!(&gb[..], b"from file");

        // 销毁
        let del = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/sessions/{sid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn session_file_traversal_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let sid = create_test_session(app.clone()).await;
        // URL 编码的 .. → axum 解码为 .. → sanitize 拒 → 400
        let put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/sessions/{sid}/files/%2e%2e/evil"))
                    .body(Body::from(&b"x"[..]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            put.status() == StatusCode::BAD_REQUEST || put.status() == StatusCode::NOT_FOUND,
            "traversal should be rejected, got {}",
            put.status()
        );
    }

    #[tokio::test]
    async fn session_snapshot_restore_via_http() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let sid = create_test_session(app.clone()).await;
        // 上传文件
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/sessions/{sid}/files/data.txt"))
                    .body(Body::from(&b"snapshot me"[..]))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 快照
        let sr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/sessions/{sid}/snapshot"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(sr.status(), StatusCode::CREATED);
        let sb = axum::body::to_bytes(sr.into_body(), 4096).await.unwrap();
        let snap_id = serde_json::from_slice::<serde_json::Value>(&sb).unwrap()["snapshot_id"]
            .as_str()
            .unwrap()
            .to_string();
        // list 快照
        let lr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/snapshots")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let lb = axum::body::to_bytes(lr.into_body(), 4096).await.unwrap();
        assert!(String::from_utf8_lossy(&lb).contains(&snap_id), "list: {}", String::from_utf8_lossy(&lb));
        // 从快照建新会话
        let cr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        format!(r#"{{"profile_name":"shell","from_snapshot":"{snap_id}"}}"#),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cr.status(), StatusCode::CREATED);
        let cb = axum::body::to_bytes(cr.into_body(), 4096).await.unwrap();
        let sid2 = serde_json::from_slice::<serde_json::Value>(&cb).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        // 新会话含快照文件
        let dl = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/sessions/{sid2}/files/data.txt"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let db = axum::body::to_bytes(dl.into_body(), 4096).await.unwrap();
        assert_eq!(&db[..], b"snapshot me");
        // 删快照
        let dr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/snapshots/{snap_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dr.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn volumes_crud_via_http() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        // create volume
        let cr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/volumes")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"data"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cr.status(), StatusCode::CREATED);
        // list
        let lr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/volumes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let lb = axum::body::to_bytes(lr.into_body(), 4096).await.unwrap();
        assert!(String::from_utf8_lossy(&lb).contains("data"), "list: {}", String::from_utf8_lossy(&lb));
        // create session mounting the volume(验证 volumes 字段被接受)
        let sr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"profile_name":"shell","volumes":[{"name":"data","mount":"volumes/data"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(sr.status(), StatusCode::CREATED);
        // delete volume
        let dr = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/volumes/data")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dr.status(), StatusCode::OK);
    }

    /// cr-034: exec list_files=true → 响应含工作区文件清单 + MIME。
    #[tokio::test]
    async fn exec_list_files_returns_workspace_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let sid = create_test_session(app.clone()).await;

        // 上传一个 PNG
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/sessions/{sid}/files/chart.png"))
                    .body(Body::from(&b"fake-png"[..]))
                    .unwrap(),
            )
            .await
            .unwrap();

        // exec 带 list_files=true
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/sessions/{sid}/exec"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"argv":["/bin/echo","hi"],"list_files":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["files"].is_array(), "files should be present: {json}");
        let files = json["files"].as_array().unwrap();
        assert!(
            files.iter().any(|f| f["path"] == "chart.png" && f["mime"] == "image/png"),
            "chart.png with image/png expected: {files:?}"
        );
    }

    /// cr-033: PTY WebSocket 端到端——连接 tty → 收到 echo 输出 + exit 控制消息。
    #[tokio::test]
    async fn tty_websocket_echoes_output() {
        use tokio_tungstenite::tungstenite::Message;

        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;
        let sid = create_test_session(app.clone()).await;

        // 起真 server(WS upgrade 需要真实 TCP 连接)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // 连 WebSocket
        let url = format!(
            "ws://{addr}/api/v1/sessions/{sid}/tty?argv=/bin/echo+hello-tty"
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("WS connect");

        let mut got_output = false;
        let mut got_exit = false;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(data) => {
                    if String::from_utf8_lossy(&data).contains("hello-tty") {
                        got_output = true;
                    }
                }
                Message::Text(s) => {
                    if s.contains("\"type\":\"exit\"") {
                        got_exit = true;
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        assert!(got_output, "should receive echo output via PTY");
        assert!(got_exit, "should receive exit control message");
    }

    /// cr-033 gap: tty WebSocket 连不存在的 session → 收到 error 控制消息。
    #[tokio::test]
    async fn tty_websocket_missing_session_error() {
        use tokio_tungstenite::tungstenite::Message;

        let tmp = tempfile::tempdir().unwrap();
        let app = make_app(tmp.path()).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let url = format!(
            "ws://{addr}/api/v1/sessions/nonexistent-session/tty?argv=/bin/echo+hi"
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("WS connect");

        let mut got_error = false;
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(s) = msg {
                if s.contains("\"type\":\"error\"") {
                    got_error = true;
                    break;
                }
            }
        }
        assert!(got_error, "should receive error for missing session");
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
        let runner = Arc::new(runner);
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        app(AppState {
            scheduler,
            sessions,
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
        let runner = Arc::new(runner);
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        let app = app(AppState {
            scheduler,
            sessions,
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
        let runner = Arc::new(runner);
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        let app = app(AppState {
            scheduler,
            sessions,
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
        let runner = Arc::new(runner);
        let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
        let sessions = build_sessions(runner);
        let app = app(AppState {
            scheduler,
            sessions,
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
