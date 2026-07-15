//! cr-085 M7: 文件 watch 端点(notify + SSE)。
//!
//! `GET /api/v1/sessions/{id}/files/watch?path=&timeout_secs=` → `text/event-stream`。
//! notify(inotify)递归监听 sanitize(path) 子树;FS 事件转 SSE(created/modified/removed)。
//! 连接关闭 / 超时 → drop watcher 自动清理(inotify fd + notify 线程)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::api::{core_err_response, AppState, ErrorResponse};

/// 活跃 watcher 计数(可观测/测试用)。创建 +1,Drop -1;用于断言连接断开后无泄漏。
pub static ACTIVE_WATCHERS: AtomicU64 = AtomicU64::new(0);

/// cr-085 M7: query 参数。
#[derive(Debug, Deserialize)]
pub struct WatchQuery {
    /// 监听子目录(workspace 相对,空=workspace 根)。越界 sanitize 拒。
    #[serde(default)]
    pub path: Option<String>,
    /// 流最大存活秒(默认 60)。到时流结束,watcher drop。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// cr-085 M7: RAII guard——Drop 时递减 ACTIVE_WATCHERS。移入 stream! 块,
/// 使流正常结束 OR 被取消(客户端断开)时都能正确清理。
struct WatchGuard;
impl Drop for WatchGuard {
    fn drop(&mut self) {
        ACTIVE_WATCHERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// cr-085 M7: watch handler。解析路径(sanitize 圈定)→ 起 notify watcher → Sse 流。
/// watcher + guard 由 stream! 持有:流 Drop(客户端断开 / 超时)即停 notify + 计数递减。
pub async fn watch_files(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<WatchQuery>,
) -> Response {
    let (watch_root, base) = match state
        .sessions
        .watch_paths(&id, q.path.as_deref().unwrap_or(""))
    {
        Ok(v) => v,
        Err(e) => return core_err_response(&e),
    };

    let (tx, mut rx) = mpsc::channel::<notify::Event>(64);
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res: Result<
        notify::Event,
        notify::Error,
    >| {
        if let Ok(ev) = res {
            // notify 在自有线程回调:blocking_send 合适;channel 满则背压(阻塞 notify 派发)。
            let _ = tx.blocking_send(ev);
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("watch init failed: {e}"),
                }),
            )
                .into_response();
        }
    };
    if let Err(e) = watcher.watch(&watch_root, RecursiveMode::Recursive) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("watch failed: {e}"),
            }),
        )
            .into_response();
    }

    ACTIVE_WATCHERS.fetch_add(1, Ordering::SeqCst);
    let guard = WatchGuard;
    let timeout = Duration::from_secs(q.timeout_secs.unwrap_or(60));

    let s = stream! {
        let _watcher = watcher; // owned → 流 Drop 即停 notify
        let _guard = guard;     // owned → 流 Drop 递减计数
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,           // 超时 → 结束
                ev = rx.recv() => match ev {
                    Some(ev) => {
                        if let Some(sse) = event_to_sse(&ev, &base) {
                            yield Ok::<_, std::convert::Infallible>(sse);
                        }
                    }
                    None => break,                    // watcher 关闭 → 结束
                }
            }
        }
    };

    Sse::new(s)
        // keep_alive:周期发 SSE comment。双重作用——
        // (1) 中间件/代理保活;(2) 客户端断开后,下条 comment 写失败 → axum 检测到 → Drop 流(含 watcher)。
        .keep_alive(KeepAlive::new().interval(Duration::from_millis(500)))
        .into_response()
}

/// cr-085 M7: notify Event → SSE Event。Create/Modify/Remove → created/modified/removed;
/// Access/Any/Other 跳过(降噪)。路径转 workspace 相对(strip_prefix base)。
fn event_to_sse(ev: &notify::Event, base: &std::path::Path) -> Option<Event> {
    let kind = match ev.kind {
        EventKind::Create(_) => "created",
        EventKind::Modify(_) => "modified",
        EventKind::Remove(_) => "removed",
        _ => return None,
    };
    let paths: Vec<String> = ev
        .paths
        .iter()
        .map(|p| {
            p.strip_prefix(base)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.to_string_lossy().into_owned())
        })
        .collect();
    if paths.is_empty() {
        return None;
    }
    Some(
        Event::default()
            .event(kind)
            .json_data(serde_json::json!({ "paths": paths }))
            .unwrap_or_default(),
    )
}
