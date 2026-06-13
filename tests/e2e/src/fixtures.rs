//! 测试 fixture：YAML 配置模板等

/// 自定义 profile 的 YAML 配置 fixture
pub fn custom_profile_yaml() -> &'static str {
    r#"
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
    max_stderr_mb: 20
    default_timeout: "30s"
    extra_readonly_paths:
      - "/data/shared"
"#
}

/// 覆盖内置 shell profile 的 YAML fixture
pub fn override_shell_yaml() -> &'static str {
    r#"
server:
  listen_addr: "0.0.0.0:8080"

sandbox:
  base_dir: "/sandboxes"

profiles:
  shell:
    rlimit:
      cpu_seconds: 5
      nofile: 32
    max_stdout_mb: 1
    default_timeout: "10s"
"#
}

/// 带 custom timeout 的 profile YAML fixture
pub fn custom_timeout_yaml() -> &'static str {
    r#"
server:
  listen_addr: "0.0.0.0:8080"

sandbox:
  base_dir: "/sandboxes"

profiles:
  long_task:
    rlimit:
      cpu_seconds: 60
    max_stdout_mb: 50
    default_timeout: "120s"
"#
}
