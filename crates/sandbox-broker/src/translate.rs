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
    profile_override: Option<&str>,
) -> Result<ToolOutput, BridgeError> {
    let sid = sessions.get_or_create(http, task_id, profile_override).await?;
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
        "fixus_read" => {
            let file_path = input.get("file_path").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("file_path"))?;
            let bytes = http.get_file(&sid, file_path).await?;
            let content = String::from_utf8_lossy(&bytes).to_string();
            let lines: Vec<&str> = content.lines().collect();
            let offset = input.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
            let limit = input.get("limit").and_then(|v| v.as_i64());
            let start = offset.min(lines.len());
            let end = match limit { Some(l) if l > 0 => (start + l as usize).min(lines.len()), _ => lines.len() };
            Ok(ToolOutput {
                success: true,
                output: serde_json::json!({
                    "content": lines[start..end].join("\n"),
                    "total_lines": lines.len(),
                    "lines_returned": end - start,
                    "offset": start,
                }),
                error: None,
            })
        }
        "fixus_write" => {
            let file_path = input.get("file_path").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("file_path"))?;
            let content = input.get("content").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("content"))?;
            http.put_file(&sid, file_path, content.as_bytes().to_vec()).await?;
            Ok(ToolOutput {
                success: true,
                output: serde_json::json!({ "bytes_written": content.len(), "file_path": file_path }),
                error: None,
            })
        }
        "fixus_edit" => {
            let file_path = input.get("file_path").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("file_path"))?;
            let old = input.get("old_string").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("old_string"))?;
            let new = input.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            let bytes = http.get_file(&sid, file_path).await?;
            let content = String::from_utf8_lossy(&bytes).to_string();
            if !content.contains(old) {
                return Ok(ToolOutput {
                    success: false,
                    output: serde_json::Value::Null,
                    error: Some(format!("old_string not found in {}", file_path)),
                });
            }
            let replaced = content.replacen(old, new, 1);
            http.put_file(&sid, file_path, replaced.as_bytes().to_vec()).await?;
            Ok(ToolOutput {
                success: true,
                output: serde_json::json!({ "file_path": file_path, "replaced": true, "bytes_written": replaced.len() }),
                error: None,
            })
        }
        "fixus_glob" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("pattern"))?;
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let files = http.find(&sid, path, pattern).await?;
            let count = files.len();
            Ok(ToolOutput {
                success: true,
                output: serde_json::json!({ "files": files, "count": count, "pattern": pattern }),
                error: None,
            })
        }
        "fixus_grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str())
                .ok_or(BridgeError::MissingField("pattern"))?;
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let results = http.search(&sid, path, pattern).await?;
            let count = results.len();
            Ok(ToolOutput {
                success: true,
                output: serde_json::json!({ "matches": results.join("\n"), "count": count, "pattern": pattern }),
                error: None,
            })
        }
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
        let r = execute("fixus_bash", &serde_json::json!({"command": "echo hi"}), 0, "t1", &http, &sm, None).await.unwrap();
        assert_eq!(*m.last_argv.lock().await, vec!["bash", "-c", "echo hi"]);
        assert!(r.success);
        assert_eq!(r.output["exit_code"], 0);
    }

    #[tokio::test]
    async fn bash_nonzero_exit_is_failure() {
        let (_m, http, sm) = setup("Completed", 2);
        let r = execute("fixus_bash", &serde_json::json!({"code": "false"}), 0, "t1", &http, &sm, None).await.unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap().contains("exit code 2"));
    }

    #[tokio::test]
    async fn jq_injection_is_single_argv_element() {
        // filter 含 shell 元字符:必须是独立 argv 元素,不经 shell
        let (m, http, sm) = setup("Completed", 0);
        let evil = "$(touch /tmp/pwned); rm -rf /";
        execute("fixus_jq", &serde_json::json!({"filter": evil, "file": "a.json"}), 0, "t1", &http, &sm, None).await.unwrap();
        let argv = m.last_argv.lock().await.clone();
        assert_eq!(argv, vec!["jq", "-r", evil, "a.json"], "filter is exactly one argv element");
        assert_eq!(argv.len(), 4);
    }

    #[tokio::test]
    async fn rg_renders_argv() {
        let (m, http, sm) = setup("Completed", 0);
        execute("fixus_rg", &serde_json::json!({"pattern": "TODO", "path": "src"}), 0, "t1", &http, &sm, None).await.unwrap();
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
        let r = execute("fixus_bash", &serde_json::json!({"command": "sleep 999"}), 0, "t1", &http, &sm, None).await.unwrap();
        assert!(!r.success);
        assert_eq!(r.error.as_deref(), Some("timed out"));
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let (_m, http, sm) = setup("Completed", 0);
        let r = execute("fixus_nope", &serde_json::json!({}), 0, "t1", &http, &sm, None).await;
        assert!(matches!(r, Err(BridgeError::UnknownTool(_))));
    }

    /// 内存文件存储 mock:get/put 真读写,find/search 返回固定 stub。
    struct FileMock {
        files: Mutex<HashMap<String, Vec<u8>>>,
        find_ret: Vec<String>,
        search_ret: Vec<String>,
    }
    #[async_trait::async_trait]
    impl SandboxHttp for FileMock {
        async fn create_session(&self, _: &str, _: HashMap<String, String>, _: u64) -> Result<String, BridgeError> { Ok("sess".into()) }
        async fn exec(&self, _: &str, _: Vec<String>, _: Option<String>, _: Option<String>) -> Result<JobResult, BridgeError> { unreachable!() }
        async fn get_file(&self, _: &str, path: &str) -> Result<Vec<u8>, BridgeError> {
            self.files.lock().await.get(path).cloned()
                .ok_or_else(|| BridgeError::Status { status: 404, body: "not found".into() })
        }
        async fn put_file(&self, _: &str, path: &str, bytes: Vec<u8>) -> Result<(), BridgeError> {
            self.files.lock().await.insert(path.into(), bytes); Ok(())
        }
        async fn head_file(&self, _: &str, _: &str) -> Result<bool, BridgeError> { unreachable!() }
        async fn find(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { Ok(self.find_ret.clone()) }
        async fn search(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, BridgeError> { Ok(self.search_ret.clone()) }
        async fn destroy_session(&self, _: &str) -> Result<(), BridgeError> { unreachable!() }
    }

    fn file_mock(files: Vec<(&str, &str)>) -> (Arc<FileMock>, Arc<dyn SandboxHttp>, SessionMap) {
        let m = Arc::new(FileMock {
            files: Mutex::new(files.into_iter().map(|(k, v)| (k.into(), v.as_bytes().to_vec())).collect()),
            find_ret: vec!["a.rs".into(), "b.rs".into()],
            search_ret: vec!["a.rs:1:TODO".into()],
        });
        let http: Arc<dyn SandboxHttp> = m.clone();
        (m, http, SessionMap::new("shell".into(), 3600))
    }

    #[tokio::test]
    async fn read_full_then_offset_limit() {
        let (_m, http, sm) = file_mock(vec![("f", "a\nb\nc\nd\ne")]);
        let r = execute("fixus_read", &serde_json::json!({"file_path": "f"}), 0, "t", &http, &sm, None).await.unwrap();
        assert_eq!(r.output["content"], "a\nb\nc\nd\ne");
        assert_eq!(r.output["total_lines"], 5);
        let r2 = execute("fixus_read", &serde_json::json!({"file_path": "f", "offset": 1, "limit": 2}), 0, "t", &http, &sm, None).await.unwrap();
        assert_eq!(r2.output["content"], "b\nc");
        assert_eq!(r2.output["lines_returned"], 2);
    }

    #[tokio::test]
    async fn write_puts_bytes() {
        let (m, http, sm) = file_mock(vec![]);
        let r = execute("fixus_write", &serde_json::json!({"file_path": "out", "content": "hello"}), 0, "t", &http, &sm, None).await.unwrap();
        assert!(r.success);
        assert_eq!(r.output["bytes_written"], 5);
        assert_eq!(m.files.lock().await.get("out").unwrap(), b"hello");
    }

    #[tokio::test]
    async fn edit_replaces_once() {
        let (m, http, sm) = file_mock(vec![("f", "hello world")]);
        let r = execute("fixus_edit", &serde_json::json!({"file_path": "f", "old_string": "world", "new_string": "rust"}), 0, "t", &http, &sm, None).await.unwrap();
        assert!(r.success);
        assert_eq!(m.files.lock().await.get("f").unwrap(), b"hello rust");
    }

    #[tokio::test]
    async fn edit_missing_old_string_fails() {
        let (_m, http, sm) = file_mock(vec![("f", "hello")]);
        let r = execute("fixus_edit", &serde_json::json!({"file_path": "f", "old_string": "nope", "new_string": "x"}), 0, "t", &http, &sm, None).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn glob_and_grep_shape() {
        let (_m, http, sm) = file_mock(vec![]);
        let g = execute("fixus_glob", &serde_json::json!({"pattern": "*.rs"}), 0, "t", &http, &sm, None).await.unwrap();
        assert_eq!(g.output["count"], 2);
        let gr = execute("fixus_grep", &serde_json::json!({"pattern": "TODO"}), 0, "t", &http, &sm, None).await.unwrap();
        assert_eq!(gr.output["count"], 1);
        assert!(gr.output["matches"].as_str().unwrap().contains("TODO"));
    }
}
