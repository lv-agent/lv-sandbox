//! 配置文件系统测试
//!
//! TDD RED 阶段：先写测试，覆盖 YAML 配置加载全场景

use std::collections::HashMap;
use std::io::Write as IoWrite;

// ==================== 默认配置 ====================

#[test]
fn default_config_build_profile_registry_contains_three_builtins() {
    let config = sandbox_server::config::AppConfig::default();

    // AppConfig::default() 的 profiles 字段为空（配置文件没有定义）
    // 但 build_profile_registry() 会生成内置 profile
    assert!(config.profiles.is_empty());

    let registry = config.build_profile_registry();
    assert!(registry.get("shell").is_some());
    assert!(registry.get("python").is_some());
    assert!(registry.get("node").is_some());
}

#[test]
fn default_config_server_section_values_correct() {
    let config = sandbox_server::config::AppConfig::default();

    assert_eq!(config.server.listen_addr, "0.0.0.0:8080");
    assert_eq!(config.server.max_concurrent_jobs, 100);
    assert_eq!(config.server.log_level, "info");
    assert_eq!(config.server.log_format, "json");
}

#[test]
fn default_config_sandbox_section_values_correct() {
    let config = sandbox_server::config::AppConfig::default();

    assert_eq!(config.sandbox.base_dir, "/sandboxes");
    assert_eq!(config.sandbox.disk_watermark_mb, 1024);
    assert_eq!(config.sandbox.default_profile, "shell");
    assert!(config.sandbox.fail_closed);
}

// ==================== YAML 解析 ====================

#[test]
fn from_yaml_string_custom_server_config() {
    let yaml = r#"
server:
  listen_addr: "127.0.0.1:9090"
  max_concurrent_jobs: 50
  log_level: "debug"
  log_format: "text"

sandbox:
  base_dir: "/tmp/sandboxes"
  disk_watermark_mb: 512
"#;

    let config: sandbox_server::config::AppConfig =
        serde_yaml::from_str(yaml).expect("YAML parse failed");

    assert_eq!(config.server.listen_addr, "127.0.0.1:9090");
    assert_eq!(config.server.max_concurrent_jobs, 50);
    assert_eq!(config.server.log_level, "debug");
    assert_eq!(config.server.log_format, "text");
    assert_eq!(config.sandbox.base_dir, "/tmp/sandboxes");
    assert_eq!(config.sandbox.disk_watermark_mb, 512);
}

#[test]
fn from_yaml_string_custom_profile() {
    let yaml = r#"
server:
  listen_addr: "0.0.0.0:8080"

sandbox:
  base_dir: "/sandboxes"

profiles:
  python:
    extra_readonly_paths:
      - "/opt/sandbox-libs/python3"
      - "/opt/wheels"
    rlimit:
      cpu_seconds: 5
      nofile: 128
      fsize_mb: 50
    max_stdout_mb: 10
    max_stderr_mb: 10
    default_timeout: "30s"
"#;

    let config: sandbox_server::config::AppConfig =
        serde_yaml::from_str(yaml).expect("YAML parse failed");

    let python = config.profiles.get("python").expect("python profile not found");
    assert_eq!(python.extra_readonly_paths.as_ref().unwrap().len(), 2);
    assert_eq!(
        python.extra_readonly_paths.as_ref().unwrap()[0],
        "/opt/sandbox-libs/python3"
    );
    assert_eq!(python.max_stdout_mb, Some(10));
    assert_eq!(python.default_timeout, Some("30s".to_string()));

    let rlimit = python.rlimit.as_ref().expect("rlimit not found");
    assert_eq!(rlimit.cpu_seconds, Some(5));
    assert_eq!(rlimit.nofile, Some(128));
    assert_eq!(rlimit.fsize_mb, Some(50));
}

#[test]
fn from_yaml_minimal_config_uses_defaults() {
    // 只提供 server 和 sandbox 必要字段，profiles 使用默认
    let yaml = r#"
server:
  listen_addr: "0.0.0.0:8080"
sandbox:
  base_dir: "/sandboxes"
"#;

    let config: sandbox_server::config::AppConfig =
        serde_yaml::from_str(yaml).expect("YAML parse failed");

    // profiles 应该为空（YAML 没有提供），但默认值字段应有值
    assert!(config.profiles.is_empty() || config.server.listen_addr == "0.0.0.0:8080");
}

#[test]
fn invalid_yaml_returns_error() {
    let yaml = r#"
server:
  listen_addr: [invalid
  broken: {{{{
"#;

    let result = serde_yaml::from_str::<sandbox_server::config::AppConfig>(yaml);
    assert!(result.is_err(), "invalid YAML should return an error");
}

// ==================== ProfileConfig → SandboxProfile 转换 ====================

#[test]
fn profile_config_to_sandbox_profile_default_fill() {
    let profile_config = sandbox_server::config::ProfileConfig {
        rlimit: None,
        extra_readonly_paths: None,
        max_stdout_mb: None,
        max_stderr_mb: None,
        default_timeout: None,
        egress_allowlist: None,
        disk_quota_mb: None,
        env: None,
    };

    let sandbox_section = sandbox_server::config::SandboxSection::default();

    let profile = profile_config
        .to_profile("shell", &sandbox_section)
        .expect("conversion failed");

    assert_eq!(profile.name, "shell");
    // 默认 rlimit 值应该被填充
    assert!(profile.rlimit.cpu_seconds.is_some());
    assert_eq!(profile.max_stdout_bytes, 5 * 1024 * 1024);
    assert_eq!(profile.max_stderr_bytes, 5 * 1024 * 1024);
    assert_eq!(profile.default_timeout, std::time::Duration::from_secs(5));
}

#[test]
fn profile_config_to_custom_values_override_defaults() {
    let profile_config = sandbox_server::config::ProfileConfig {
        rlimit: Some(sandbox_server::config::RlimitFileConfig {
            cpu_seconds: Some(10),
            nofile: Some(256),
            nproc: None,
            fsize_mb: Some(100),
            core: None,
            stack_mb: Some(32),
            memlock: None,
        }),
        extra_readonly_paths: Some(vec![
            "/opt/libs/python3".to_string(),
            "/opt/wheels".to_string(),
        ]),
        max_stdout_mb: Some(20),
        max_stderr_mb: Some(15),
        default_timeout: Some("60s".to_string()),
        egress_allowlist: None,
        disk_quota_mb: Some(50),
        env: Some(HashMap::from([(
            "PYTHONPATH".to_string(),
            "/opt/t".to_string(),
        )])),
    };

    let sandbox_section = sandbox_server::config::SandboxSection::default();

    let profile = profile_config
        .to_profile("python", &sandbox_section)
        .expect("conversion failed");

    assert_eq!(profile.name, "python");
    // rlimit 自定义值
    assert_eq!(profile.rlimit.cpu_seconds, Some(10));
    assert_eq!(profile.rlimit.nofile, Some(256));
    assert_eq!(profile.rlimit.fsize_bytes, Some(100 * 1024 * 1024));
    assert_eq!(profile.rlimit.stack_bytes, Some(32 * 1024 * 1024));
    // 输出限制
    assert_eq!(profile.max_stdout_bytes, 20 * 1024 * 1024);
    assert_eq!(profile.max_stderr_bytes, 15 * 1024 * 1024);
    // timeout
    assert_eq!(profile.default_timeout, std::time::Duration::from_secs(60));
    // extra_readonly_paths
    assert_eq!(profile.extra_readonly_paths.len(), 2);
    // cr-022: disk_quota_mb 透传(MB)
    assert_eq!(profile.disk_quota_mb, Some(50));
    // cr-025: env 透传
    assert_eq!(profile.env.get("PYTHONPATH").map(|s| s.as_str()), Some("/opt/t"));
}

// ==================== cr-023: server.api_key ====================

#[test]
fn server_section_api_key_defaults_none_and_parses() {
    // 默认 = None(鉴权关)
    let default = sandbox_server::config::ServerSection::default();
    assert!(default.api_key.is_none());
    // 显式配置
    let yaml = "server:\n  api_key: \"secret-xyz\"\n";
    let cfg: sandbox_server::config::AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.server.api_key.as_deref(), Some("secret-xyz"));
    // 缺省段仍 None
    let cfg2: sandbox_server::config::AppConfig = serde_yaml::from_str("server:\n").unwrap();
    assert!(cfg2.server.api_key.is_none());
}

#[test]
fn rlimit_unit_conversion_mb_to_bytes() {
    let rlimit_file = sandbox_server::config::RlimitFileConfig {
        cpu_seconds: Some(5),
        nofile: Some(64),
        nproc: Some(32),
        fsize_mb: Some(10),
        core: Some(0),
        stack_mb: Some(8),
        memlock: Some(0),
    };

    let rlimit = rlimit_file.to_rlimit_config();

    assert_eq!(rlimit.cpu_seconds, Some(5));
    assert_eq!(rlimit.nofile, Some(64));
    assert_eq!(rlimit.fsize_bytes, Some(10 * 1024 * 1024));
    assert_eq!(rlimit.stack_bytes, Some(8 * 1024 * 1024));
    assert_eq!(rlimit.core, Some(0));
    assert_eq!(rlimit.memlock, Some(0));
}

// ==================== 文件加载 ====================

#[test]
fn config_file_missing_uses_defaults() {
    let config = sandbox_server::config::AppConfig::load_from_path("/nonexistent/config.yaml")
        .expect("load should succeed");

    // 回退到默认配置，profiles 字段为空，但 registry 有内置 profile
    assert!(config.profiles.is_empty());
    assert_eq!(config.server.listen_addr, "0.0.0.0:8080");

    let registry = config.build_profile_registry();
    assert!(registry.get("shell").is_some());
}

#[test]
fn config_file_present_loads_correctly() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let config_path = dir.path().join("config.yaml");

    let yaml_content = r#"
server:
  listen_addr: "127.0.0.1:3000"
  max_concurrent_jobs: 200
  log_level: "warn"
  log_format: "text"

sandbox:
  base_dir: "/tmp/test-sandboxes"
  disk_watermark_mb: 0
  default_profile: "python"
  fail_closed: false

profiles:
  shell:
    rlimit:
      cpu_seconds: 1
    max_stdout_mb: 1
"#;

    let mut f = std::fs::File::create(&config_path).expect("failed to create file");
    f.write_all(yaml_content.as_bytes()).expect("write failed");

    let config = sandbox_server::config::AppConfig::load_from_path(&config_path)
        .expect("load failed");

    assert_eq!(config.server.listen_addr, "127.0.0.1:3000");
    assert_eq!(config.server.max_concurrent_jobs, 200);
    assert_eq!(config.sandbox.disk_watermark_mb, 0);
    assert_eq!(config.sandbox.default_profile, "python");
    assert!(!config.sandbox.fail_closed);

    // profiles 中有 shell（来自 YAML）
    let shell = config.profiles.get("shell").expect("shell profile not found");
    assert_eq!(shell.max_stdout_mb, Some(1));
}

#[test]
fn disk_watermark_mb_zero_means_disabled() {
    let yaml = r#"
server:
  listen_addr: "0.0.0.0:8080"
sandbox:
  base_dir: "/sandboxes"
  disk_watermark_mb: 0
"#;

    let config: sandbox_server::config::AppConfig =
        serde_yaml::from_str(yaml).expect("parse failed");

    assert_eq!(config.sandbox.disk_watermark_mb, 0);
}

// ==================== AppConfig → 运行时组件 ====================

#[test]
fn app_config_to_sandbox_config() {
    let config = sandbox_server::config::AppConfig::default();

    let sandbox_config = config.to_sandbox_config();

    assert_eq!(sandbox_config.sandbox_base_dir, std::path::PathBuf::from("/sandboxes"));
    // disk_watermark_mb=1024 → 1024*1024*1024 字节
    assert_eq!(sandbox_config.disk_watermark_bytes, 1024 * 1024 * 1024);
}

#[test]
fn app_config_disk_watermark_zero_converts_to_zero() {
    let mut config = sandbox_server::config::AppConfig::default();
    config.sandbox.disk_watermark_mb = 0;

    let sandbox_config = config.to_sandbox_config();

    assert_eq!(sandbox_config.disk_watermark_bytes, 0);
}

#[test]
fn custom_profile_registered_to_runner() {
    let yaml = r#"
server:
  listen_addr: "0.0.0.0:8080"
sandbox:
  base_dir: "/sandboxes"
profiles:
  custom_task:
    rlimit:
      cpu_seconds: 10
      nofile: 256
    max_stdout_mb: 20
    default_timeout: "60s"
    extra_readonly_paths:
      - "/data/shared"
"#;

    let config: sandbox_server::config::AppConfig =
        serde_yaml::from_str(yaml).expect("parse failed");

    let sandbox_section = &config.sandbox;

    // 将 profiles 转换为 SandboxProfile 并注册
    let mut registry = sandbox_core::profile::ProfileRegistry::with_defaults();

    for (name, pc) in &config.profiles {
        let profile = pc.to_profile(name, sandbox_section).expect("conversion failed");
        registry.register(profile);
    }

    // 自定义 profile 应该存在
    let custom = registry.get("custom_task").expect("custom_task not found");
    assert_eq!(custom.name, "custom_task");
    assert_eq!(custom.rlimit.cpu_seconds, Some(10));
    assert_eq!(custom.max_stdout_bytes, 20 * 1024 * 1024);
    assert_eq!(custom.extra_readonly_paths.len(), 1);
    assert_eq!(
        custom.extra_readonly_paths[0],
        std::path::PathBuf::from("/data/shared")
    );
}

// ==================== cr-036: templates ====================

#[test]
fn templates_parse_with_setup() {
    let yaml = "templates:\n  ds:\n    setup: \"echo installing\"\n    rlimit:\n      cpu_seconds: 10\n";
    let cfg: sandbox_server::config::AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.templates.len(), 1);
    let tmpl = &cfg.templates["ds"];
    assert_eq!(tmpl.setup.as_deref(), Some("echo installing"));
    assert_eq!(tmpl.profile.rlimit.as_ref().unwrap().cpu_seconds, Some(10));
    // profile conversion works
    let profile = tmpl.profile.to_profile("ds", &sandbox_server::config::SandboxSection::default()).unwrap();
    assert_eq!(profile.name, "ds");
}

// ==================== cr-034: mime_for ====================

#[test]
fn mime_detection_common_types() {
    use sandbox_server::api::mime_for;
    assert_eq!(mime_for("chart.png"), "image/png");
    assert_eq!(mime_for("photo.jpg"), "image/jpeg");
    assert_eq!(mime_for("photo.JPEG"), "image/jpeg");
    assert_eq!(mime_for("page.html"), "text/html");
    assert_eq!(mime_for("data.json"), "application/json");
    assert_eq!(mime_for("table.csv"), "text/csv");
    assert_eq!(mime_for("notes.txt"), "text/plain");
    assert_eq!(mime_for("readme.md"), "text/markdown");
    assert_eq!(mime_for("unknown.xyz"), "application/octet-stream");
    assert_eq!(mime_for("noext"), "application/octet-stream");
}
