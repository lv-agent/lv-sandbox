//! cr-026 会话管理器:持久工作区 + 绑定 profile,跨 exec 存活。
//!
//! 会话 = 一次性 job 的泛化:工作区生命周期与 exec 解耦(create/destroy 管,
//! exec 复用)。exec 串行(每会话互斥)。文件 I/O 经 workspace 模块(路径穿越防护)。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use sandbox_core::error::CoreError;
use sandbox_core::job::{JobRequest, JobResult, StreamEvent};
use sandbox_core::profile::SandboxProfile;
use sandbox_core::sandbox_context::SandboxRunner;
use sandbox_core::workspace::JobWorkspace;

use crate::audit::AuditLogger;

/// 会话表项(内部)。
struct SessionEntry {
    id: String,
    workspace: JobWorkspace,
    profile: SandboxProfile,
    created_at: Instant,
    last_activity: Instant,
    execs: u64,
    exec_lock: Arc<tokio::sync::Mutex<()>>,
}

/// 会话对外视图(列表/查询用,可序列化)。
#[derive(Debug, Serialize, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub profile: String,
    pub created_at_secs: u64,
    pub last_activity_secs: u64,
    pub execs: u64,
}

/// 会话管理器。
pub struct SessionManager {
    runner: Arc<SandboxRunner>,
    sessions: Arc<RwLock<HashMap<String, SessionEntry>>>,
    audit: Arc<AuditLogger>,
}

impl SessionManager {
    pub fn new(runner: Arc<SandboxRunner>, audit: Arc<AuditLogger>) -> Self {
        Self {
            runner,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            audit,
        }
    }

    /// 建会话:查 profile → 建持久工作区 → 入表。`env` 合并进绑定 profile.env。
    pub fn create_session(
        &self,
        profile_name: &str,
        env: HashMap<String, String>,
    ) -> Result<String, CoreError> {
        let mut profile = self
            .runner
            .profile_registry()
            .get(profile_name)
            .ok_or_else(|| CoreError::ProfileNotFound(profile_name.to_string()))?
            .clone();
        // 会话级 env 合并进绑定 profile(template baseline + 会话补充)
        for (k, v) in env {
            profile.env.insert(k, v);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let workspace = self.runner.workspace_mgr().create_session_workspace(&id)?;

        self.sessions.write().expect("sessions lock poisoned").insert(
            id.clone(),
            SessionEntry {
                id: id.clone(),
                workspace,
                profile,
                created_at: Instant::now(),
                last_activity: Instant::now(),
                execs: 0,
                exec_lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        Ok(id)
    }

    fn info_of(e: &SessionEntry) -> SessionInfo {
        SessionInfo {
            session_id: e.id.clone(),
            profile: e.profile.name.clone(),
            created_at_secs: e.created_at.elapsed().as_secs(),
            last_activity_secs: e.last_activity.elapsed().as_secs(),
            execs: e.execs,
        }
    }

    /// 列所有会话。
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .read()
            .expect("sessions lock poisoned")
            .values()
            .map(Self::info_of)
            .collect()
    }

    /// 查询单个会话。
    pub fn get_session(&self, id: &str) -> Option<SessionInfo> {
        self.sessions
            .read()
            .expect("sessions lock poisoned")
            .get(id)
            .map(Self::info_of)
    }

    /// 销毁会话:清工作区 + 出表。
    pub fn destroy_session(&self, id: &str) -> Result<(), CoreError> {
        if self
            .sessions
            .write()
            .expect("sessions lock poisoned")
            .remove(id)
            .is_some()
        {
            self.runner.workspace_mgr().cleanup_session(id)?;
            Ok(())
        } else {
            Err(CoreError::Workspace(format!("session not found: {id}")))
        }
    }

    /// 在会话工作区执行命令(串行:每会话互斥)。用绑定 profile;request.profile_name 忽略。
    pub async fn exec_session(
        &self,
        id: &str,
        request: JobRequest,
        cancel: CancellationToken,
        sink: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<JobResult, CoreError> {
        // 取快照(克隆 profile + workspace + exec_lock),立刻释放读锁,避免长 await 持锁
        let (workspace, profile, exec_lock) = {
            let guard = self.sessions.read().expect("sessions lock poisoned");
            let e = guard.get(id).ok_or_else(|| {
                CoreError::Workspace(format!("session not found: {id}"))
            })?;
            (e.workspace.clone(), e.profile.clone(), e.exec_lock.clone())
        };

        let argv = request.argv.clone();
        self.audit.log(crate::audit::AuditEvent::new(
            crate::audit::AuditEventType::JobStarted,
            id,
            &profile.name,
            argv.clone(),
            None,
            None,
            None,
            Some("session exec".to_string()),
        ));

        // 串行:同一会话 exec 互斥
        let _guard = exec_lock.lock().await;
        let result = self
            .runner
            .run_in_workspace(&workspace, &profile, request, cancel, sink)
            .await;

        // 终态审计 + 更新计数
        let result = match result {
            Ok(r) => {
                self.audit.log(crate::audit::AuditEvent::new(
                    crate::audit::status_to_event_type(&r.status),
                    id,
                    &profile.name,
                    argv,
                    r.exit_code,
                    r.signal,
                    Some(r.duration.as_millis() as u64),
                    crate::audit::status_detail(&r.status),
                ));
                r
            }
            Err(e) => {
                self.audit.log(crate::audit::AuditEvent::new(
                    crate::audit::AuditEventType::JobFailed,
                    id,
                    &profile.name,
                    argv,
                    None,
                    None,
                    None,
                    Some(format!("session exec error: {e}")),
                ));
                return Err(e);
            }
        };

        if let Some(e) = self
            .sessions
            .write()
            .expect("sessions lock poisoned")
            .get_mut(id)
        {
            e.last_activity = Instant::now();
            e.execs += 1;
        }

        Ok(result)
    }

    // ==================== 文件 I/O(委托 workspace 模块,操作 session 工作区的 workspace/ 子目录) ====================

    fn workspace_dir(&self, id: &str) -> Result<std::path::PathBuf, CoreError> {
        let guard = self.sessions.read().expect("sessions lock poisoned");
        guard
            .get(id)
            .map(|e| e.workspace.workspace.clone())
            .ok_or_else(|| CoreError::Workspace(format!("session not found: {id}")))
    }

    pub fn put_file(&self, id: &str, rel: &str, data: &[u8]) -> Result<(), CoreError> {
        let base = self.workspace_dir(id)?;
        sandbox_core::workspace::put_file(&base, rel, data)
    }

    pub fn get_file(&self, id: &str, rel: &str) -> Result<Vec<u8>, CoreError> {
        let base = self.workspace_dir(id)?;
        sandbox_core::workspace::get_file(&base, rel)
    }

    pub fn list_files(&self, id: &str, rel: &str) -> Result<Vec<sandbox_core::workspace::FileEntry>, CoreError> {
        let base = self.workspace_dir(id)?;
        sandbox_core::workspace::list_files(&base, rel)
    }

    pub fn delete_file(&self, id: &str, rel: &str) -> Result<(), CoreError> {
        let base = self.workspace_dir(id)?;
        sandbox_core::workspace::delete_file(&base, rel)
    }
}
