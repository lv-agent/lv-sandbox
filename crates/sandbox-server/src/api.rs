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

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::scheduler::Scheduler;

/// 应用共享状态
pub struct AppState {
    pub scheduler: Arc<Scheduler>,
    /// 配置文件路径，供 /api/v1/reload 重新加载
    pub config_path: PathBuf,
}

/// 构建路由
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/api/v1/jobs", post(create_job))
        .route("/api/v1/jobs/{job_id}", get(get_job))
        .route("/api/v1/jobs/{job_id}/cancel", post(cancel_job))
        .route("/api/v1/status", get(status))
        .route("/api/v1/profiles", get(profiles))
        .route("/api/v1/reload", post(reload))
        .with_state(Arc::new(state))
}

// ==================== Handlers ====================

async fn health() -> &'static str {
    "ok"
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

/// cr-018: POST /jobs — 异步提交，立即返回 job_id
async fn create_job(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateJobRequest>,
) -> impl IntoResponse {
    let timeout = match req.timeout.as_deref() {
        Some(t) => match parse_duration(t) {
            Some(d) => Some(d),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("无效的 timeout 格式: {t}"),
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
                stdout: Some(String::from_utf8_lossy(&result.stdout).to_string()),
                stderr: Some(String::from_utf8_lossy(&result.stderr).to_string()),
                duration_ms: Some(result.duration.as_millis() as u64),
                timed_out: Some(result.timed_out),
            }),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "任务不存在或已过期".into(),
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
                error: "任务不存在".into(),
            }),
        )
            .into_response(),
        Err(CancelError::AlreadyDone) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "任务已完成，无法取消".into(),
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
                    message: format!("从 {} 重新加载 {} 个 profile", path, profiles.len()),
                    profiles_loaded: profiles,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ReloadResponse {
                success: false,
                message: format!("reload 失败: {e}"),
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
        })
    }

    #[tokio::test]
    async fn profiles_返回内置profile列表() {
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
            .expect("profiles 应为数组")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        profiles.sort();
        assert_eq!(profiles, vec!["node", "python", "shell"]);
    }

    #[tokio::test]
    async fn reload_重新加载配置返回新profile列表() {
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
            .expect("profiles_loaded 应为数组")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            profiles.contains(&"custom_task".to_string()),
            "reload 后应包含 custom_task, 实际: {:?}",
            profiles
        );
    }
}
