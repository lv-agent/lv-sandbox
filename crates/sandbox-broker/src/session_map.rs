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
    pub async fn get_or_create(
        &self,
        http: &Arc<dyn SandboxHttp>,
        task_id: &str,
    ) -> Result<String, BridgeError> {
        // fast path
        if let Some(sid) = self.map.lock().await.get(task_id).cloned() {
            return Ok(sid);
        }
        let mut metadata = HashMap::new();
        metadata.insert("fixus_task_id".to_string(), task_id.to_string());
        let sid = http.create_session(&self.profile, metadata, self.timeout_secs).await?;
        self.map.lock().await.insert(task_id.to_string(), sid.clone());
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
    }
    #[async_trait::async_trait]
    impl SandboxHttp for CountingMock {
        async fn create_session(&self, _p: &str, _m: HashMap<String, String>, _t: u64)
            -> Result<String, BridgeError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
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
        Arc::new(CountingMock { creates: AtomicUsize::new(0), first_id: "sess-1".into() })
    }

    #[tokio::test]
    async fn first_call_creates_second_reuses() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        let a = sm.get_or_create(&http, "task-A").await.unwrap();
        let b = sm.get_or_create(&http, "task-A").await.unwrap();
        assert_eq!(a, "sess-1");
        assert_eq!(a, b, "same task reuses same session");
        assert_eq!(m.creates.load(Ordering::SeqCst), 1, "create_session called exactly once");
    }

    #[tokio::test]
    async fn distinct_tasks_distinct_lookups() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        sm.get_or_create(&http, "task-A").await.unwrap();
        sm.get_or_create(&http, "task-B").await.unwrap();
        assert_eq!(m.creates.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalidate_forces_recreate() {
        let m = mock();
        let sm = SessionMap::new("shell".into(), 3600);
        let http: Arc<dyn SandboxHttp> = m.clone();
        sm.get_or_create(&http, "task-A").await.unwrap();
        sm.invalidate("task-A").await;
        sm.get_or_create(&http, "task-A").await.unwrap();
        assert_eq!(m.creates.load(Ordering::SeqCst), 2);
    }
}
