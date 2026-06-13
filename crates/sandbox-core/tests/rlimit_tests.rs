//! rlimit 模块集成测试：RlimitConfig
//!
//! 测试 rlimit 配置构建器和 apply() 实际生效。
//! rlimit.apply() 在子进程中测试（因为会影响整个进程）。

use sandbox_core::rlimit::RlimitConfig;
use std::os::unix::process::CommandExt;
use std::process::Command;

#[test]
fn builder链式调用生成正确的配置() {
    let config = RlimitConfig::new()
        .cpu_seconds(2)
        .nofile(64)
        .nproc(16)
        .fsize_mb(10)
        .core_disabled()
        .stack_mb(8)
        .memlock_disabled();

    assert_eq!(config.cpu_seconds, Some(2));
    assert_eq!(config.nofile, Some(64));
    assert_eq!(config.nproc, Some(16));
    assert_eq!(config.fsize_bytes, Some(10 * 1024 * 1024));
    assert_eq!(config.core, Some(0));
    assert_eq!(config.stack_bytes, Some(8 * 1024 * 1024));
    assert_eq!(config.memlock, Some(0));
    assert_eq!(config.address_space_bytes, None);
}

#[test]
fn 预设shell_profile值正确() {
    let config = sandbox_core::rlimit::shell_default();

    assert_eq!(config.cpu_seconds, Some(2));
    assert_eq!(config.nofile, Some(64));
    assert_eq!(config.nproc, Some(32));
    assert_eq!(config.fsize_bytes, Some(10 * 1024 * 1024));
    assert_eq!(config.core, Some(0));
    assert_eq!(config.memlock, Some(0));
}

#[test]
fn 预设python_profile值正确() {
    let config = sandbox_core::rlimit::python_default();

    assert_eq!(config.cpu_seconds, Some(2));
    assert_eq!(config.nofile, Some(64));
    // Python/Node 默认不启用 RLIMIT_AS
    assert_eq!(config.address_space_bytes, None);
}

#[test]
fn apply_在子进程中生效_nofile限制() {
    let config = RlimitConfig::new().nofile(16);
    let config_clone = config.clone();

    let output = unsafe {
        Command::new("sh")
            .arg("-c")
            .arg("echo NOFILE=$(ulimit -n)")
            .pre_exec(move || {
                config_clone.apply().expect("rlimit apply 失败");
                Ok(())
            })
            .output()
            .expect("执行失败")
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NOFILE=16"),
        "期望 NOFILE=16，实际输出: {stdout}"
    );
}

#[test]
fn apply_在子进程中生效_fsize限制() {
    let config = RlimitConfig::new().fsize_mb(1);
    let config_clone = config.clone();

    let output = unsafe {
        Command::new("sh")
            .arg("-c")
            .arg("dd if=/dev/zero of=/tmp/rlimit_test_bigfile bs=1M count=2 2>&1; echo EXIT=$?")
            .env("TMPDIR", "/tmp")
            .pre_exec(move || {
                config_clone.apply().expect("rlimit apply 失败");
                Ok(())
            })
            .output()
            .expect("执行失败")
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXIT="),
        "应有 EXIT 输出: {stdout}"
    );
}

#[test]
fn 空配置apply不报错() {
    let config = RlimitConfig::new();
    let config_clone = config.clone();

    let output = unsafe {
        Command::new("sh")
            .arg("-c")
            .arg("echo OK")
            .pre_exec(move || {
                config_clone.apply().expect("空配置 apply 不应报错");
                Ok(())
            })
            .output()
            .expect("执行失败")
    };

    assert_eq!(output.status.code(), Some(0));
}
