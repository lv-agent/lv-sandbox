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

    /// 提交 job 并等待完成（cr-018 异步：POST /jobs + 轮询 GET /jobs/{id}）。
    /// 对 MCP 客户端保持同步语义（提交并拿结果）。
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
            "stdin": params.stdin,
        });
        // POST /jobs（create，立即返回 job_id）
        let resp = self
            .client
            .post(format!("{}/api/v1/jobs", self.base_url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("submit 失败: HTTP {}", resp.status()));
        }
        // 轮询 GET /jobs/{id} 直到终态（上限 300s）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!("job {} 轮询超时（300s）", job_id));
            }
            let get = self
                .client
                .get(format!("{}/api/v1/jobs/{}", self.base_url, job_id))
                .send()
                .await?;
            if !get.status().is_success() {
                return Err(anyhow!("get_job 失败: HTTP {}", get.status()));
            }
            let json: serde_json::Value = get.json().await?;
            if json["status"].as_str() == Some("Running") {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
            return Ok(JobResultInfo {
                job_id: json["job_id"].as_str().unwrap_or("").to_string(),
                status: json["status"].as_str().unwrap_or("").to_string(),
                exit_code: json["exit_code"].as_i64().map(|i| i as i32),
                signal: json["signal"].as_i64().map(|i| i as i32),
                stdout: json["stdout"].as_str().unwrap_or("").to_string(),
                stderr: json["stderr"].as_str().unwrap_or("").to_string(),
                duration_ms: json["duration_ms"].as_i64().unwrap_or(0) as u64,
                timed_out: json["timed_out"].as_bool().unwrap_or(false),
            });
        }
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
    use wiremock::matchers::{body_partial_json, method, path, path_regex};
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
        // POST /jobs → 202 Running
        Mock::given(method("POST"))
            .and(path("/api/v1/jobs"))
            .and(body_partial_json(serde_json::json!({
                "argv": ["/bin/echo", "hi"],
                "profile_name": "shell",
                "job_id": "job-1",
            })))
            .respond_with(
                ResponseTemplate::new(202).set_body_json(serde_json::json!({
                    "job_id": "job-1",
                    "status": "Running",
                })),
            )
            .mount(&server)
            .await;
        // GET /jobs/job-1 → 200 Done
        Mock::given(method("GET"))
            .and(path("/api/v1/jobs/job-1"))
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
            .and(path("/api/v1/jobs"))
            .respond_with(
                ResponseTemplate::new(202).set_body_json(serde_json::json!({
                    "job_id": "auto",
                    "status": "Running",
                })),
            )
            .mount(&server)
            .await;
        // job_id 自动生成（UUID），用 regex 匹配 GET 路径
        Mock::given(method("GET"))
            .and(path_regex("/api/v1/jobs/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job_id": "auto",
                "status": "Completed",
                "exit_code": 0,
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
            .and(path("/api/v1/jobs"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = SandboxHttpClient::new(server.uri()).unwrap();
        let result = client.submit(&run_params(None)).await;
        assert!(result.is_err(), "5xx 应返回错误");
    }
}
