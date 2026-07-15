//! 幂等缓存:同 idempotency_key 的重复 tool_invoked 回缓存结果,不重复 exec。
//! 照搬 fixus stub 语义。bridge 重启缓存丢失 = 可能重放(fixus redo_group 已容纳)。

use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub struct IdempotentCache {
    cache: RwLock<HashMap<String, ToolResult>>,
}

impl IdempotentCache {
    pub fn new() -> Self { Self { cache: RwLock::new(HashMap::new()) } }
    pub async fn get(&self, key: &str) -> Option<ToolResult> {
        self.cache.read().await.get(key).cloned()
    }
    pub async fn put(&self, key: String, result: ToolResult) {
        self.cache.write().await.insert(key, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(success: bool) -> ToolResult {
        ToolResult { success, output: serde_json::json!({"x":1}), error: None, duration_ms: 5 }
    }

    #[tokio::test]
    async fn miss_then_hit() {
        let c = IdempotentCache::new();
        assert!(c.get("k1").await.is_none());
        c.put("k1".into(), r(true)).await;
        let got = c.get("k1").await.unwrap();
        assert!(got.success);
    }

    #[tokio::test]
    async fn distinct_keys_independent() {
        let c = IdempotentCache::new();
        c.put("a".into(), r(true)).await;
        c.put("b".into(), r(false)).await;
        assert!(c.get("a").await.unwrap().success);
        assert!(!c.get("b").await.unwrap().success);
    }
}
