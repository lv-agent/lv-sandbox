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
    #[rmcp::tool(description = "在安全沙箱中执行命令（landlock+seccomp+cgroup 隔离）。返回 JSON：job_id, status, exit_code, stdout, stderr, timed_out。参数：argv(必填,命令数组), profile(shell/python/node,默认shell), timeout(如30s), env(环境变量), job_id(可选)")]
    async fn sandbox_run(
        &self,
        params: Parameters<SandboxRunParams>,
    ) -> Result<String, String> {
        let result = self.http.submit(&params.0).await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    /// 列出所有可用的安全 profile。
    #[rmcp::tool(description = "列出所有可用的安全 profile（shell/python/node 及自定义），无需参数")]
    async fn sandbox_profiles(&self) -> Result<String, String> {
        let info = self.http.get_profiles().await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    }

    /// 查询 worker 并发状态。
    #[rmcp::tool(description = "查询 sandbox worker 状态：running_jobs, max_concurrent, uptime_secs，无需参数")]
    async fn sandbox_status(&self) -> Result<String, String> {
        let info = self.http.get_status().await.map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    }

    /// 热重载 sandbox-server 配置文件。
    #[rmcp::tool(description = "热重载 sandbox-server 的 YAML 配置（重新加载 profile）。可选参数 config_path")]
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
            "轻量级 Agent 沙箱 MCP 网关。使用 sandbox_run 执行命令，sandbox_profiles 查看可用 profile，sandbox_status 查询状态，sandbox_reload 热重载配置。",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::SandboxRunParams;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
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
    async fn sandbox_run_返回job结果json() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/submit"))
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
    async fn sandbox_profiles_返回列表() {
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
    async fn sandbox_status_返回状态() {
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
    async fn sandbox_reload_返回profile列表() {
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
    async fn sandbox_run_server错误返回err() {
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
    fn get_info_声明工具能力() {
        let http = SandboxHttpClient::new("http://127.0.0.1:0").unwrap();
        let srv = SandboxMcpServer::new(http);
        let info = srv.get_info();
        assert!(info.instructions.is_some());
    }
}
