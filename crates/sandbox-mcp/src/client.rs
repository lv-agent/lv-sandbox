//! HTTP 客户端：调用 sandbox-server REST API。
//!
//! 封装对 sandbox-server 的 HTTP 调用，由 MCP 工具复用。

use crate::tools::{
    JobResultInfo, ProfilesInfo, ReloadInfo, SandboxReloadParams, SandboxRunParams, StatusInfo,
};
use anyhow::{anyhow, Result};
use uuid::Uuid;

/// 封装对 sandbox-server 的 HTTP 调用。
///
/// `base_url` 形如 `http://127.0.0.1:8080`（不带尾斜杠）。
pub struct SandboxHttpClient {
    base_url: String,
    client: reqwest::Client,
}

impl SandboxHttpClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder().build()?,
        })
    }

    /// sandbox-server 基地址
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 提交 job。未提供 job_id 时自动生成 UUID。
    pub async fn submit(&self, params: &SandboxRunParams) -> Result<JobResultInfo> {
        let job_id = params
            .job_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let body = serde_json::json!({
            "job_id": job_id,
            "argv": params.argv,
            "profile_name": params.profile,
            "timeout": params.timeout,
            "custom_env": params.env,
        });
        let resp = self
            .client
            .post(format!("{}/api/v1/submit", self.base_url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("submit 失败: HTTP {}", resp.status()));
        }
        Ok(resp.json::<JobResultInfo>().await?)
    }

    /// 列出可用 profile
    pub async fn get_profiles(&self) -> Result<ProfilesInfo> {
        let resp = self
            .client
            .get(format!("{}/api/v1/profiles", self.base_url))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("get_profiles 失败: HTTP {}", resp.status()));
        }
        Ok(resp.json::<ProfilesInfo>().await?)
    }

    /// 查询 worker 状态
    pub async fn get_status(&self) -> Result<StatusInfo> {
        let resp = self
            .client
            .get(format!("{}/api/v1/status", self.base_url))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("get_status 失败: HTTP {}", resp.status()));
        }
        Ok(resp.json::<StatusInfo>().await?)
    }

    /// 热重载配置。config_path 作为 query 传递（server 当前用启动路径）。
    pub async fn reload(&self, params: &SandboxReloadParams) -> Result<ReloadInfo> {
        let mut req = self
            .client
            .post(format!("{}/api/v1/reload", self.base_url));
        if let Some(ref p) = params.config_path {
            req = req.query(&[("path", p)]);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("reload 失败: HTTP {}", resp.status()));
        }
        Ok(resp.json::<ReloadInfo>().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::*;
    use std::collections::HashMap;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn run_params(job_id: Option<&str>) -> SandboxRunParams {
        SandboxRunParams {
            argv: vec!["/bin/echo".into(), "hi".into()],
            profile: "shell".into(),
            timeout: None,
            env: HashMap::new(),
            stdin: None,
            job_id: job_id.map(String::from),
        }
    }

    #[tokio::test]
    async fn submit_发送请求并解析响应() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/submit"))
            .and(body_partial_json(serde_json::json!({
                "argv": ["/bin/echo", "hi"],
                "profile_name": "shell",
                "job_id": "job-1",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job_id": "job-1",
                "status": "Completed",
                "exit_code": 0,
                "signal": null,
                "stdout": "hi\n",
                "stderr": "",
                "duration_ms": 10,
                "timed_out": false,
            })))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let result = client.submit(&run_params(Some("job-1"))).await.unwrap();
        assert_eq!(result.job_id, "job-1");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "hi\n");
        assert_eq!(result.status, "Completed");
    }

    #[tokio::test]
    async fn submit_未提供job_id自动生成非空() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/submit"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job_id": "auto",
                "status": "Completed",
                "exit_code": 0,
                "signal": null,
                "stdout": "",
                "stderr": "",
                "duration_ms": 1,
                "timed_out": false,
            })))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        // 不提供 job_id
        let result = client.submit(&run_params(None)).await.unwrap();
        assert_eq!(result.status, "Completed");
    }

    #[tokio::test]
    async fn get_profiles_返回profile列表() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/profiles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "profiles": ["shell", "python", "node"] }),
            ))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let info = client.get_profiles().await.unwrap();
        assert_eq!(info.profiles, vec!["shell", "python", "node"]);
    }

    #[tokio::test]
    async fn get_status_返回worker状态() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "running_jobs": 3,
                "max_concurrent": 100,
                "uptime_secs": 3600,
            })))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let info = client.get_status().await.unwrap();
        assert_eq!(info.running_jobs, 3);
        assert_eq!(info.max_concurrent, 100);
    }

    #[tokio::test]
    async fn reload_返回新profile列表() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/reload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "profiles_loaded": ["shell", "custom"],
                "message": "ok",
            })))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let info = client
            .reload(&SandboxReloadParams { config_path: None })
            .await
            .unwrap();
        assert!(info.success);
        assert_eq!(info.profiles_loaded, vec!["shell", "custom"]);
    }

    #[tokio::test]
    async fn submit_server错误返回错误() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/submit"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let result = client.submit(&run_params(None)).await;
        assert!(result.is_err(), "5xx 应返回错误");
    }
}
