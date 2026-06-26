//! cr-031: 生命周期 webhook——job/session 终态时,异步 POST AuditEvent 到配置 URL。
//!
//! 默认 urls 空 = no-op(零行为变化)。fire-and-forget:不阻塞 job;失败 3 次重试后记 warn。

use std::time::Duration;

use crate::audit::AuditEvent;

/// Webhook 分发器。
pub struct WebhookDispatcher {
    urls: Vec<String>,
    client: reqwest::Client,
}

impl WebhookDispatcher {
    pub fn new(urls: Vec<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { urls, client }
    }

    /// 空 urls 的 no-op 分发器(默认)。
    pub fn noop() -> Self {
        Self::new(vec![])
    }

    /// 终态事件分发。空 urls → no-op;否则每 url spawn 一个 POST 任务(3 次重试)。
    pub fn dispatch(&self, event: &AuditEvent) {
        if self.urls.is_empty() {
            return;
        }
        let Ok(body) = serde_json::to_string(event) else {
            return;
        };
        for url in &self.urls {
            let url = url.clone();
            let body = body.clone();
            let client = self.client.clone();
            tokio::spawn(async move {
                for attempt in 1..=3u32 {
                    match client
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(body.clone())
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => return,
                        Ok(r) => tracing::warn!(
                            url = %url, status = %r.status(), attempt,
                            "webhook non-2xx, retrying"
                        ),
                        Err(e) => tracing::warn!(
                            url = %url, error = %e, attempt,
                            "webhook POST failed, retrying"
                        ),
                    }
                    tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
                }
                tracing::warn!(url = %url, "webhook delivery failed after 3 attempts");
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditEventType;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn dispatch_posts_terminal_event() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_partial_json(serde_json::json!({
                "event_type": "JobCompleted",
                "job_id": "j1",
                "exit_code": 0,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let d = WebhookDispatcher::new(vec![format!("{}/hook", server.uri())]);
        let ev = AuditEvent::new(
            AuditEventType::JobCompleted,
            "j1",
            "shell",
            vec!["/bin/echo".into()],
            Some(0),
            None,
            Some(3),
            None,
        );
        d.dispatch(&ev);

        // fire-and-forget;给投递一点时间再校验
        tokio::time::sleep(Duration::from_millis(500)).await;
        server.verify().await;
    }

    #[tokio::test]
    async fn noop_dispatcher_does_not_post() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let d = WebhookDispatcher::noop();
        let ev = AuditEvent::new(
            AuditEventType::JobCompleted,
            "x",
            "shell",
            vec![],
            None,
            None,
            None,
            None,
        );
        d.dispatch(&ev);
        tokio::time::sleep(Duration::from_millis(200)).await;
        server.verify().await;
    }
}
