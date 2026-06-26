//! env 模块集成测试：build_sanitized_env()
//!
//! 三阶段优先级:核心默认 < profile.env(template baseline) < request custom_env(只加新 key)。
//! HOME/TMPDIR 永不可覆盖(隔离不变量)。

use std::collections::HashMap;
use std::path::Path;

use sandbox_core::env::build_sanitized_env;

fn empty() -> HashMap<String, String> {
    HashMap::new()
}

#[test]
fn base_allowlist_contains_path_home_tmpdir_lang() {
    let workspace = Path::new("/sandboxes/job-001");
    let env = build_sanitized_env("job-001", workspace, &empty(), &empty());

    assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/bin");
    assert_eq!(env.get("HOME").unwrap(), "/sandboxes/job-001");
    assert_eq!(env.get("TMPDIR").unwrap(), "/sandboxes/job-001/tmp");
    assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
    assert_eq!(env.len(), 4);
}

#[test]
fn custom_env_vars_appended_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("MY_VAR".to_string(), "my_value".to_string());
    extra.insert("ANOTHER".to_string(), "42".to_string());

    let env = build_sanitized_env("job-001", workspace, &empty(), &extra);

    assert_eq!(env.get("MY_VAR").unwrap(), "my_value");
    assert_eq!(env.get("ANOTHER").unwrap(), "42");
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

    let env = build_sanitized_env("job-001", workspace, &empty(), &extra);

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

    let env = build_sanitized_env("job-001", workspace, &empty(), &extra);

    assert_eq!(env.get("SSL_CERT_FILE").unwrap(), "/etc/ssl/certs/ca.pem");
    assert_eq!(env.get("SSL_CERT_DIR").unwrap(), "/etc/ssl/certs");
}

#[test]
fn tz_var_added_to_allowlist() {
    let workspace = Path::new("/sandboxes/job-001");
    let mut extra = HashMap::new();
    extra.insert("TZ".to_string(), "Asia/Shanghai".to_string());

    let env = build_sanitized_env("job-001", workspace, &empty(), &extra);

    assert_eq!(env.get("TZ").unwrap(), "Asia/Shanghai");
}

#[test]
fn different_job_ids_use_different_paths() {
    let env_a = build_sanitized_env(
        "job-001",
        Path::new("/sandboxes/job-001"),
        &empty(),
        &empty(),
    );
    let env_b = build_sanitized_env(
        "job-002",
        Path::new("/sandboxes/job-002"),
        &empty(),
        &empty(),
    );

    assert_eq!(env_a.get("HOME").unwrap(), "/sandboxes/job-001");
    assert_eq!(env_b.get("HOME").unwrap(), "/sandboxes/job-002");
    assert_eq!(env_a.get("TMPDIR").unwrap(), "/sandboxes/job-001/tmp");
    assert_eq!(env_b.get("TMPDIR").unwrap(), "/sandboxes/job-002/tmp");
}

#[test]
fn does_not_inherit_any_implicit_vars() {
    let workspace = Path::new("/sandboxes/job-001");
    let env = build_sanitized_env("job-001", workspace, &empty(), &empty());

    assert!(!env.contains_key("USER"));
    assert!(!env.contains_key("SHELL"));
    assert!(!env.contains_key("TERM"));
    assert!(!env.contains_key("PWD"));
    assert!(!env.contains_key("OLDPWD"));
    assert!(!env.contains_key("HOSTNAME"));
}

// ==================== cr-025: profile.env(template baseline) ====================

#[test]
fn profile_env_overrides_path_and_adds_keys() {
    let ws = Path::new("/s/j");
    let mut pe = HashMap::new();
    pe.insert("PATH".to_string(), "/opt/t/bin:/usr/bin:/bin".to_string());
    pe.insert("PYTHONPATH".to_string(), "/opt/t".to_string());
    let env = build_sanitized_env("j", ws, &pe, &empty());
    assert_eq!(env.get("PATH").unwrap(), "/opt/t/bin:/usr/bin:/bin");
    assert_eq!(env.get("PYTHONPATH").unwrap(), "/opt/t");
}

#[test]
fn profile_env_cannot_override_protected_home_tmpdir() {
    let ws = Path::new("/s/j");
    let mut pe = HashMap::new();
    pe.insert("HOME".to_string(), "/evil".to_string());
    pe.insert("TMPDIR".to_string(), "/evil/tmp".to_string());
    let env = build_sanitized_env("j", ws, &pe, &empty());
    assert_eq!(env.get("HOME").unwrap(), "/s/j");
    assert_eq!(env.get("TMPDIR").unwrap(), "/s/j/tmp");
}

#[test]
fn custom_env_cannot_override_profile_env() {
    let ws = Path::new("/s/j");
    let mut pe = HashMap::new();
    pe.insert("FOO".to_string(), "profile".to_string());
    let mut extra = HashMap::new();
    extra.insert("FOO".to_string(), "request".to_string());
    let env = build_sanitized_env("j", ws, &pe, &extra);
    assert_eq!(env.get("FOO").unwrap(), "profile");
}

#[test]
fn custom_env_cannot_override_protected_core() {
    let ws = Path::new("/s/j");
    let mut extra = HashMap::new();
    extra.insert("HOME".to_string(), "/evil".to_string());
    let env = build_sanitized_env("j", ws, &empty(), &extra);
    assert_eq!(env.get("HOME").unwrap(), "/s/j");
}
