//! cr-026 会话管理器:持久工作区 + 绑定 profile,跨 exec 存活。
//!
//! 会话 = 一次性 job 的泛化:工作区生命周期与 exec 解耦(create/destroy 管,
//! exec 复用)。exec 串行(每会话互斥)。文件 I/O 经 workspace 模块(路径穿越防护)。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

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

/// cr-028: 卷挂载声明(`workspace/<mount>` symlink → 卷目录)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VolumeMount {
    pub name: String,
    pub mount: String,
}

/// cr-029: 会话持久元数据(写 `sessions/{id}/.session-meta.json`,跨重启重建用)。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SessionMeta {
    profile_name: String,
    env: HashMap<String, String>,
    #[serde(default)]
    volumes: Vec<VolumeMount>,
}

/// 会话管理器。
pub struct SessionManager {
    runner: Arc<SandboxRunner>,
    sessions: Arc<RwLock<HashMap<String, SessionEntry>>>,
    audit: Arc<AuditLogger>,
    /// cr-031: 生命周期 webhook(默认 noop)
    webhooks: Arc<crate::webhook::WebhookDispatcher>,
}

impl SessionManager {
    pub fn new(runner: Arc<SandboxRunner>, audit: Arc<AuditLogger>) -> Self {
        Self {
            runner,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            audit,
            webhooks: Arc::new(crate::webhook::WebhookDispatcher::noop()),
        }
    }

    /// cr-031: 注入 webhook 分发器(builder,main 用)。
    pub fn with_webhooks(mut self, w: Arc<crate::webhook::WebhookDispatcher>) -> Self {
        self.webhooks = w;
        self
    }

    /// cr-040: 扫描并清理过期的会话。返回被清理的 session id 列表。
    /// `ttl_secs` = 无活动超时秒数。
    pub fn reap_expired(&self, ttl_secs: u64) -> Vec<String> {
        let now = Instant::now();
        let ttl = Duration::from_secs(ttl_secs);
        let mut reaped = Vec::new();

        // 收集过期 id(持读锁)
        let expired: Vec<String> = {
            let guard = self.sessions.read().expect("sessions lock poisoned");
            guard
                .iter()
                .filter(|(_, e)| now.duration_since(e.last_activity) > ttl)
                .map(|(id, _)| id.clone())
                .collect()
        };

        // 逐个销毁(destroy_session 持写锁)
        for id in &expired {
            match self.destroy_session(id) {
                Ok(()) => {
                    tracing::info!(session_id = %id, "reaped expired session (TTL {}s)", ttl_secs);
                    reaped.push(id.clone());
                }
                Err(e) => {
                    tracing::warn!(session_id = %id, error = %e, "failed to reap expired session");
                }
            }
        }
        reaped
    }

    /// cr-040: 启动后台 TTL reaper(定时扫描 + 清理)。返回 JoinHandle(可 cancel)。
    pub fn spawn_reaper(self: &Arc<Self>, ttl_secs: u64, interval_secs: u64) -> tokio::task::JoinHandle<()> {
        let sm = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                ticker.tick().await;
                let reaped = sm.reap_expired(ttl_secs);
                if !reaped.is_empty() {
                    tracing::info!(count = reaped.len(), "session TTL reaper cleaned up");
                }
            }
        })
    }

    /// cr-033: 暴露会话运行上下文(tty handler 用):workspace + profile + exec_lock + runner。
    pub fn exec_context(
        &self,
        id: &str,
    ) -> Result<
        (
            JobWorkspace,
            SandboxProfile,
            Arc<tokio::sync::Mutex<()>>,
            Arc<SandboxRunner>,
        ),
        CoreError,
    > {
        let guard = self.sessions.read().expect("sessions lock poisoned");
        let e = guard
            .get(id)
            .ok_or_else(|| CoreError::Workspace(format!("session not found: {id}")))?;
        Ok((
            e.workspace.clone(),
            e.profile.clone(),
            e.exec_lock.clone(),
            self.runner.clone(),
        ))
    }

    /// 建会话:查 profile → 建持久工作区(可从快照恢复 / 挂卷)→ 入表。
    /// cr-027: `from_snapshot` 从快照 fork;cr-028: `volumes` 挂持久卷(symlink + landlock ReadWrite)。
    pub fn create_session(
        &self,
        profile_name: &str,
        env: HashMap<String, String>,
        from_snapshot: Option<String>,
        volumes: Vec<VolumeMount>,
    ) -> Result<String, CoreError> {
        let mut profile = self
            .runner
            .profile_registry()
            .get(profile_name)
            .ok_or_else(|| CoreError::ProfileNotFound(profile_name.to_string()))?
            .clone();
        // 会话级 env 合并进绑定 profile(template baseline + 会话补充)
        let meta_env = env.clone(); // cr-029: 持久化用(重建时再合并)
        for (k, v) in env {
            profile.env.insert(k, v);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let workspace = self.runner.workspace_mgr().create_session_workspace(&id)?;

        // cr-029: 持久化会话元数据(跨重启重连,含 volumes)
        let meta = SessionMeta {
            profile_name: profile_name.to_string(),
            env: meta_env,
            volumes: volumes.clone(),
        };
        let _ = std::fs::write(
            workspace.root.join(".session-meta.json"),
            serde_json::to_vec(&meta).unwrap_or_default(),
        );

        // cr-027: 从快照恢复(fork)
        if let Some(snap_id) = &from_snapshot {
            self.runner
                .workspace_mgr()
                .restore_snapshot(snap_id, &workspace.workspace)?;
        }

        // cr-028: 挂卷(workspace/<mount> symlink → 卷目录;卷路径入 extra_writable_paths 授 ReadWrite)
        Self::mount_volumes(
            self.runner.workspace_mgr(),
            &workspace,
            &mut profile,
            &volumes,
        )?;

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

        // 终态审计 + webhook + 更新计数
        let result = match result {
            Ok(r) => {
                let ev = crate::audit::AuditEvent::new(
                    crate::audit::status_to_event_type(&r.status),
                    id,
                    &profile.name,
                    argv,
                    r.exit_code,
                    r.signal,
                    Some(r.duration.as_millis() as u64),
                    crate::audit::status_detail(&r.status),
                );
                self.webhooks.dispatch(&ev);
                self.audit.log(ev);
                r
            }
            Err(e) => {
                let ev = crate::audit::AuditEvent::new(
                    crate::audit::AuditEventType::JobFailed,
                    id,
                    &profile.name,
                    argv,
                    None,
                    None,
                    None,
                    Some(format!("session exec error: {e}")),
                );
                self.webhooks.dispatch(&ev);
                self.audit.log(ev);
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

    /// cr-028: 挂卷(workspace/<mount> symlink → 卷目录;卷路径入 extra_writable_paths 授 landlock ReadWrite)。
    /// cr-029: 重建时复用(恢复 landlock 授权)。
    fn mount_volumes(
        mgr: &sandbox_core::workspace::WorkspaceManager,
        workspace: &JobWorkspace,
        profile: &mut SandboxProfile,
        volumes: &[VolumeMount],
    ) -> Result<(), CoreError> {
        for vm in volumes {
            let vol_path = mgr.volume_path(&vm.name);
            std::fs::create_dir_all(&vol_path)?;
            let link = workspace.workspace.join(&vm.mount);
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(&link); // 幂等
            std::os::unix::fs::symlink(&vol_path, &link)?;
            profile.extra_writable_paths.push(vol_path);
        }
        Ok(())
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

    // ==================== cr-027: 快照(磁盘-only,跨重启存活) ====================

    /// 快照会话:持 exec_lock(等运行中 exec 完成,静默)→ 拷 workspace → 返回 snapshot_id。
    pub async fn snapshot_session(&self, id: &str) -> Result<String, CoreError> {
        let (ws_path, exec_lock) = {
            let guard = self.sessions.read().expect("sessions lock poisoned");
            let e = guard
                .get(id)
                .ok_or_else(|| CoreError::Workspace(format!("session not found: {id}")))?;
            (e.workspace.workspace.clone(), e.exec_lock.clone())
        };
        // cr-027: 持 exec_lock 确保静默(不与运行中 exec 竞态)
        let _guard = exec_lock.lock().await;
        let snap_id = uuid::Uuid::new_v4().to_string();
        self.runner
            .workspace_mgr()
            .create_snapshot(&ws_path, &snap_id)?;
        Ok(snap_id)
    }

    /// 列所有快照 id(扫盘)。
    pub fn list_snapshots(&self) -> Result<Vec<String>, CoreError> {
        self.runner.workspace_mgr().list_snapshots()
    }

    /// 销毁快照。
    pub fn destroy_snapshot(&self, id: &str) -> Result<(), CoreError> {
        self.runner.workspace_mgr().cleanup_snapshot(id)
    }

    // ==================== cr-028: 卷(跨会话持久 rw) ====================

    pub fn create_volume(&self, name: &str) -> Result<(), CoreError> {
        self.runner.workspace_mgr().create_volume(name)
    }
    pub fn list_volumes(&self) -> Result<Vec<String>, CoreError> {
        self.runner.workspace_mgr().list_volumes()
    }
    pub fn cleanup_volume(&self, name: &str) -> Result<(), CoreError> {
        self.runner.workspace_mgr().cleanup_volume(name)
    }

    // ==================== cr-029: 跨重启重连(从盘重建注册表) ====================

    /// 启动恢复:扫 `sessions/`,读 `.session-meta.json`,重建 SessionEntry。
    /// profile 缺失则跳过(记日志)。返回重建数。
    pub fn rebuild_from_disk(&self) -> Result<usize, CoreError> {
        let mgr = self.runner.workspace_mgr();
        let ids = mgr.list_sessions()?;
        let mut count = 0;
        for id in ids {
            let meta_path = mgr
                .base_dir()
                .join("sessions")
                .join(&id)
                .join(".session-meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue; // 无 meta(遗留/未知)→ 跳过
            };
            let Ok(meta) = serde_json::from_str::<SessionMeta>(&content) else {
                continue;
            };
            let Some(mut profile) = self.runner.profile_registry().get(&meta.profile_name).cloned() else {
                tracing::warn!(
                    session_id = %id,
                    profile = %meta.profile_name,
                    "rebuild skip: profile not found"
                );
                continue;
            };
            for (k, v) in &meta.env {
                profile.env.insert(k.clone(), v.clone());
            }
            // 复用既有工作区(create_session_workspace 幂等 mkdir)
            let workspace = mgr.create_session_workspace(&id)?;
            // cr-029: 重新挂卷(恢复 landlock ReadWrite 授权——否则重启后卷不可写)
            Self::mount_volumes(mgr, &workspace, &mut profile, &meta.volumes)?;
            self.sessions
                .write()
                .expect("sessions lock poisoned")
                .insert(
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
            count += 1;
        }
        if count > 0 {
            tracing::info!(rebuilt = count, "sessions rebuilt from disk");
        }
        Ok(count)
    }
}
