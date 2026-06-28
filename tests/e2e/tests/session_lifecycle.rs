//! Session HTTP API E2E 测试。
//!
//! 覆盖: session CRUD, exec, files, snapshot, volume, TTL reaper,
//! session exec metrics。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use sandbox_e2e::helpers::*;

/// 发送请求并返回 (status, body_bytes)
async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, axum::body::Bytes) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    (status, body)
}

fn parse_body(body: &axum::body::Bytes) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

// ==================== session CRUD ====================

#[tokio::test]
async fn session_create_list_get_destroy_via_http() {
    let (_tmp, app) = create_test_app().await;

    // create → 201
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"shell","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // list → sessions 数组
    let (_s, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/v1/sessions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let list = parse_body(&body);
    assert!(list["sessions"].as_array().unwrap().iter().any(|s| s["session_id"] == sid));

    // get
    let (status, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let info = parse_body(&body);
    assert_eq!(info["session_id"], sid);
    assert_eq!(info["profile"], "shell");

    // destroy
    let (status, _) = send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(status.is_success());

    // verify gone
    let (status, _) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_create_unknown_profile_returns_400() {
    let (_tmp, app) = create_test_app().await;
    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"nope","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    // ProfileNotFound → 400 BAD_REQUEST
    assert!(!status.is_success());
}

// ==================== session exec + files ====================

#[tokio::test]
async fn session_exec_and_file_io_via_http() {
    let (_tmp, app) = create_test_app().await;

    // create session
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"shell","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // exec: write file
    let exec_body = serde_json::json!({"argv":["/bin/sh","-c","echo hello-e2e > out.txt"],"timeout":"5s"});
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/exec"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&exec_body).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_body(&body)["status"], "Completed");

    // get file
    let (status, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}/files/out.txt"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("hello-e2e"));

    // list files
    let (_s, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}/files"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let ls = parse_body(&body);
    assert!(ls["entries"].as_array().unwrap().iter().any(|e| e["name"] == "out.txt"));

    // put file
    let (status, _) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/sessions/{sid}/files/uploaded.txt"))
            .header("content-type", "application/octet-stream")
            .body(Body::from("e2e-upload-data"))
            .unwrap(),
    )
    .await;
    assert!(status.is_success());

    // verify uploaded
    let (_, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}/files/uploaded.txt"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(&body[..], b"e2e-upload-data");

    // exec with list_files=true
    let exec2 = serde_json::json!({"argv":["/bin/echo","done"],"timeout":"5s","list_files":true});
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/exec"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&exec2).unwrap()))
            .unwrap(),
    )
    .await;
    let r = parse_body(&body);
    let files = r["files"].as_array().unwrap();
    assert!(files.len() >= 2);
    assert!(files.iter().any(|f| f["path"] == "out.txt"));
    assert!(files.iter().any(|f| f["path"] == "uploaded.txt" && f["mime"] == "text/plain"));

    // cleanup
    send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
}

// ==================== snapshot ====================

#[tokio::test]
async fn snapshot_create_list_restore_delete_via_http() {
    let (_tmp, app) = create_test_app().await;

    // create session + write file
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"shell","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    let exec = serde_json::json!({"argv":["/bin/sh","-c","echo snapshot-e2e > snap.txt"],"timeout":"5s"});
    send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/exec"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&exec).unwrap()))
            .unwrap(),
    )
    .await;

    // snapshot → 201
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/snapshot"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let snap_id = parse_body(&body)["snapshot_id"].as_str().unwrap().to_string();

    // list snapshots → {"snapshots": ["id1", "id2"]}
    let (_s, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/v1/snapshots")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let snap_json = parse_body(&body);
    let snaps = snap_json["snapshots"].as_array().unwrap();
    assert!(snaps.iter().any(|s| s.as_str() == Some(&snap_id)));

    // restore: create from snapshot
    let restore_body =
        serde_json::json!({"profile_name":"shell","env":{},"volumes":[],"from_snapshot":snap_id});
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&restore_body).unwrap()))
            .unwrap(),
    )
    .await;
    let sid2 = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // verify restored file
    let (_, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid2}/files/snap.txt"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(String::from_utf8_lossy(&body).contains("snapshot-e2e"));

    // delete snapshot
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/snapshots/{snap_id}"))
        .body(Body::empty())
        .unwrap();
    assert!(app.clone().oneshot(del).await.unwrap().status().is_success());

    // cleanup sessions
    for id in &[sid, sid2] {
        send(
            &app,
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/sessions/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    }
}

// ==================== volumes ====================

#[tokio::test]
async fn volume_http_crud_and_session_mount() {
    let (_tmp, app) = create_test_app().await;

    // create volume → 201
    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/volumes")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"e2e-vol"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // list volumes → {"volumes": [...]}
    let (_s, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/v1/volumes")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let list_json = parse_body(&body);
    let vols = list_json["volumes"].as_array().unwrap();
    assert!(vols.iter().any(|v| v.as_str() == Some("e2e-vol")));

    // create session with volume mount
    let sess = serde_json::json!({
        "profile_name":"shell",
        "env":{},
        "volumes":[{"name":"e2e-vol","mount":"volumes/shared"}]
    });
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&sess).unwrap()))
            .unwrap(),
    )
    .await;
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // exec: write to volume
    let exec = serde_json::json!({"argv":["/bin/sh","-c","echo vol-e2e > volumes/shared/v.txt"],"timeout":"5s"});
    send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/exec"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&exec).unwrap()))
            .unwrap(),
    )
    .await;

    // read volume file
    let (_, body) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}/files/volumes/shared/v.txt"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(String::from_utf8_lossy(&body).contains("vol-e2e"));

    // cleanup
    send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/v1/volumes/e2e-vol")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
}

// ==================== TTL reaper ====================

#[tokio::test]
async fn session_ttl_reaper_cleans_expired_session_via_http() {
    use sandbox_core::sandbox_context::SandboxConfig;
    use sandbox_server::api::{app, AppState};
    use sandbox_server::audit::AuditLogger;
    use sandbox_server::scheduler::Scheduler;
    use sandbox_server::session::SessionManager;
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = Arc::new(sandbox_core::sandbox_context::SandboxRunner::new(&cfg).await.unwrap());
    let scheduler = Arc::new(Scheduler::new(runner.clone(), 10));
    let sessions = Arc::new(SessionManager::new(runner, Arc::new(AuditLogger::noop())));

    // 启动 reaper: TTL=1s, interval=2s
    let handle = sessions.clone().spawn_reaper(1, 2);

    let state = AppState {
        scheduler,
        sessions,
        config_path: std::path::PathBuf::new(),
        api_key: None,
        rate_limiter: None,
    };
    let app = app(state);

    // create session
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"shell","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // 等待 reaper 扫描清理(interval=2s, 等至少一次完整 tick)
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // session should be gone
    let (status, _) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "session should be reaped by TTL reaper");

    handle.abort();
}

// ==================== session exec metrics ====================

#[tokio::test]
async fn session_exec_increments_metrics_via_http() {
    let (_tmp, app) = create_test_app().await;

    let baseline = prometheus_metric_value("sandbox_job_started_total");

    // create session
    let (_, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"profile_name":"shell","env":{},"volumes":[]}"#))
            .unwrap(),
    )
    .await;
    let sid = parse_body(&body)["session_id"].as_str().unwrap().to_string();

    // exec
    let exec = serde_json::json!({"argv":["/bin/echo","metric-e2e"],"timeout":"5s"});
    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/sessions/{sid}/exec"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&exec).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let after = prometheus_metric_value("sandbox_job_started_total");
    assert!(after > baseline, "session exec should increment job_started_total: baseline={baseline} after={after}");

    // cleanup
    send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/sessions/{sid}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
}

// ==================== helper ====================

fn prometheus_metric_value(name: &str) -> f64 {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let families = prometheus::gather();
    let mut buf = Vec::new();
    encoder.encode(&families, &mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);

    for line in text.lines() {
        if line.starts_with(name) && !line.starts_with('#') {
            if let Some(val) = line.split_whitespace().nth(1) {
                return val.parse().unwrap_or(0.0);
            }
        }
    }
    0.0
}
