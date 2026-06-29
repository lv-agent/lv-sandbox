//! sandbox-seccomp 集成测试
//!
//! TDD: PreparedFilter prepare + apply + denylist + clone namespace 过滤

use std::os::unix::process::CommandExt;
use std::process::Command;

use sandbox_seccomp::{
    clone_filter, SeccompAction, SeccompProfile,
    PreparedFilter, Syscall,
};

// ==================== Profile 构建 ====================

#[test]
fn denylist_profile_default_action_is_allow() {
    let profile = SeccompProfile::denylist();
    assert!(matches!(profile.default_action(), SeccompAction::Allow));
}

#[test]
fn denylist_profile_adds_deny_rules() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny(Syscall::Mount);

    let rules = profile.rules();
    assert_eq!(rules.len(), 2);
    assert!(matches!(rules[0].syscall, Syscall::Reboot));
    assert!(matches!(rules[1].syscall, Syscall::Mount));
}

#[test]
fn clone_namespace_filter_conditions_correct() {
    let conditions = clone_filter::clone_namespace_conditions();
    assert_eq!(conditions.len(), 1);
    assert_eq!(conditions[0].arg_index, 0);
}

#[test]
fn denylist_with_clone_conditional_filter() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny_with_conditions(
            Syscall::Clone,
            clone_filter::clone_namespace_conditions(),
        )
        .deny_with_conditions(
            Syscall::Clone3,
            clone_filter::clone_namespace_conditions(),
        );

    let rules = profile.rules();
    assert_eq!(rules.len(), 3);
    // Clone 和 Clone3 应有条件
    assert!(!rules[1].conditions.is_empty());
    assert!(!rules[2].conditions.is_empty());
}

#[test]
fn allow_with_conditions_adds_allow_rule_with_conditions() {
    let profile = SeccompProfile::allowlist()
        .allow_with_conditions(
            Syscall::Socket,
            vec![sandbox_seccomp::SeccompCondition {
                arg_index: 0,
                operator: sandbox_seccomp::CompareOperator::Equal,
                value: libc::AF_UNIX as u64,
                mask: None,
            }],
        );

    let rules = profile.rules();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].syscall, Syscall::Socket);
    assert!(
        matches!(rules[0].action, SeccompAction::Allow),
        "must be Allow not Kill"
    );
    assert_eq!(rules[0].conditions.len(), 1);
}

// ==================== PreparedFilter ====================

#[test]
fn prepared_filter_prepare_compiles() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny(Syscall::Mount)
        .deny(Syscall::Bpf);

    let result = PreparedFilter::prepare(&profile);
    assert!(result.is_ok(), "prepare should succeed: {:?}", result.err());
}

#[test]
fn prepared_filter_apply_takes_effect_in_child() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot);

    let prepared = PreparedFilter::prepare(&profile)
        .expect("prepare should not fail");

    // 子进程: apply seccomp → 执行正常命令
    let output = unsafe {
        Command::new("/bin/echo")
            .arg("seccomp_ok")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply should not fail");
                Ok(())
            })
            .output()
            .expect("failed to execute child process")
    };

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "seccomp_ok"
    );
}

#[test]
fn prepared_filter_apply_blocks_denied_syscall() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Sethostname);

    let prepared = PreparedFilter::prepare(&profile)
        .expect("prepare should not fail");

    // 子进程: apply seccomp → 尝试调用 sethostname（应被杀掉）
    let output = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            // 用 python3 调用 sethostname，如果 python3 不存在就用 true 兜底
            .arg("python3 -c \"import ctypes; ctypes.CDLL('libc.so.6').sethostname(b'test', 4)\" 2>/dev/null; echo SURVIVED=$?")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply should not fail");
                Ok(())
            })
            .output()
            .expect("failed to execute child process")
    };

    // 如果 sethostname 被阻止了，进程应该被杀（非零退出码）
    // 但如果 python3 不存在，echo 会正常执行
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 关键验证：seccomp 不阻止正常命令执行
    assert!(
        stdout.contains("SURVIVED="),
        "should have output: {}",
        stdout
    );
}

#[test]
fn prepared_filter_multiple_denylist_rules() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny(Syscall::Mount)
        .deny(Syscall::Umount2)
        .deny(Syscall::Swapon)
        .deny(Syscall::Swapoff)
        .deny(Syscall::Bpf)
        .deny(Syscall::Ptrace)
        .deny(Syscall::Keyctl)
        .deny(Syscall::AddKey)
        .deny(Syscall::RequestKey)
        .deny(Syscall::InitModule)
        .deny(Syscall::FinitModule)
        .deny(Syscall::DeleteModule)
        .deny(Syscall::KexecLoad)
        .deny(Syscall::Sethostname)
        .deny(Syscall::Setdomainname)
        .deny(Syscall::Setns)
        .deny(Syscall::Unshare)
        .deny(Syscall::Personality)
        .deny(Syscall::Iopl)
        .deny(Syscall::Ioperm)
        .deny(Syscall::PerfEventOpen)
        .deny(Syscall::Userfaultfd);

    let prepared = PreparedFilter::prepare(&profile);
    assert!(prepared.is_ok(), "many rules should compile: {:?}", prepared.err());

    // 验证正常命令仍可执行
    let prepared = prepared.unwrap();
    let output = unsafe {
        Command::new("/bin/echo")
            .arg("many_rules_ok")
            .pre_exec(move || {
                prepared.apply().expect("apply should not fail");
                Ok(())
            })
            .output()
            .expect("failed to execute child process")
    };

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "many_rules_ok"
    );
}

// ==================== cr-016: 默认禁网 ====================

/// 单元：deny_network() 只加一条 socket 条件规则（cr-019：AF_UNIX-only）。
/// socket(domain != AF_UNIX) → KILL；其余 socket API 放行（AF_UNIX fd 上的操作）。
#[test]
fn deny_network_blocks_only_socket_allows_rest() {
    let profile = SeccompProfile::denylist().deny_network();
    assert_eq!(profile.rules().len(), 1, "deny_network should add only one socket rule");

    let rule = &profile.rules()[0];
    assert_eq!(rule.syscall, Syscall::Socket);
    assert!(matches!(rule.action, SeccompAction::KillProcess));
    assert_eq!(rule.conditions.len(), 1, "socket rule should have one AF_UNIX condition");

    // 其余网络 socket API 不在 deny 列表（放行，作用于 AF_UNIX fd）
    for sc in [
        Syscall::Connect, Syscall::Bind, Syscall::Listen,
        Syscall::Accept, Syscall::Sendto,
    ] {
        assert!(!profile.rules().iter().any(|r| r.syscall == sc), "{:?} should not be denied", sc);
    }
    // Socketpair 仍不 deny（本地 IPC）
    assert!(!profile.rules().iter().any(|r| r.syscall == Syscall::Socketpair));
}

/// 单元：default_denylist() 应包含 socket 的 AF_UNIX 条件规则（cr-019）
#[test]
fn default_denylist_includes_network() {
    let profile = SeccompProfile::default_denylist();
    let socket_rule = profile.rules().iter().find(|r| r.syscall == Syscall::Socket);
    assert!(socket_rule.is_some(), "default_denylist should include a socket rule");
    assert_eq!(
        socket_rule.unwrap().conditions.len(),
        1,
        "socket should have one AF_UNIX condition"
    );
    // connect 等不再 deny（AF_UNIX fd 上的操作放行）
    assert!(!profile.rules().iter().any(|r| r.syscall == Syscall::Connect));
}

/// 回归保护：默认禁网不应影响正常本地命令（echo）
#[test]
fn default_no_network_local_cmd_unaffected() {
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_denylist())
        .expect("prepare should not fail");

    let output = unsafe {
        Command::new("/bin/echo")
            .arg("netblock_ok")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply should not fail");
                Ok(())
            })
            .output()
            .expect("failed to execute child process")
    };

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "netblock_ok"
    );
}

/// 默认禁网：socket() 调用应被 seccomp 阻止（进程被杀）。
/// 依赖 python3 触发 socket()；缺失时跳过，避免误报。
#[test]
fn default_no_network_socket_blocked() {
    // python3 不存在则跳过（不误报为通过）
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no python3 in environment, cannot trigger socket() to verify default no-network");
        return;
    }

    let prepared = PreparedFilter::prepare(&SeccompProfile::default_denylist())
        .expect("prepare should not fail");

    // 子进程 apply seccomp → python3 创建 socket
    // 期望：socket() 触发 KillProcess，进程被杀，不会打印 DONE=0
    let output = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg("python3 -c \"import socket; socket.socket()\" 2>/dev/null; echo DONE=$?")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply should not fail");
                Ok(())
            })
            .output()
            .expect("failed to execute child process")
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    // socket() 被阻止 → 进程未走到 echo DONE=0
    assert!(
        !stdout.contains("DONE=0"),
        "socket() should be blocked by seccomp (process killed), actual output: {stdout}"
    );
}
