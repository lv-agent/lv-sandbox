//! 8 工具:fixus tool_name + input → lv-sandbox HTTP + 结果翻译。
//! 依赖 SandboxHttp trait(不依赖 reqwest),用 MockSandbox 单测,无需 live server。

use std::sync::Arc;

use crate::error::BridgeError;
use crate::lv_client::{JobResult, SandboxHttp};
use crate::session_map::SessionMap;

/// 工具执行产出(success + 对齐 stub 的 output 形状)。
pub struct ToolOutput {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

fn ok_exec(j: &JobResult) -> ToolOutput {
    let exit = j.exit_code.unwrap_or(-1);
    let timed_out = j.timed_out.unwrap_or(false);
    let success = !timed_out && exit == 0;
    let error = if timed_out { Some("timed out".to_string()) }
        else if !success { Some(format!("exit code {}", exit)) }
        else { None };
    ToolOutput {
        success,
        output: serde_json::json!({
            "stdout": j.stdout.clone().unwrap_or_default(),
            "stderr": j.stderr.clone().unwrap_or_default(),
            "exit_code": exit,
        }),
        error,
    }
}

pub async fn execute(
    tool_name: &str,
    input: &serde_json::Value,
    timeout_secs: u64,
    task_id: &str,
    http: &Arc<dyn SandboxHttp>,
    sessions: &SessionMap,
) -> Result<ToolOutput, BridgeError> {
    let sid = sessions.get_or_create(http, task_id).await?;
    let to = if timeout_secs > 0 { Some(format!("{}s", timeout_secs)) } else { None };
    match tool_name {
        "fixus_bash" => {
            let code = input.get("command").or_else(|| input.get("code"))
                .and_then(|v| v.as_str()).unwrap_or("echo 'no command'");
            let j = http.exec(&sid, vec!["bash".into(), "-c".into(), code.into()], to.clone(), None).await?;
            Ok(ok_exec(&j))
        }
        "fixus_jq" => {
            let filter = input.get("filter").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("filter"))?;
            let file = input.get("file").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("file"))?;
            // argv 元素独立,不经 shell:filter 含 $(...) 只被 jq 当字面过滤式
            let j = http.exec(&sid, vec!["jq".into(), "-r".into(), filter.into(), file.into()], to.clone(), None).await?;
            Ok(ok_exec(&j))
        }
        "fixus_rg" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("pattern"))?;
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let j = http.exec(&sid, vec!["rg".into(), pattern.into(), path.into()], to.clone(), None).await?;
            Ok(ok_exec(&j))
        }
        // 文件工具见 T6
        _ => Err(BridgeError::UnknownTool(tool_name.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    /// 记录 exec 调用的 argv,返回可配置 JobResult。
    struct ExecMock {
        last_argv: Mutex<Vec<String>>,
        result: JobResult,
    }
    #[async_trait::async_trait]
    impl SandboxHttp for ExecMock {
        async fn create_session(&self, _: &str, _: HashMap<String, String>, _: u64) -> Result<String, BridgeError> { Ok("sess".into()) }
        async fn exec(&self, _sid: &str, argv: Vec<String>, _: Option<String>, _: Option<String>) -> Result<JobResult, BridgeError> {
            *self.last_argv.lock().await = argv.clone();
            Ok(self.result.clone())
        }
        async fn get_file(&self, _: &str, _: &str) -> Result<Vec<u8>, BridgeError> { unreachable!() }
        async fn put_file(&self, _: &str, _: &str, _: Vec<u8>) -> Result<(), BridgeError> { unreachable!() }
        async fn head_file(&self, _: &str, _: &str) -> Result<bool, BridgeError> { unreachable!() }
        async fn find(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { unreachable!() }
        async fn search(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { unreachable!() }
        async fn destroy_session(&self, _: &str) -> Result<(), BridgeError> { unreachable!() }
    }

    fn setup(status: &str, exit: i32) -> (Arc<ExecMock>, Arc<dyn SandboxHttp>, SessionMap) {
        let m = Arc::new(ExecMock {
            last_argv: Mutex::new(vec![]),
            result: JobResult { status: status.into(), exit_code: Some(exit), stdout: Some("out".into()), stderr: Some("err".into()), duration_ms: Some(7), timed_out: Some(false) },
        });
        let http: Arc<dyn SandboxHttp> = m.clone();
        let sm = SessionMap::new("shell".into(), 3600);
        (m, http, sm)
    }

    #[tokio::test]
    async fn bash_renders_argv_and_maps_exit() {
        let (m, http, sm) = setup("Completed", 0);
        let r = execute("fixus_bash", &serde_json::json!({"command": "echo hi"}), 0, "t1", &http, &sm).await.unwrap();
        assert_eq!(*m.last_argv.lock().await, vec!["bash", "-c", "echo hi"]);
        assert!(r.success);
        assert_eq!(r.output["exit_code"], 0);
    }

    #[tokio::test]
    async fn bash_nonzero_exit_is_failure() {
        let (_m, http, sm) = setup("Completed", 2);
        let r = execute("fixus_bash", &serde_json::json!({"code": "false"}), 0, "t1", &http, &sm).await.unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap().contains("exit code 2"));
    }

    #[tokio::test]
    async fn jq_injection_is_single_argv_element() {
        // filter 含 shell 元字符:必须是独立 argv 元素,不经 shell
        let (m, http, sm) = setup("Completed", 0);
        let evil = "$(touch /tmp/pwned); rm -rf /";
        execute("fixus_jq", &serde_json::json!({"filter": evil, "file": "a.json"}), 0, "t1", &http, &sm).await.unwrap();
        let argv = m.last_argv.lock().await.clone();
        assert_eq!(argv, vec!["jq", "-r", evil, "a.json"], "filter is exactly one argv element");
        assert_eq!(argv.len(), 4);
    }

    #[tokio::test]
    async fn rg_renders_argv() {
        let (m, http, sm) = setup("Completed", 0);
        execute("fixus_rg", &serde_json::json!({"pattern": "TODO", "path": "src"}), 0, "t1", &http, &sm).await.unwrap();
        assert_eq!(*m.last_argv.lock().await, vec!["rg", "TODO", "src"]);
    }

    #[tokio::test]
    async fn timed_out_is_failure() {
        let m = Arc::new(ExecMock {
            last_argv: Mutex::new(vec![]),
            result: JobResult { status: "Killed".into(), exit_code: None, stdout: None, stderr: None, duration_ms: Some(30), timed_out: Some(true) },
        });
        let http: Arc<dyn SandboxHttp> = m.clone();
        let sm = SessionMap::new("shell".into(), 3600);
        let r = execute("fixus_bash", &serde_json::json!({"command": "sleep 999"}), 0, "t1", &http, &sm).await.unwrap();
        assert!(!r.success);
        assert_eq!(r.error.as_deref(), Some("timed out"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let (_m, http, sm) = setup("Completed", 0);
        let r = execute("fixus_nope", &serde_json::json!({}), 0, "t1", &http, &sm).await;
        assert!(matches!(r, Err(BridgeError::UnknownTool(_))));
    }
}
