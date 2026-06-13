use std::collections::HashMap;
use std::path::Path;

/// 从零构建最小白名单环境变量。
/// 不继承 runner 的任何环境变量。
pub fn build_sanitized_env(
    _job_id: &str,
    job_workspace: &Path,
    extra: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // 基础白名单
    env.insert("PATH", "/usr/bin:/bin".to_string());
    env.insert(
        "HOME",
        job_workspace.to_string_lossy().to_string(),
    );
    env.insert(
        "TMPDIR",
        job_workspace.join("tmp").to_string_lossy().to_string(),
    );
    env.insert("LANG", "C.UTF-8".to_string());

    // 按需添加
    if let Some(v) = extra.get("TZ") {
        env.insert("TZ", v.clone());
    }
    if let Some(v) = extra.get("SSL_CERT_FILE") {
        env.insert("SSL_CERT_FILE", v.clone());
    }
    if let Some(v) = extra.get("SSL_CERT_DIR") {
        env.insert("SSL_CERT_DIR", v.clone());
    }
    if let Some(v) = extra.get("HTTP_PROXY") {
        env.insert("HTTP_PROXY", v.clone());
    }
    if let Some(v) = extra.get("HTTPS_PROXY") {
        env.insert("HTTPS_PROXY", v.clone());
    }
    if let Some(v) = extra.get("NO_PROXY") {
        env.insert("NO_PROXY", v.clone());
    }

    // 加入 extra 中的其他自定义变量
    for (key, value) in extra {
        if !env.contains_key(key.as_str()) {
            env.insert(key, value.clone());
        }
    }

    // 将 key 从 &str 转为 String
    env.into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}
