//! cr-033: 交互 PTY / terminal——WebSocket upgrade + PTY 双向转发。
//!
//! `GET /api/v1/sessions/{id}/tty?argv=/bin/sh&argv=-c&argv=...&timeout_secs=30`
//! → WebSocket。客户端发 Binary/Text → 写入 PTY master(子进程 stdin);
//! 子进程 stdout(PTY master 读)→ Binary 发回客户端。进程退出 → Text JSON `{type:"exit",...}`。

use std::os::unix::io::FromRawFd;
use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;

use sandbox_core::env::build_sanitized_env;
use sandbox_core::process::PreparedSandboxContext;

use crate::api::AppState;
use crate::session::SessionManager;

#[derive(serde::Deserialize)]
pub struct TtyQuery {
    /// argv(空格分隔的 query 参数:?argv=/bin/sh+-c+echo+hi)。v1:单个参数不含空格。
    pub argv: String,
    /// 超时(秒),默认 300
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// WebSocket upgrade handler。
pub async fn session_tty(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<TtyQuery>,
) -> Response {
    ws.on_upgrade(move |socket| run_tty(socket, state.sessions.clone(), id, q))
}

async fn run_tty(mut socket: WebSocket, sm: Arc<SessionManager>, sid: String, q: TtyQuery) {
    let argv: Vec<String> = q.argv.split_whitespace().map(|s| s.to_string()).collect();
    if argv.is_empty() {
        let _ = socket.send(Message::text(r#"{"type":"error","message":"no argv"}"#)).await;
        return;
    }

    // 1. 取会话上下文(workspace + profile + exec_lock + runner)
    let (workspace, profile, exec_lock, runner) = match sm.exec_context(&sid) {
        Ok(v) => v,
        Err(e) => {
            let _ = socket.send(Message::text(format!(
                r#"{{"type":"error","message":"session not found: {e}"}}"#
            ))).await;
            return;
        }
    };
    let _lock = exec_lock.lock().await;

    // 2. openpty
    let (master_fd, slave_fd) = {
        let mut m: libc::c_int = -1;
        let mut s: libc::c_int = -1;
        let rc = unsafe {
            libc::openpty(
                &mut m, &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if rc != 0 {
            let _ = socket.send(Message::text(r#"{"type":"error","message":"openpty failed"}"#)).await;
            return;
        }
        (m, s)
    };

    // 3. env
    let env = build_sanitized_env(
        &sid,
        &workspace.root,
        &profile.env,
        &std::collections::HashMap::new(),
    );

    // 4. 安全上下文(landlock/seccomp/cgroup/rlimit)+ tty_slave_fd
    let mut prepared = match PreparedSandboxContext::prepare(
        &profile,
        &workspace.workspace,
        &sid,
        runner.capability(),
        runner.workspace_mgr(),
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::text(format!(
                    r#"{{"type":"error","message":"prepare: {e}"}}"#
                )))
                .await;
            unsafe { libc::close(master_fd); libc::close(slave_fd); }
            return;
        }
    };
    prepared.tty_slave_fd = Some(slave_fd);
    let cgroup = prepared.take_cgroup();

    // 5. 构建 Command(slave 作 stdin/stdout/stderr + pre_exec 全安全管道)
    let workspace_path = workspace.workspace.clone();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    let slave_in = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let slave_out = slave_in.try_clone().unwrap_or_else(|_| slave_in.try_clone().unwrap());
    let slave_err = slave_in.try_clone().unwrap_or_else(|_| slave_in.try_clone().unwrap());
    cmd.env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::from(slave_in))
        .stdout(std::process::Stdio::from(slave_out))
        .stderr(std::process::Stdio::from(slave_err));
    unsafe {
        cmd.pre_exec(move || {
            // error_pipe_fd = -1(无 pipe;出错子进程直接 _exit)
            prepared.apply_in_child(&workspace_path, -1);
            Ok(())
        });
    }

    // 6. 启动
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::text(format!(
                    r#"{{"type":"error","message":"spawn: {e}"}}"#
                )))
                .await;
            unsafe { libc::close(master_fd); }
            if let Some(cg) = cgroup { let _ = cg.destroy(); }
            return;
        }
    };
    let child_pid = child.id().unwrap_or(0) as i32;

    // 7. master→WS(blocking read → channel)
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let mfd = master_fd;
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(mfd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 { break; }
            if tx.blocking_send(buf[..n as usize].to_vec()).is_err() { break; }
        }
    });

    let timeout = Duration::from_secs(q.timeout_secs.unwrap_or(300));

    // 8. 主循环:WS ↔ master + child exit + timeout
    loop {
        tokio::select! {
            // master → WS(子进程输出)
            Some(data) = rx.recv() => {
                if socket.send(Message::binary(data)).await.is_err() { break; }
            }
            // WS → master(客户端输入)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = unsafe { libc::write(master_fd, data.as_ptr().cast(), data.len()) };
                    }
                    Some(Ok(Message::Text(s))) => {
                        let b = s.as_bytes().to_vec();
                        let _ = unsafe { libc::write(master_fd, b.as_ptr().cast(), b.len()) };
                    }
                    _ => break, // WS 关闭/错误
                }
            }
            // 子进程退出——先排空 channel(竞态:快速进程的输出可能在 exit 后才到)
            r = child.wait() => {
                // drain 即时数据
                while let Ok(data) = rx.try_recv() {
                    let _ = socket.send(Message::binary(data)).await;
                }
                // 给 PTY 读线程一点时间读完残留 + 再 drain
                tokio::time::sleep(Duration::from_millis(50)).await;
                while let Ok(data) = rx.try_recv() {
                    let _ = socket.send(Message::binary(data)).await;
                }
                let code = r.as_ref().ok().and_then(|s| s.code());
                let signal = r.as_ref().ok().and_then(|s| s.signal());
                let body = format!(
                    r#"{{"type":"exit","exit_code":{:?},"signal":{:?}}}"#,
                    code, signal
                );
                let _ = socket.send(Message::text(body)).await;
                break;
            }
            // 超时
            _ = tokio::time::sleep(timeout) => {
                unsafe { libc::killpg(child_pid, libc::SIGTERM); }
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = child.kill().await;
                let _ = socket.send(Message::text(r#"{"type":"timeout"}"#)).await;
                break;
            }
        }
    }

    // 9. 清理
    if let Some(cg) = cgroup { let _ = cg.destroy(); }
    unsafe { libc::close(master_fd); }
}
