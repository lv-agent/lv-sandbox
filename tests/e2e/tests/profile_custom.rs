//! 配置驱动的自定义 profile E2E 测试
//!
//! 验证 YAML 配置的自定义 profile 通过完整链路执行

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;
use sandbox_server::config::ProfileConfig;

#[tokio::test]
async fn 自定义profile覆盖rlimit通过http执行() {
    let custom = sandbox_core::profile::SandboxProfile {
        name: "custom_rl".to_string(),
        rlimit: sandbox_core::rlimit::RlimitConfig::new()
            .cpu_seconds(10)
            .nofile(256),
        ..sandbox_core::profile::SandboxProfile::shell()
    };

    let (_tmp, app) = create_test_app_with_profiles(vec![custom]).await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("custom-rl-001", &["/bin/echo", "custom_rl"], "custom_rl"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
    assert!(result["stdout"].as_str().unwrap().contains("custom_rl"));
}

#[tokio::test]
async fn 自定义profile带extra_readonly_paths() {
    let mut custom = sandbox_core::profile::SandboxProfile::shell();
    custom.name = "custom_paths".to_string();
    custom.extra_readonly_paths = vec!["/usr/lib".into()];

    let (_tmp, app) = create_test_app_with_profiles(vec![custom]).await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("custom-path-001", &["/bin/echo", "paths_ok"], "custom_paths"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
}

#[tokio::test]
async fn 自定义profile覆盖内置shell() {
    // 重新注册 shell profile（覆盖内置的）
    let mut overridden = sandbox_core::profile::SandboxProfile::shell();
    // 覆盖 timeout 为 10s
    overridden.default_timeout = std::time::Duration::from_secs(10);

    let (_tmp, app) = create_test_app_with_profiles(vec![overridden]).await;
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request("override-shell-001", &["/bin/echo", "overridden"]),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
}

#[tokio::test]
async fn 自定义profile带custom_timeout() {
    let mut custom = sandbox_core::profile::SandboxProfile::shell();
    custom.name = "long_task".to_string();
    custom.default_timeout = std::time::Duration::from_secs(30);

    let (_tmp, app) = create_test_app_with_profiles(vec![custom]).await;
    // 不指定 timeout，使用 profile 的 default_timeout
    let (status, result) = send_and_parse::<serde_json::Value>(
        app,
        make_submit_request_with_profile("long-001", &["/bin/echo", "long"], "long_task"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Completed");
}

#[tokio::test]
async fn yaml解析profile_config转换为sandbox_profile() {
    let yaml = r#"
rlimit:
  cpu_seconds: 10
  nofile: 256
max_stdout_mb: 20
default_timeout: "30s"
extra_readonly_paths:
  - "/data/shared"
"#;

    let pc: ProfileConfig = serde_yaml::from_str(yaml).expect("YAML 解析失败");
    let profile = pc
        .to_profile("yaml_test", &sandbox_server::config::SandboxSection::default())
        .expect("转换失败");

    assert_eq!(profile.name, "yaml_test");
    assert!(profile.rlimit.cpu_seconds == Some(10));
    assert_eq!(profile.max_stdout_bytes, 20 * 1024 * 1024);
    assert_eq!(profile.default_timeout, std::time::Duration::from_secs(30));
    assert_eq!(profile.extra_readonly_paths.len(), 1);
}
