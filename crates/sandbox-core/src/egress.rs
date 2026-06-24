//! cr-019: 出站白名单规则与匹配。默认拒绝(空规则集 → 任何请求都拒绝)。

use serde::{Deserialize, Serialize};

/// 单条出站白名单规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressRule {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub port: Option<u16>,
}

/// 出站白名单匹配器。默认拒绝(空规则集 → 任何请求都拒绝)。
#[derive(Debug, Clone)]
pub struct AllowlistMatcher {
    rules: Vec<NormalizedRule>,
}

#[derive(Debug, Clone)]
struct NormalizedRule {
    host: String,
    port: Option<u16>,
}

impl AllowlistMatcher {
    pub fn new(rules: Vec<EgressRule>) -> Self {
        let rules = rules
            .into_iter()
            .map(|r| NormalizedRule {
                host: r.host.to_lowercase(),
                port: r.port,
            })
            .collect();
        Self { rules }
    }

    /// 校验 (host, port) 是否被任一规则放行。
    pub fn is_allowed(&self, host: &str, port: u16) -> bool {
        let host = host.to_lowercase();
        self.rules
            .iter()
            .any(|r| host_matches(&host, &r.host) && port_matches(port, r.port))
    }
}

/// 通配 `*` 只匹配最左恰好一个 label(同 x509/TLS SAN 语义)。
/// `*.example.com` 命中 `a.example.com`,不命中 `a.b.example.com`、不命中 `example.com`。
fn host_matches(host: &str, rule_host: &str) -> bool {
    if let Some(suffix) = rule_host.strip_prefix('*') {
        // suffix 以 '.' 开头(如 ".example.com")
        match host.strip_suffix(suffix) {
            Some(prefix) => !prefix.is_empty() && !prefix.contains('.'),
            None => false,
        }
    } else {
        host == rule_host
    }
}

fn port_matches(port: u16, rule_port: Option<u16>) -> bool {
    match rule_port {
        None => true,
        Some(p) => p == port,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空白名单拒绝一切() {
        let m = AllowlistMatcher::new(vec![]);
        assert!(!m.is_allowed("any.host", 443));
    }

    #[test]
    fn 精确host命中() {
        let m = AllowlistMatcher::new(vec![EgressRule {
            host: "api.openai.com".into(),
            port: None,
        }]);
        assert!(m.is_allowed("api.openai.com", 443));
        assert!(m.is_allowed("api.openai.com", 80)); // port=None → 任意端口
        assert!(!m.is_allowed("other.com", 443));
    }

    #[test]
    fn 端口限定() {
        let m = AllowlistMatcher::new(vec![EgressRule {
            host: "api.openai.com".into(),
            port: Some(443),
        }]);
        assert!(m.is_allowed("api.openai.com", 443));
        assert!(!m.is_allowed("api.openai.com", 80));
    }

    #[test]
    fn 通配匹配最左单label() {
        let m = AllowlistMatcher::new(vec![EgressRule {
            host: "*.pypi.org".into(),
            port: None,
        }]);
        assert!(m.is_allowed("download.pypi.org", 443)); // 单 label
        assert!(!m.is_allowed("a.b.pypi.org", 443)); // 多级 → 不命中
        assert!(!m.is_allowed("pypi.org", 443)); // base 本身 → 不命中
    }

    #[test]
    fn 大小写不敏感() {
        let m = AllowlistMatcher::new(vec![EgressRule {
            host: "API.OpenAI.com".into(),
            port: None,
        }]);
        assert!(m.is_allowed("api.openai.com", 443));
        assert!(m.is_allowed("API.OPENAI.COM", 443));
    }
}
