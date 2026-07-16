//! task_id → lv-sandbox session_id 懒映射。首次见 task_id 调 create_session 建并记;
//! 后续同 task_id 复用同一 session(work_dir 连续性由 session FS 保证)。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::BridgeError;
use crate::lv_client::SandboxHttp;

pub struct SessionMap {
    profile: String,
    timeout_secs: u64,
    map: Mutex<HashMap<String, String>>, // task_id -> session_id
}

impl SessionMap {
    pub fn new(profile: String, timeout_secs: u64) -> Self {
        Self { profile, timeout_secs, map: Mutex::new(HashMap::new()) }
    }

    /// 返回 task_id 对应的 session_id;不存在则调 http.create_session 建并记。
    ///
    /// `profile_override`:本 task 专用 profile;`None` 用 SessionMap 默认 profile。
    /// cr-12:fixus 声明 net 能力时,translate 传 `Some("git")` 以选 git egress profile。
    /// 仅在首次为某 task_id 建会话时生效(后续复用既有 session,不再改 profile)。
    pub async fn get_or_create(
        &self,
        http: &Arc<dyn SandboxHttp>,
        task_id: &str,
        profile_override: Option<&str>,
    ) -> Result<String, BridgeError> {
        // 持锁跨 create_session:避免并发首见同 task_id 时双建 session(TOCTOU —— 两个
        // tool_invoked 都 miss 缓存、各建一个 session,后写覆盖前写 → work_dir 连续性断裂 +
        // 孤儿 session)。tokio::Mutex 允许跨 .await 持有;创建每 task 一次(~ms),串行化可接受。
        let mut map = self.map.lock().await;
        if let Some(sid) = map.get(task_id).cloned() {
            return Ok(sid);
        }
        let profile = profile_override.unwrap_or(&self.profile);
        let mut metadata = HashMap::new();
        metadata.insert("fixus_task_id".to_string(), task_id.to_string());
        let sid = http.create_session(profile, metadata, self.timeout_secs).await?;
        map.insert(task_id.to_string(), sid.clone());
        Ok(sid)
    }

    /// session 建失败/失效时清条目,下次重建。v1 不主动调(靠 lv-sandbox TTL 回收);
    /// 保留为 SessionMap 完整 API(get_or_create / invalidate 成对),供后续 exec 失败恢复用。
    #[allow(dead_code)]
    pub async fn invalidate(&self, task_id: &str) {
        self.map.lock().await.remove(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingMock {
        creates: AtomicUsize,
        first_id: String,
        // 每次 create_session 收到的 profile 参数(按调用顺序)。
        profiles: Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl SandboxHttp for CountingMock {
        async fn create_session(&self, p: &str, _m: HashMap<String, String>, _t: u64)
            -> Result<String, BridgeError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.profiles.lock().await.push(p.to_string());
            Ok(self.first_id.clone())
        }
        async fn exec(&self, _: &str, _: Vec<String>, _: Option<String>, _: Option<String>) -> Result<crate::lv_client::JobResult, BridgeError> { unreachable!() }
        async fn get_file(&self, _: &str, _: &str) -> Result<Vec<u8>, BridgeError> { unreachable!() }
        async fn put_file(&self, _: &str, _: &str, _: Vec<u8>) -> Result<(), BridgeError> { unreachable!() }
        async fn head_file(&self, _: &str, _: &str) -> Result<bool, BridgeError> { unreachable!() }
        async fn find(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { unreachable!() }
        async fn search(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { unreachable!() }
        async fn destroy_session(&self, _: &str) -> Result<(), BridgeError> { unreachable!() }
    }

    fn mock() -> Arc<CountingMock> {
        Arc::new(CountingMock {
            creates: AtomicUsize::new(0),
            first_id: "sess-1".into(),
            profiles: Mutex::new(vec![]),
        })
    }

    #[tokio::test]
    async fn first_call_creates_second_reuses() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        let a = sm.get_or_create(&http, "task-A", None).await.unwrap();
        let b = sm.get_or_create(&http, "task-A", None).await.unwrap();
        assert_eq!(a, "sess-1");
        assert_eq!(a, b, "same task reuses same session");
        assert_eq!(m.creates.load(Ordering::SeqCst), 1, "create_session called exactly once");
    }

    #[tokio::test]
    async fn distinct_tasks_distinct_lookups() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        sm.get_or_create(&http, "task-A", None).await.unwrap();
        sm.get_or_create(&http, "task-B", None).await.unwrap();
        assert_eq!(m.creates.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalidate_forces_recreate() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        sm.get_or_create(&http, "task-A", None).await.unwrap();
        sm.invalidate("task-A").await;
        sm.get_or_create(&http, "task-A", None).await.unwrap();
        assert_eq!(m.creates.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn per_task_profile_override() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        // task-A:override → "git"(net hint 翻译路径)
        sm.get_or_create(&http, "task-A", Some("git")).await.unwrap();
        // task-B:无 override → SessionMap 默认 "shell"
        sm.get_or_create(&http, "task-B", None).await.unwrap();
        // 同 task 再查复用(不触发新 create)
        sm.get_or_create(&http, "task-A", Some("git")).await.unwrap();
        let profiles = m.profiles.lock().await.clone();
        assert_eq!(profiles, vec!["git".to_string(), "shell".to_string()],
            "per-task profile applied; reuse did not create a new session");
        assert_eq!(m.creates.load(Ordering::SeqCst), 2, "one create per task");
    }
}
