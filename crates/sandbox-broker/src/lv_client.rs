//! lv-sandbox HTTP 客户端 + SandboxHttp 抽象(trait 让 translate.rs 可单测)。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::BridgeError;

/// exec 结果(镜像 lv-sandbox JobResponse 的关键字段)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobResult {
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub duration_ms: Option<u64>,
    pub timed_out: Option<bool>,
}

/// 抽象沙箱 HTTP 表面。LvClient = reqwest 实现;MockSandbox = 测试实现。
#[async_trait]
pub trait SandboxHttp: Send + Sync {
    async fn create_session(
        &self,
        profile: &str,
        metadata: HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<String, BridgeError>;
    async fn exec(
        &self,
        session_id: &str,
        argv: Vec<String>,
        timeout: Option<String>,
        stdin: Option<String>,
    ) -> Result<JobResult, BridgeError>;
    async fn get_file(&self, session_id: &str, path: &str) -> Result<Vec<u8>, BridgeError>;
    async fn put_file(
        &self,
        session_id: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BridgeError>;
    async fn head_file(&self, session_id: &str, path: &str) -> Result<bool, BridgeError>;
    async fn find(
        &self,
        session_id: &str,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<String>, BridgeError>;
    async fn search(
        &self,
        session_id: &str,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<String>, BridgeError>;
    async fn destroy_session(&self, session_id: &str) -> Result<(), BridgeError>;
}

/// reqwest 实现:对 lv-sandbox server 的 HTTP 调用。
pub struct LvClient {
    base: String,
    auth: Option<String>, // "Bearer <key>"
    http: reqwest::Client,
}

impl LvClient {
    pub fn new(base: String, api_key: Option<String>, timeout: Duration) -> Self {
        let auth = api_key.map(|k| format!("Bearer {k}"));
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(a) = &auth {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(a) {
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .build()
            .expect("reqwest client build");
        Self {
            base: base.trim_end_matches('/').to_string(),
            auth,
            http,
        }
    }

    pub fn arc(self) -> Arc<dyn SandboxHttp> {
        Arc::new(self)
    }

    async fn err_for(
        &self,
        resp: reqwest::Response,
    ) -> Result<reqwest::Response, BridgeError> {
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(BridgeError::Status { status, body })
        }
    }
}

#[async_trait]
impl SandboxHttp for LvClient {
    async fn create_session(
        &self,
        profile: &str,
        metadata: HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<String, BridgeError> {
        #[derive(Serialize)]
        struct Req<'a> {
            profile_name: &'a str,
            #[serde(skip_serializing_if = "HashMap::is_empty")]
            metadata: &'a HashMap<String, String>,
            timeout_secs: u64,
        }
        let resp = self
            .http
            .post(format!("{}/api/v1/sessions", self.base))
            .json(&Req {
                profile_name: profile,
                metadata: &metadata,
                timeout_secs,
            })
            .send()
            .await?;
        let resp = self.err_for(resp).await?;
        #[derive(Deserialize)]
        struct R {
            session_id: String,
        }
        Ok(resp.json::<R>().await?.session_id)
    }

    async fn exec(
        &self,
        session_id: &str,
        argv: Vec<String>,
        timeout: Option<String>,
        stdin: Option<String>,
    ) -> Result<JobResult, BridgeError> {
        #[derive(Serialize)]
        struct Req {
            argv: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            timeout: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            stdin: Option<String>,
        }
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sessions/{}/exec",
                self.base, session_id
            ))
            .json(&Req { argv, timeout, stdin })
            .send()
            .await?;
        self.err_for(resp)
            .await?
            .json::<JobResult>()
            .await
            .map_err(|e| BridgeError::Decode(e.to_string()))
    }

    async fn get_file(&self, session_id: &str, path: &str) -> Result<Vec<u8>, BridgeError> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/sessions/{}/files/{}",
                self.base, session_id, path
            ))
            .send()
            .await?;
        Ok(self.err_for(resp).await?.bytes().await?.to_vec())
    }

    async fn put_file(
        &self,
        session_id: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), BridgeError> {
        let resp = self
            .http
            .put(format!(
                "{}/api/v1/sessions/{}/files/{}",
                self.base, session_id, path
            ))
            .body(bytes)
            .send()
            .await?;
        self.err_for(resp).await?;
        Ok(())
    }

    async fn head_file(&self, session_id: &str, path: &str) -> Result<bool, BridgeError> {
        let resp = self
            .http
            .request(
                reqwest::Method::HEAD,
                format!("{}/api/v1/sessions/{}/files/{}", self.base, session_id, path),
            )
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    async fn find(
        &self,
        session_id: &str,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<String>, BridgeError> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sessions/{}/files/find",
                self.base, session_id
            ))
            .json(&serde_json::json!({ "path": path, "pattern": pattern }))
            .send()
            .await?;
        let v: serde_json::Value = self.err_for(resp).await?.json().await?;
        // lv-sandbox find 返回 {files:[{path, entry:{...}}], truncated} —— 取 path 字段。
        Ok(v["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("path").and_then(|p| p.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn search(
        &self,
        session_id: &str,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<String>, BridgeError> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/sessions/{}/files/search",
                self.base, session_id
            ))
            .json(&serde_json::json!({ "path": path, "pattern": pattern }))
            .send()
            .await?;
        let v: serde_json::Value = self.err_for(resp).await?.json().await?;
        // lv-sandbox search 返回 {results:[{path, matches:[{line, text}]}], truncated}
        // —— 展平成 grep -rn 风格 "path:line:text"(对齐 fixus stub fixus_grep 契约)。
        Ok(v["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|file| {
                        let path = file.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let matches = file.get("matches").and_then(|m| m.as_array())?;
                        Some((path, matches))
                    })
                    .flat_map(|(path, matches)| {
                        matches.iter().filter_map(move |m| {
                            let line = m.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
                            let text = m.get("text").and_then(|x| x.as_str()).unwrap_or("");
                            Some(format!("{path}:{line}:{text}"))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn destroy_session(&self, session_id: &str) -> Result<(), BridgeError> {
        let resp = self
            .http
            .delete(format!("{}/api/v1/sessions/{}", self.base, session_id))
            .send()
            .await?;
        self.err_for(resp).await?;
        Ok(())
    }
}
