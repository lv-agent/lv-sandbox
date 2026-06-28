//! cr-042: 速率限制中间件(固定窗口,DashMap per-IP)。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;

use crate::config::RateLimitConfig;

/// 固定窗口内 per-IP 状态
struct WindowState {
    count: u64,
    window_start: Instant,
}

/// 速率限制器
pub struct RateLimiter {
    requests_per_window: u64,
    window_duration: Duration,
    windows: DashMap<IpAddr, WindowState>,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            requests_per_window: config.requests_per_window,
            window_duration: Duration::from_secs(config.window_secs),
            windows: DashMap::new(),
        }
    }

    /// 检查指定 IP 是否允许此次请求。
    /// 返回 Ok(()) = 放行; Err(RateLimitExceeded) = 429
    pub fn check(&self, ip: IpAddr) -> Result<(), RateLimitExceeded> {
        let now = Instant::now();
        let mut entry = self.windows.entry(ip).or_insert_with(|| WindowState {
            count: 0,
            window_start: now,
        });

        // 窗口过期则重置
        if now.duration_since(entry.window_start) > self.window_duration {
            entry.count = 1;
            entry.window_start = now;
            return Ok(());
        }

        entry.count += 1;
        if entry.count > self.requests_per_window {
            // 超限——回退,返回错误
            entry.count -= 1;
            return Err(RateLimitExceeded);
        }

        Ok(())
    }

    /// 当前活跃 IP 窗口数(供测试/调试)
    pub fn active_windows(&self) -> usize {
        self.windows.len()
    }
}

/// 速率超限错误
#[derive(Debug)]
pub struct RateLimitExceeded;

impl IntoResponse for RateLimitExceeded {
    fn into_response(self) -> Response {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded",
        )
            .into_response()
    }
}

/// 从请求中提取客户端 IP。
/// 优先 X-Forwarded-For(最右地址),否则尝试从连接信息获取。
fn extract_client_ip<B>(req: &axum::http::Request<B>) -> Option<IpAddr> {
    // X-Forwarded-For: "client, proxy1, proxy2" → 取最右(离自己最近的代理)
    if let Some(fwd) = req.headers().get("x-forwarded-for") {
        if let Ok(val) = fwd.to_str() {
            if let Some(ip_str) = val.split(',').next_back().map(|s| s.trim()) {
                if let Ok(ip) = ip_str.parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

/// axum 中间件: 速率限制
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    // /health 豁免
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    if let Some(ip) = extract_client_ip(&req) {
        if limiter.check(ip).is_err() {
            crate::metrics::RATE_LIMIT_DENIED_TOTAL.inc();
            return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
        }
    }
    // 无法提取 IP —— 放行(不因为缺 IP 头而拒绝)

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// 固定窗口内未超限放行。
    #[test]
    fn allows_within_limit() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_window: 5,
            window_secs: 60,
        };
        let limiter = RateLimiter::new(&config);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        for _ in 0..5 {
            assert!(limiter.check(ip).is_ok(), "should allow within limit");
        }
    }

    /// 超限后返回错误。
    #[test]
    fn rejects_when_limit_exceeded() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_window: 3,
            window_secs: 60,
        };
        let limiter = RateLimiter::new(&config);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        for _ in 0..3 {
            assert!(limiter.check(ip).is_ok());
        }
        assert!(limiter.check(ip).is_err(), "4th request should be denied");
    }

    /// 不同 IP 独立计数。
    #[test]
    fn different_ips_independent() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_window: 2,
            window_secs: 60,
        };
        let limiter = RateLimiter::new(&config);
        let ip_a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        assert!(limiter.check(ip_a).is_ok());
        assert!(limiter.check(ip_a).is_ok());
        assert!(limiter.check(ip_a).is_err(), "ip_a should be denied after 2");
        assert!(limiter.check(ip_b).is_ok(), "ip_b should still have capacity");
        assert!(limiter.check(ip_b).is_ok());
    }

    /// X-Forwarded-For 取最右 IP。
    #[test]
    fn extract_ip_from_x_forwarded_for_rightmost() {
        use axum::http::Request as HttpRequest;
        let req = HttpRequest::builder()
            .header("x-forwarded-for", "1.2.3.4, 10.0.0.1")
            .body(())
            .unwrap();
        let ip = extract_client_ip(&req);
        assert_eq!(ip, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    /// 单 IP 无逗号。
    #[test]
    fn extract_ip_from_x_forwarded_for_single() {
        use axum::http::Request as HttpRequest;
        let req = HttpRequest::builder()
            .header("x-forwarded-for", "10.0.0.1")
            .body(())
            .unwrap();
        let ip = extract_client_ip(&req);
        assert_eq!(ip, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    /// 无 X-Forwarded-For 返回 None。
    #[test]
    fn extract_ip_missing_header_returns_none() {
        use axum::http::Request as HttpRequest;
        let req = HttpRequest::builder().body(()).unwrap();
        assert!(extract_client_ip(&req).is_none());
    }

    /// active_windows 跟踪活跃 IP 数。
    #[test]
    fn active_windows_tracks_distinct_ips() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_window: 1,
            window_secs: 60,
        };
        let limiter = RateLimiter::new(&config);
        let _ = limiter.check(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        let _ = limiter.check(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(limiter.active_windows(), 2);
    }
}
