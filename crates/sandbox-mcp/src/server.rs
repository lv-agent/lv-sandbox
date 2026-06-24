//! MCP Server：将 sandbox-server 能力封装为 MCP 工具。

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool_handler, tool_router};

use crate::client::SandboxHttpClient;
use crate::tools::{SandboxReloadParams, SandboxRunParams};

/// MCP Server 持有 HTTP 客户端，无状态。
pub struct SandboxMcpServer {
    pub(crate) http: SandboxHttpClient,
    // 由 #[tool_handler] 生成的 list_tools/call_tool 使用，静态分析看不到
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl SandboxMcpServer {
    pub fn new(http: SandboxHttpClient) -> Self {
        Self {
            http,
            tool_router: Self::tool_router(),
        }
    }

    /// 在安全沙箱中执行命令，返回 stdout/stderr/exit_code。
    #[rmcp::tool(description = "Run a command in the sandbox (landlock+seccomp+cgroup isolation). Returns JSON: job_id, status, exit_code, stdout, stderr, timed_out. Args: argv (required, command array), profile (shell/python/node, default shell), timeout (e.g. 30s), env, job_id (optional)")]
    async fn sandbox_run(
        &self,
        params: Parameters<SandboxRunParams>,
    ) -> Result<String, String> {
        let result = self.http.submit(&params.0).await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    /// 列出所有可用的安全 profile。
    #[rmcp::tool(description = "List all available sandbox profiles (shell/python/node and custom). No arguments.")]
    async fn sandbox_profiles(&self) -> Result<String, String> {
        let info = self.http.get_profiles().await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    }

    /// 查询 worker 并发状态。
    #[rmcp::tool(description = "Query sandbox worker status: running_jobs, max_concurrent, uptime_secs. No arguments.")]
    async fn sandbox_status(&self) -> Result<String, String> {
        let info = self.http.get_status().await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    }

    /// 热重载 sandbox-server 配置文件。
    #[rmcp::tool(description = "Hot-reload sandbox-server's YAML config (reloads profiles). Optional arg: config_path")]
    async fn sandbox_reload(
        &self,
        params: Parameters<SandboxReloadParams>,
    ) -> Result<String, String> {
        let info = self
            .http
            .reload(&params.0)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    }
}

#[tool_handler]
impl ServerHandler for SandboxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Lightweight agent sandbox MCP gateway. Use sandbox_run to run commands, sandbox_profiles to list profiles, sandbox_status to query status, sandbox_reload to hot-reload config.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::SandboxRunParams;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn make_server(uri: String) -> SandboxMcpServer {
        SandboxMcpServer::new(SandboxHttpClient::new(uri).unwrap())
    }

    fn run_params() -> Parameters<SandboxRunParams> {
        Parameters(SandboxRunParams {
            argv: vec!["/bin/echo".into(), "ok".into()],
            profile: "shell".into(),
            timeout: None,
            env: HashMap::new(),
            stdin: None,
            job_id: None,
        })
    }

    #[tokio::test]
    async fn sandbox_run_returns_job_json() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/jobs"))
            .respond_with(
                ResponseTemplate::new(202).set_body_json(serde_json::json!({
                    "job_id": "j1", "status": "Running",
                })),
            )
            .mount(&mock)
            .await;
        // job_id 自动生成，用 regex 匹配 GET 路径
        Mock::given(method("GET"))
            .and(path_regex("/api/v1/jobs/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job_id": "j1", "status": "Completed", "exit_code": 0,
                "signal": null, "stdout": "ok\n", "stderr": "", "duration_ms": 5, "timed_out": false,
            })))
            .mount(&mock)
            .await;

        let srv = make_server(mock.uri()).await;
        let result = srv.sandbox_run(run_params()).await.unwrap();
        assert!(result.contains("Completed"));
        assert!(result.contains("\"ok\\n\""));
    }

    #[tokio::test]
    async fn sandbox_profiles_returns_list() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/profiles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "profiles": ["shell", "python", "node"] }),
            ))
            .mount(&mock)
            .await;

        let srv = make_server(mock.uri()).await;
        let result = srv.sandbox_profiles().await.unwrap();
        assert!(result.contains("python"));
    }

    #[tokio::test]
    async fn sandbox_status_returns_state() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running_jobs": 0, "max_concurrent": 100, "uptime_secs": 10,
            })))
            .mount(&mock)
            .await;

        let srv = make_server(mock.uri()).await;
        let result = srv.sandbox_status().await.unwrap();
        assert!(result.contains("max_concurrent"));
        assert!(result.contains("100"));
    }

    #[tokio::test]
    async fn sandbox_reload_returns_profiles() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/reload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "profiles_loaded": ["shell"], "message": "ok",
            })))
            .mount(&mock)
            .await;

        let srv = make_server(mock.uri()).await;
        let result = srv
            .sandbox_reload(Parameters(crate::tools::SandboxReloadParams {
                config_path: None,
            }))
            .await
            .unwrap();
        assert!(result.contains("true"));
    }

    #[tokio::test]
    async fn sandbox_run_server_error_returns_err() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/submit"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock)
            .await;

        let srv = make_server(mock.uri()).await;
        let result = srv.sandbox_run(run_params()).await;
        assert!(result.is_err());
    }

    #[test]
    fn get_info_declares_tools() {
        let http = SandboxHttpClient::new("http://127.0.0.1:0").unwrap();
        let srv = SandboxMcpServer::new(http);
        let info = srv.get_info();
        assert!(info.instructions.is_some());
    }
}
