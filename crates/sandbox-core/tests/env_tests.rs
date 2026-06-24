//! env 模块集成测试：build_sanitized_env()
//!
//! 测试 build_sanitized_env 从零构建白名单环境变量。

use std::collections::HashMap;
use std::path::Path;

use sandbox_core::env::build_sanitized_env;

#[test]
fn base_allowlist_contains_path_home_tmpdir_lang() {
    let workspace = Path::new("/sandboxes/job-001");
    let extra = HashMap::new();

    let env = build_sanitized_env("job-001", workspace, &extra);

    assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/bin");
    assert_eq!(env.get("HOME").unwrap(), "/sandboxes/job-001");
    assert_eq!(env.get("TMPDIR").unwrap(), "/sandboxes/job-001/tmp");
    assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
    // 只有这 4 个基础变量
    assert_eq!(env.len(), 4);
}

#[test]
fn custom_env_vars_appended_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("MY_VAR".to_string(), "my_value".to_string());
    extra.insert("ANOTHER".to_string(), "42".to_string());

    let env = build_sanitized_env("job-001", workspace, &extra);

    assert_eq!(env.get("MY_VAR").unwrap(), "my_value");
    assert_eq!(env.get("ANOTHER").unwrap(), "42");
    // 基础变量仍然存在
    assert!(env.contains_key("PATH"));
    assert!(env.contains_key("HOME"));
}

#[test]
fn allowed_proxy_vars_added_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("HTTP_PROXY".to_string(), "http://proxy:8080".to_string());
    extra.insert("HTTPS_PROXY".to_string(), "http://proxy:8080".to_string());
    extra.insert("NO_PROXY".to_string(), "localhost".to_string());

    let env = build_sanitized_env("job-001", workspace, &extra);

    assert_eq!(env.get("HTTP_PROXY").unwrap(), "http://proxy:8080");
    assert_eq!(env.get("HTTPS_PROXY").unwrap(), "http://proxy:8080");
    assert_eq!(env.get("NO_PROXY").unwrap(), "localhost");
}

#[test]
fn ssl_cert_vars_added_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("SSL_CERT_FILE".to_string(), "/etc/ssl/certs/ca.pem".to_string());
    extra.insert("SSL_CERT_DIR".to_string(), "/etc/ssl/certs".to_string());

    let env = build_sanitized_env("job-001", workspace, &extra);

    assert_eq!(env.get("SSL_CERT_FILE").unwrap(), "/etc/ssl/certs/ca.pem");
    assert_eq!(env.get("SSL_CERT_DIR").unwrap(), "/etc/ssl/certs");
}

#[test]
fn tz_var_added_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("TZ".to_string(), "Asia/Shanghai".to_string());

    let env = build_sanitized_env("job-001", workspace, &extra);

    assert_eq!(env.get("TZ").unwrap(), "Asia/Shanghai");
}

#[test]
fn different_job_ids_use_different_paths() {
    let env_a = build_sanitized_env(
        "job-001",
        Path::new("/sandboxes/job-001"),
        &HashMap::new(),
    );
    let env_b = build_sanitized_env(
        "job-002",
        Path::new("/sandboxes/job-002"),
        &HashMap::new(),
    );

    assert_eq!(env_a.get("HOME").unwrap(), "/sandboxes/job-001");
    assert_eq!(env_b.get("HOME").unwrap(), "/sandboxes/job-002");
    assert_eq!(env_a.get("TMPDIR").unwrap(), "/sandboxes/job-001/tmp");
    assert_eq!(env_b.get("TMPDIR").unwrap(), "/sandboxes/job-002/tmp");
}

#[test]
fn does_not_inherit_any_implicit_vars() {
    let workspace = Path::new("/sandboxes/job-001");
    let env = build_sanitized_env("job-001", workspace, &HashMap::new());

    // 不应该包含这些常见环境变量
    assert!(!env.contains_key("USER"));
    assert!(!env.contains_key("SHELL"));
    assert!(!env.contains_key("TERM"));
    assert!(!env.contains_key("PWD"));
    assert!(!env.contains_key("OLDPWD"));
    assert!(!env.contains_key("HOSTNAME"));
}
