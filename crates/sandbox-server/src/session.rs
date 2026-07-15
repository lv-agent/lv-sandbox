//! cr-026 会话管理器:持久工作区 + 绑定 profile,跨 exec 存活。
//!
//! 会话 = 一次性 job 的泛化:工作区生命周期与 exec 解耦(create/destroy 管,
//! exec 复用)。exec 串行(每会话互斥)。文件 I/O 经 workspace 模块(路径穿越防护)。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

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
    /// cr-085 M2: 绝对创建时间(跨重启 rebuild 恢复)。
    started_at: SystemTime,
    /// cr-085 M2: E2B alias(M3 起 create 填)。
    alias: Option<String>,
    /// cr-085 M2: 总生命周期超时秒(M3 起 create 填)。
    timeout_secs: Option<u64>,
    /// cr-085 M2: 自由 KV 元数据(M3 起 create 填)。
    metadata: HashMap<String, String>,
}

/// 会话对外视图(列表/查询用,可序列化)。
#[derive(Debug, Serialize, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub profile: String,
    pub created_at_secs: u64,
    pub last_activity_secs: u64,
    pub execs: u64,
    /// cr-085 M2: 派生字段(对齐 E2B SandboxInfo)。
    pub template_id: Option<String>,
    pub state: String,
    pub started_at: Option<u64>,
    pub cpu_count: u32,
    pub memory_size: u64,
    pub alias: Option<String>,
    pub timeout_secs: Option<u64>,
    pub metadata: HashMap<String, String>,
}

/// cr-033: 会话执行上下文(workspace + 绑定 profile + exec_lock + runner),tty handler 用。
pub type SessionExecContext = (
    JobWorkspace,
    SandboxProfile,
    Arc<tokio::sync::Mutex<()>>,
    Arc<SandboxRunner>,
);

/// cr-028: 卷挂载声明(`workspace/<mount>` symlink → 卷目录)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VolumeMount {
    pub name: String,
    pub mount: String,
}

// ==================== cr-085 M6: find / search ====================

/// cr-085 M6: find 结果(glob 匹配的文件清单 + 截断标记)。
#[derive(Debug, Clone, Serialize)]
pub struct FindResult {
    pub files: Vec<FoundFile>,
    pub truncated: bool,
}

/// cr-085 M6: 单条 find 命中——workspace 相对路径 + 富 FileEntry。
#[derive(Debug, Clone, Serialize)]
pub struct FoundFile {
    /// workspace 相对路径(与 get/list 等文件 API 路径风格一致)。
    pub path: String,
    pub entry: sandbox_core::workspace::FileEntry,
}

/// cr-085 M6: search 结果(内容命中的文件清单 + 截断标记)。
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub results: Vec<SearchFileResult>,
    pub truncated: bool,
}

/// cr-085 M6: 单个文件的 search 命中集。
#[derive(Debug, Clone, Serialize)]
pub struct SearchFileResult {
    pub path: String,
    pub matches: Vec<SearchHit>,
}

/// cr-085 M6: 单行命中——1-based 行号 + 命中行原文。
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// 1-based 行号。
    pub line: u64,
    /// 命中行原文(按 max_line_len 截断 + '…')。
    pub text: String,
}

/// cr-085 M6: search 三重上限(防大目录/大文件爆炸)。
#[derive(Debug, Clone)]
pub struct SearchOpts {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_matches: usize,
    pub max_line_len: usize,
}
impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            max_files: 1000,
            max_file_bytes: 2 * 1024 * 1024, // 2 MiB
            max_matches: 1000,
            max_line_len: 512,
        }
    }
}

/// cr-085 M6: 绝对路径 → workspace 相对路径(strip_prefix 失败则回退全路径字符串)。
fn rel_to_base(base: &std::path::Path, p: &std::path::Path) -> String {
    p.strip_prefix(base)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

/// cr-085 M6: 二进制检测——前 1KiB 含 NUL 字节视为二进制(经典启发式)。
fn is_binary(content: &[u8]) -> bool {
    let n = content.len().min(1024);
    content[..n].contains(&0u8)
}

/// cr-085 M6: 按字符数截断行(char 边界安全,超限追加 '…')。
fn truncate_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut s: String = line.chars().take(max).collect();
    s.push('…');
    s
}

/// cr-029: 会话持久元数据(写 `sessions/{id}/.session-meta.json`,跨重启重建用)。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SessionMeta {
    profile_name: String,
    env: HashMap<String, String>,
    #[serde(default)]
    volumes: Vec<VolumeMount>,
    /// cr-085 M2: E2B alias(Sandbox.connect 别名)。M3 起 create 写入。
    #[serde(default)]
    alias: Option<String>,
    /// cr-085 M2: 会话总生命周期超时(秒)。
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// cr-085 M2: 自由 KV 元数据(E2B metadata)。
    #[serde(default)]
    metadata: HashMap<String, String>,
    /// cr-085 M2: 绝对创建时间(unix 秒,跨重启 rebuild 恢复)。
    #[serde(default)]
    started_at_secs: Option<u64>,
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
    pub fn exec_context(&self, id: &str) -> Result<SessionExecContext, CoreError> {
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
        self.create_session_with_opts(
            profile_name,
            env,
            from_snapshot,
            volumes,
            None,
            HashMap::new(),
            None,
        )
    }

    /// cr-085 M3: 带 alias/metadata/timeout 的创建(对齐 E2B Sandbox.create)。
    /// 原 create_session 委托此方法(默认 alias/metadata/timeout 为空)。
    pub fn create_session_with_opts(
        &self,
        profile_name: &str,
        env: HashMap<String, String>,
        from_snapshot: Option<String>,
        volumes: Vec<VolumeMount>,
        alias: Option<String>,
        metadata: HashMap<String, String>,
        timeout_secs: Option<u64>,
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
            // cr-085 M3: 接 create 参数填真值
            alias: alias.clone(),
            timeout_secs,
            metadata: metadata.clone(),
            started_at_secs: Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
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
                started_at: SystemTime::now(),
                alias,
                timeout_secs,
                metadata,
            },
        );
        Ok(id)
    }

    fn info_of(e: &SessionEntry) -> SessionInfo {
        let cg = e.profile.cgroup_resources.as_ref();
        let memory_size = cg.and_then(|c| c.memory_max).unwrap_or(0);
        let cpu_count = match cg.and_then(|c| c.cpu_max_quota) {
            Some(quota) => {
                let period = cg
                    .and_then(|c| c.cpu_max_period)
                    .filter(|p| *p > 0)
                    .unwrap_or(1_000_000);
                ((quota as f64 / period as f64).ceil() as u64).max(1) as u32
            }
            None => std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
        };
        let started_at = e
            .started_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        SessionInfo {
            session_id: e.id.clone(),
            profile: e.profile.name.clone(),
            created_at_secs: e.created_at.elapsed().as_secs(),
            last_activity_secs: e.last_activity.elapsed().as_secs(),
            execs: e.execs,
            template_id: Some(e.profile.name.clone()),
            state: "RUNNING".to_string(),
            started_at,
            cpu_count,
            memory_size,
            alias: e.alias.clone(),
            timeout_secs: e.timeout_secs,
            metadata: e.metadata.clone(),
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

    /// cr-085 M4: 更新 alias/timeout/metadata(替换语义),同步重写 .session-meta.json。
    /// 对齐 E2B set_timeout/set_metadata(全量替换);增量 add_metadata 由 shim get+merge+set 实现。
    pub fn update_session(
        &self,
        id: &str,
        alias: Option<String>,
        timeout_secs: Option<u64>,
        metadata: HashMap<String, String>,
    ) -> Result<(), CoreError> {
        let mut guard = self.sessions.write().expect("sessions lock poisoned");
        let e = guard
            .get_mut(id)
            .ok_or_else(|| CoreError::Workspace(format!("session not found: {id}")))?;
        e.alias = alias.clone();
        e.timeout_secs = timeout_secs;
        e.metadata = metadata.clone();
        e.last_activity = Instant::now();
        let meta_path = e.workspace.root.join(".session-meta.json");
        let mut meta: SessionMeta = serde_json::from_str(
            &std::fs::read_to_string(&meta_path)
                .map_err(|_| CoreError::Workspace("session-meta.json missing".into()))?,
        )
        .map_err(|_| CoreError::Workspace("session-meta.json corrupt".into()))?;
        meta.alias = alias;
        meta.timeout_secs = timeout_secs;
        meta.metadata = metadata;
        std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap_or_default())
            .map_err(|_| CoreError::Workspace("session-meta.json write failed".into()))?;
        Ok(())
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

        // cr-041: session exec metrics(与 scheduler 对齐)
        crate::metrics::JOB_STARTED_TOTAL.inc();
        crate::metrics::RUNNING_JOBS.inc();
        let timer = crate::metrics::FORK_EXEC_DURATION.start_timer();

        let result = self
            .runner
            .run_in_workspace(&workspace, &profile, request, cancel, sink)
            .await;

        timer.observe_duration();
        crate::metrics::JOB_FINISHED_TOTAL.inc();
        crate::metrics::RUNNING_JOBS.dec();

        // 终态审计 + webhook + 更新计数
        let result = match result {
            Ok(r) => {
                if r.timed_out {
                    crate::metrics::JOB_TIMEOUT_TOTAL.inc();
                }
                for v in &r.sandbox_violations {
                    match v {
                        sandbox_core::job::SandboxViolation::SeccompDenied { .. } => {
                            crate::metrics::JOB_SECCOMP_DENIED_TOTAL.inc();
                        }
                        sandbox_core::job::SandboxViolation::OomKill => {
                            crate::metrics::JOB_OOM_KILLED_TOTAL.inc();
                        }
                        _ => {}
                    }
                }
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

    /// cr-085 M5: 建目录(E2B filesystem.make_dir)。
    pub fn make_dir(&self, id: &str, rel: &str) -> Result<(), CoreError> {
        let base = self.workspace_dir(id)?;
        sandbox_core::workspace::make_dir(&base, rel)
    }

    /// cr-085 M5: 路径存在(E2B filesystem.exists;HEAD 端点用)。
    pub fn file_exists(&self, id: &str, rel: &str) -> bool {
        let Ok(base) = self.workspace_dir(id) else {
            return false;
        };
        sandbox_core::workspace::exists(&base, rel)
    }

    /// cr-085 M7: 解析 watch 目标路径(sanitize 圈定,拒越界)+ 返回 workspace base
    /// (供 SSE 事件转相对路径)。watch_files handler 用。
    pub fn watch_paths(
        &self,
        id: &str,
        rel: &str,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf), CoreError> {
        let base = self.workspace_dir(id)?;
        let watch_root = if rel.is_empty() {
            base.clone()
        } else {
            sandbox_core::workspace::sanitize_relpath(&base, rel)?
        };
        if !watch_root.exists() {
            return Err(CoreError::Workspace(format!("path not found: {rel}")));
        }
        Ok((watch_root, base))
    }

    // ==================== cr-085 M6: find / search ====================

    /// cr-085 M6: glob 匹配文件(`**/*.py`)。walkdir 递归遍历 path 子树,globset 匹配
    /// **相对 path 的**路径(E2B 语义),返回命中文件(workspace 相对路径 + 富 FileEntry)。
    /// limit 截断防爆炸;越界 path 由 sanitize_relpath 拒绝。
    pub fn find_session_files(
        &self,
        id: &str,
        rel: &str,
        pattern: &str,
        limit: usize,
    ) -> Result<FindResult, CoreError> {
        let base = self.workspace_dir(id)?;
        let search_root = if rel.is_empty() {
            base.clone()
        } else {
            sandbox_core::workspace::sanitize_relpath(&base, rel)?
        };
        if !search_root.exists() {
            return Err(CoreError::Workspace(format!("path not found: {rel}")));
        }
        let glob = globset::Glob::new(pattern)
            .map_err(|e| CoreError::Workspace(format!("invalid glob pattern: {e}")))?;
        let matcher = glob.compile_matcher();
        let mut files = Vec::new();
        let mut truncated = false;
        for entry in walkdir::WalkDir::new(&search_root).into_iter() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue, // 无权限/损坏条目 → 跳过,不整体失败
            };
            // 相对 search_root 用于 glob 匹配(pattern 相对 path)
            let rel_to_root = match entry.path().strip_prefix(&search_root) {
                Ok(r) if !r.as_os_str().is_empty() => r,
                _ => continue, // 跳过 search_root 自身
            };
            if !matcher.is_match(rel_to_root) {
                continue;
            }
            if files.len() >= limit {
                truncated = true;
                break;
            }
            let entry_meta = match sandbox_core::workspace::file_entry(entry.path()) {
                Ok(fe) => fe,
                Err(_) => continue,
            };
            // 相对 workspace base 上报(与 get/list 等文件 API 路径一致)
            let path = rel_to_base(&base, entry.path());
            files.push(FoundFile {
                path,
                entry: entry_meta,
            });
        }
        Ok(FindResult { files, truncated })
    }

    /// cr-085 M6: 正则匹配文件**内容**。walkdir 遍历普通文件(跳过目录/symlink),
    /// 跳过二进制(前 1KiB 含 NUL 字节)与超 max_file_bytes 的文件,逐行 regex 匹配。
    /// 三重上限(max_files / max_file_bytes / max_matches)防爆炸;越界 path 拒绝。
    pub fn search_session_files(
        &self,
        id: &str,
        rel: &str,
        pattern: &str,
        opts: &SearchOpts,
    ) -> Result<SearchResult, CoreError> {
        let base = self.workspace_dir(id)?;
        let search_root = if rel.is_empty() {
            base.clone()
        } else {
            sandbox_core::workspace::sanitize_relpath(&base, rel)?
        };
        if !search_root.exists() {
            return Err(CoreError::Workspace(format!("path not found: {rel}")));
        }
        let re = regex::Regex::new(pattern)
            .map_err(|e| CoreError::Workspace(format!("invalid regex pattern: {e}")))?;
        let mut results = Vec::new();
        let mut truncated = false;
        let mut scanned = 0usize;
        let mut total_matches = 0usize;
        for entry in walkdir::WalkDir::new(&search_root).into_iter() {
            if truncated {
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            // 只搜普通文件内容(目录/symlink 跳过;symlink 不跟随)
            if !entry.file_type().is_file() {
                continue;
            }
            if scanned >= opts.max_files {
                truncated = true;
                break;
            }
            let md = match std::fs::metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.len() > opts.max_file_bytes {
                continue;
            }
            let content = match std::fs::read(entry.path()) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if is_binary(&content) {
                continue;
            }
            scanned += 1;
            let text = String::from_utf8_lossy(&content);
            let mut hits = Vec::new();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    hits.push(SearchHit {
                        line: (i + 1) as u64,
                        text: truncate_line(line, opts.max_line_len),
                    });
                    total_matches += 1;
                    if total_matches >= opts.max_matches {
                        truncated = true;
                        break;
                    }
                }
            }
            if !hits.is_empty() {
                let path = rel_to_base(&base, entry.path());
                results.push(SearchFileResult { path, matches: hits });
            }
        }
        Ok(SearchResult { results, truncated })
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
                        started_at: meta
                            .started_at_secs
                            .and_then(|s| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(s)))
                            .unwrap_or_else(SystemTime::now),
                        alias: meta.alias,
                        timeout_secs: meta.timeout_secs,
                        metadata: meta.metadata,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// cr-085 M2: 旧格式 .session-meta.json(无 alias/timeout/metadata)必须仍能反序列化,
    /// 新字段 default 填充。这是跨重启重连(rebuild_from_disk)的最高风险点。
    #[test]
    fn session_meta_old_format_loads_with_defaults() {
        let old = r#"{"profile_name":"shell","env":{"A":"1"},"volumes":[]}"#;
        let meta: SessionMeta = serde_json::from_str(old).expect("old meta must load");
        assert_eq!(meta.profile_name, "shell");
        assert_eq!(meta.env.get("A").map(|s| s.as_str()), Some("1"));
        // 新字段 default(旧 json 缺这些 → 不应反序列化失败)
        assert_eq!(meta.alias, None);
        assert_eq!(meta.timeout_secs, None);
        assert!(meta.metadata.is_empty());
    }
}
