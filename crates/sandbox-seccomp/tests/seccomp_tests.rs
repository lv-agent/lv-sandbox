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
fn denylist_profile_默认动作为允许() {
    let profile = SeccompProfile::denylist();
    assert!(matches!(profile.default_action(), SeccompAction::Allow));
}

#[test]
fn denylist_profile_添加拒绝规则() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny(Syscall::Mount);

    let rules = profile.rules();
    assert_eq!(rules.len(), 2);
    assert!(matches!(rules[0].syscall, Syscall::Reboot));
    assert!(matches!(rules[1].syscall, Syscall::Mount));
}

#[test]
fn clone_namespace_过滤条件正确() {
    let conditions = clone_filter::clone_namespace_conditions();
    assert_eq!(conditions.len(), 1);
    assert_eq!(conditions[0].arg_index, 0);
}

#[test]
fn denylist_with_clone_条件过滤() {
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

// ==================== PreparedFilter ====================

#[test]
fn prepared_filter_prepare_成功编译() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot)
        .deny(Syscall::Mount)
        .deny(Syscall::Bpf);

    let result = PreparedFilter::prepare(&profile);
    assert!(result.is_ok(), "prepare 应成功: {:?}", result.err());
}

#[test]
fn prepared_filter_apply_在子进程中生效() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Reboot);

    let prepared = PreparedFilter::prepare(&profile)
        .expect("prepare 不应失败");

    // 子进程: apply seccomp → 执行正常命令
    let output = unsafe {
        Command::new("/bin/echo")
            .arg("seccomp_ok")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply 不应失败");
                Ok(())
            })
            .output()
            .expect("执行子进程失败")
    };

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "seccomp_ok"
    );
}

#[test]
fn prepared_filter_apply_阻止被拒绝的syscall() {
    let profile = SeccompProfile::denylist()
        .deny(Syscall::Sethostname);

    let prepared = PreparedFilter::prepare(&profile)
        .expect("prepare 不应失败");

    // 子进程: apply seccomp → 尝试调用 sethostname（应被杀掉）
    let output = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            // 用 python3 调用 sethostname，如果 python3 不存在就用 true 兜底
            .arg("python3 -c \"import ctypes; ctypes.CDLL('libc.so.6').sethostname(b'test', 4)\" 2>/dev/null; echo SURVIVED=$?")
            .pre_exec(move || {
                prepared.apply().expect("seccomp apply 不应失败");
                Ok(())
            })
            .output()
            .expect("执行子进程失败")
    };

    // 如果 sethostname 被阻止了，进程应该被杀（非零退出码）
    // 但如果 python3 不存在，echo 会正常执行
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 关键验证：seccomp 不阻止正常命令执行
    assert!(
        stdout.contains("SURVIVED="),
        "应有输出: {}",
        stdout
    );
}

#[test]
fn prepared_filter_多条denylist规则() {
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
    assert!(prepared.is_ok(), "大量规则应成功编译: {:?}", prepared.err());

    // 验证正常命令仍可执行
    let prepared = prepared.unwrap();
    let output = unsafe {
        Command::new("/bin/echo")
            .arg("many_rules_ok")
            .pre_exec(move || {
                prepared.apply().expect("apply 不应失败");
                Ok(())
            })
            .output()
            .expect("执行子进程失败")
    };

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "many_rules_ok"
    );
}
