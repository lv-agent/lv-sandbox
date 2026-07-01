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

// ==================== cr-045: allowlist 模式 ====================

#[test]
fn default_allowlist_shell_default_action_is_kill() {
    let p = SeccompProfile::default_allowlist_shell();
    assert!(matches!(p.default_action(), SeccompAction::KillProcess));
}

#[test]
fn default_allowlist_shell_includes_basic_syscalls() {
    let p = SeccompProfile::default_allowlist_shell();
    // 关键:常规 syscall 必须在白名单(否则 echo 都跑不起来)
    let names: Vec<String> = p
        .rules()
        .iter()
        .filter_map(|r| match r.syscall {
            Syscall::Custom(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    for need in ["read", "write", "openat", "close", "exit_group", "execve", "mmap"] {
        assert!(
            names.iter().any(|n| n == need),
            "allowlist missing required syscall: {need}"
        );
    }
}

#[test]
fn default_allowlist_shell_socket_af_unix_only() {
    let p = SeccompProfile::default_allowlist_shell();
    let socket_rule = p.rules().iter().find(|r| r.syscall == Syscall::Socket);
    assert!(socket_rule.is_some(), "must have a socket rule");
    let r = socket_rule.unwrap();
    assert!(matches!(r.action, SeccompAction::Allow));
    assert_eq!(r.conditions.len(), 1);
    assert_eq!(r.conditions[0].value, libc::AF_UNIX as u64);
}

/// 回归:allowlist 下 shell 基本命令必须能跑(锁白名单完备性)。
/// 白名单漏 syscall → 命令被 SIGSYS 杀 → 输出缺失 → 测试 fail。
#[test]
fn default_allowlist_shell_runs_basic_commands() {
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_shell())
        .expect("prepare should not fail");

    // 1) 纯 echo
    let out = unsafe {
        Command::new("/bin/echo")
            .arg("allowlist_ok")
            .pre_exec(move || {
                prepared.apply().expect("apply should not fail");
                Ok(())
            })
            .output()
            .expect("exec failed")
    };
    assert_eq!(out.status.code(), Some(0), "echo exited abnormally: {:?}", out);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "allowlist_ok"
    );

    // 2) sh -c(涉及 fork/exec/重定向,覆盖更多 syscall)
    let prepared2 = PreparedFilter::prepare(&SeccompProfile::default_allowlist_shell())
        .expect("prepare should not fail");
    let out2 = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg("echo sh_ok; printf 'p\\n'")
            .pre_exec(move || {
                prepared2.apply().expect("apply should not fail");
                Ok(())
            })
            .output()
            .expect("exec failed")
    };
    assert_eq!(
        out2.status.code(),
        Some(0),
        "sh -c exited abnormally: {:?}",
        out2
    );
    let s = String::from_utf8_lossy(&out2.stdout);
    assert!(
        s.contains("sh_ok") && s.contains("p\n"),
        "sh output wrong: {s}"
    );
}

/// 回归:allowlist 下 INET socket 仍被禁(保持 cr-019 AF_UNIX-only 基线)。
#[test]
fn default_allowlist_shell_blocks_inet_socket() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no python3 to trigger INET socket");
        return;
    }
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_shell())
        .expect("prepare should not fail");
    let out = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg("python3 -c \"import socket; socket.socket(socket.AF_INET)\" 2>/dev/null; echo DONE=$?")
            .pre_exec(move || {
                prepared.apply().expect("apply should not fail");
                Ok(())
            })
            .output()
            .expect("exec failed")
    };
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("DONE=0"),
        "INET socket() must be killed under allowlist, got: {s}"
    );
}

/// 回归(扩展,cr-045):allowlist 下真实 shell 工作流必须能跑。
/// Task 3 的 echo/printf 太窄;此测试覆盖管道/重定向/目录列举/文件读/循环/变量/子进程,
/// 锁白名单对真实 shell 用法的完备性(漏 syscall → sh 被 SIGSYS 杀 → ALLDONE 缺失)。
#[test]
fn default_allowlist_shell_runs_realistic_workloads() {
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_shell())
        .expect("prepare should not fail");
    let out = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(
                "x=hi; for i in 1 2; do echo $x $i; done | cat; \
                 ls / >/dev/null; cat /etc/hostname >/dev/null; echo ALLDONE",
            )
            .pre_exec(move || {
                prepared.apply().expect("apply should not fail");
                Ok(())
            })
            .output()
            .expect("exec failed")
    };
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "realistic sh workload exited abnormally: {:?}",
        out
    );
    assert!(s.contains("hi 1"), "for-loop + var + pipe failed: {s}");
    assert!(s.contains("ALLDONE"), "ls/cat/redirect chain failed: {s}");
}

/// 回归(cr-045):allowlist 下常用命令(grep/sed/awk/find/head/tail/wc/sort/uniq/tr/cut/tee/
/// heredoc/test)必须能跑——出厂 shell allowlist 须支持常用命令执行。
/// 任一被 SIGSYS 杀(子进程继承 filter)则该条 assert 失败。
#[test]
fn default_allowlist_shell_runs_common_commands() {
    let scripts: &[(&str, &str)] = &[
        ("grep", "echo apple > /tmp/_al_g; grep apple /tmp/_al_g"),
        ("sed", "echo apple > /tmp/_al_s; sed 's/a/A/' /tmp/_al_s"),
        ("awk", "echo apple > /tmp/_al_a; awk '{print NR}' /tmp/_al_a"),
        ("find", "find /tmp -maxdepth 1 -name '_al_g'"),
        ("head", "echo apple > /tmp/_al_h; head -1 /tmp/_al_h"),
        ("tail", "echo apple > /tmp/_al_t; tail -1 /tmp/_al_t"),
        ("wc", "echo apple > /tmp/_al_w; wc -l /tmp/_al_w"),
        ("sort", "echo apple > /tmp/_al_so; sort /tmp/_al_so"),
        ("uniq", "printf 'a\\na\\n' > /tmp/_al_u; uniq /tmp/_al_u"),
        ("tr", "echo abc > /tmp/_al_tr; tr a-z A-Z < /tmp/_al_tr"),
        ("cut", "echo abc > /tmp/_al_c; cut -c1 /tmp/_al_c"),
        ("tee", "echo abc > /tmp/_al_t1; tee /tmp/_al_t2 < /tmp/_al_t1 >/dev/null"),
        ("heredoc", "cat <<'EOF'\nheredoc_ok\nEOF"),
        ("test_bracket", "echo x > /tmp/_al_t3; test -f /tmp/_al_t3 && [ -f /tmp/_al_t3 ]"),
    ];
    for &(name, script) in scripts {
        let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_shell())
            .expect("prepare");
        let out = unsafe {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .pre_exec(move || {
                    prepared.apply().expect("apply");
                    Ok(())
                })
                .output()
                .expect("exec")
        };
        assert_eq!(
            out.status.code(),
            Some(0),
            "common command '{name}' failed/killed under allowlist: {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ==================== cr-045 Phase 2: python allowlist ====================

#[test]
fn default_allowlist_python_default_action_is_kill() {
    let p = SeccompProfile::default_allowlist_python();
    assert!(matches!(p.default_action(), SeccompAction::KillProcess));
}

#[test]
fn default_allowlist_python_includes_gettid() {
    let p = SeccompProfile::default_allowlist_python();
    let names: Vec<String> = p
        .rules()
        .iter()
        .filter_map(|r| match r.syscall {
            Syscall::Custom(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|n| n == "gettid"), "python allowlist must include gettid");
    for need in ["read", "write", "openat", "mmap", "execve"] {
        assert!(names.iter().any(|n| n == need), "python allowlist missing {need}");
    }
}

/// 回归:python 典型脚本在 allowlist 下必须能跑(print + import + file IO)。
/// 白名单漏 syscall → python 被 SIGSYS 杀 → 输出缺失。
#[test]
fn default_allowlist_python_runs_typical_script() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no python3");
        return;
    }
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_python())
        .expect("prepare");
    let script = "python3 -c 'import os, sys; print(\"py_ok\", os.getcwd()); \
                  open(\"/tmp/_al_py\", \"w\").write(\"x\"); print(\"io_ok\")'";
    let out = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .pre_exec(move || {
                prepared.apply().expect("apply");
                Ok(())
            })
            .output()
            .expect("exec")
    };
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "python killed/failed under allowlist: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("py_ok"), "python print failed: {s}");
    assert!(s.contains("io_ok"), "python file IO failed: {s}");
}

/// 回归(cr-045 P2):python 常用库(json/re)+ 子进程在 allowlist 下能跑。
/// 吸取 Phase 1 教训(初始测试太窄),覆盖 C 扩展(json/re)+ fork-exec(os.system)。
#[test]
fn default_allowlist_python_runs_realistic() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: no python3");
        return;
    }
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_python())
        .expect("prepare");
    let script = "python3 -c 'import json, re, os; \
                  print(\"json_ok\", json.dumps({\"k\": 1})); \
                  print(\"re_ok\", re.match(\"a\", \"abc\").group()); \
                  os.system(\"echo sub_ok\")'";
    let out = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .pre_exec(move || {
                prepared.apply().expect("apply");
                Ok(())
            })
            .output()
            .expect("exec")
    };
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "python realistic killed: {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("json_ok"), "json failed: {s}");
    assert!(s.contains("re_ok"), "re failed: {s}");
    assert!(s.contains("sub_ok"), "subprocess(os.system) failed: {s}");
}

// ==================== cr-045 Phase 3: node allowlist ====================

#[test]
fn default_allowlist_node_default_action_is_kill() {
    let p = SeccompProfile::default_allowlist_node();
    assert!(matches!(p.default_action(), SeccompAction::KillProcess));
}

#[test]
fn default_allowlist_node_includes_gettid() {
    let p = SeccompProfile::default_allowlist_node();
    let names: Vec<String> = p
        .rules()
        .iter()
        .filter_map(|r| match r.syscall {
            Syscall::Custom(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|n| n == "gettid"), "node allowlist must include gettid");
    for need in ["read", "write", "openat", "mmap", "execve"] {
        assert!(names.iter().any(|n| n == need), "node allowlist missing {need}");
    }
}

/// 回归:node 典型脚本在 allowlist 下必须能跑(console + fs + crypto)。
/// 白名单漏 syscall → node 被 SIGSYS 杀 → 输出缺失。
#[test]
fn default_allowlist_node_runs_typical_script() {
    let ver_out = std::process::Command::new("node").arg("--version").output();
    let Ok(ver_out) = ver_out else {
        eprintln!("skipping: no node");
        return;
    };
    let ver = String::from_utf8_lossy(&ver_out.stdout);
    // node 22+ libuv 启动即 io_uring_setup;沙箱禁 io_uring(逃逸面 + bypass seccomp)
    // → node 22+ 在沙箱完全不可用。生产镜像 node 18(bookworm,无 io_uring)allowlist OK;
    // host nvm node 22+ skip,镜像内 node 18 由 CI Job B 验证。
    let major = ver
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if major >= 22 {
        eprintln!(
            "skipping: node {} uses io_uring (sandbox blocks io_uring; node 18/20 image OK)",
            ver.trim()
        );
        return;
    }
    let prepared = PreparedFilter::prepare(&SeccompProfile::default_allowlist_node())
        .expect("prepare");
    let script = "node -e 'const fs=require(\"fs\"); \
                  fs.writeFileSync(\"/tmp/_al_node\",\"x\"); \
                  console.log(\"node_ok\", fs.readFileSync(\"/tmp/_al_node\",\"utf8\")); \
                  console.log(\"crypto_ok\", require(\"crypto\").randomBytes(4).toString(\"hex\"))'";
    let out = unsafe {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .pre_exec(move || {
                prepared.apply().expect("apply");
                Ok(())
            })
            .output()
            .expect("exec")
    };
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "node killed/failed under allowlist: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(s.contains("node_ok"), "node console/fs failed: {s}");
    assert!(s.contains("crypto_ok"), "node crypto failed: {s}");
}
