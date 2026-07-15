//! cr-026 SessionManager 集成测试。
use sandbox_core::job::{JobRequest, JobStatus};
use sandbox_core::profile::SandboxProfile;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};
use sandbox_server::audit::AuditLogger;
use sandbox_server::session::{SessionManager, VolumeMount};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn mgr() -> (tempfile::TempDir, SessionManager) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    (
        tmp,
        SessionManager::new(runner, Arc::new(AuditLogger::noop())),
    )
}

fn req(argv: Vec<String>) -> JobRequest {
    JobRequest {
        job_id: "s".to_string(),
        argv,
        profile_name: "shell".to_string(), // 会话 exec 用绑定 profile,此项忽略
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
        cwd: None,    }
}

#[tokio::test]
async fn session_exec_shares_workspace_across_calls() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // exec A 写文件
    let r1 = m
        .exec_session(
            &id,
            req(vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo hello > out.txt".into(),
            ]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(r1.status, JobStatus::Completed));
    // exec B 读同一文件(证明工作区跨 exec 持久)
    let r2 = m
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "out.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r2.stdout).contains("hello"),
        "shared workspace should retain out.txt: {:?}",
        r2.stdout
    );
}

#[tokio::test]
async fn session_lifecycle_create_list_get_destroy() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    assert!(m.get_session(&id).is_some());
    assert!(m.list_sessions().iter().any(|s| s.session_id == id));
    m.destroy_session(&id).unwrap();
    assert!(m.get_session(&id).is_none());
    assert!(!m.list_sessions().iter().any(|s| s.session_id == id));
}

/// cr-085 M2: get_session 返回富信息——started_at 绝对时间、state、cpu/memory(从 profile 折算)、template_id。
#[tokio::test]
async fn get_session_info_returns_rich_fields() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let info = m.get_session(&id).expect("session exists");
    assert_eq!(info.template_id.as_deref(), Some("shell"), "template_id = profile");
    assert_eq!(info.state, "RUNNING");
    // started_at 是绝对 unix 秒(非 elapsed),接近当前
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let started = info.started_at.expect("started_at present");
    assert!(
        started <= now && started > now.saturating_sub(60),
        "started_at near now: {started}"
    );
    // cpu_count/memory_size 从 profile cgroup 折算(shell: 200ms/1s→1 核,128MB)
    assert!(info.cpu_count >= 1, "cpu_count");
    assert!(info.memory_size > 0, "memory_size");
    // M2 阶段 alias/metadata/timeout 未填(M3 才接 create 参数)
    assert_eq!(info.alias, None);
    assert!(info.metadata.is_empty());
    assert_eq!(info.timeout_secs, None);
}

/// cr-085 M3: create_session_with_opts 持久化 alias/metadata/timeout,get_info 可回读。
#[tokio::test]
async fn create_session_with_opts_persists_alias_metadata_timeout() {
    let (_tmp, m) = mgr().await;
    let mut md = HashMap::new();
    md.insert("owner".to_string(), "team-a".to_string());
    let id = m
        .create_session_with_opts(
            "shell",
            HashMap::new(),
            None,
            vec![],
            Some("my-alias".to_string()),
            md,
            Some(300),
        )
        .unwrap();
    let info = m.get_session(&id).unwrap();
    assert_eq!(info.alias.as_deref(), Some("my-alias"));
    assert_eq!(info.timeout_secs, Some(300));
    assert_eq!(
        info.metadata.get("owner").map(|s| s.as_str()),
        Some("team-a")
    );
}

/// cr-085 M3: exec cwd——相对 workspace 的子目录,chdir 到那执行(cat 相对路径能读到)。
/// 若 cwd 未生效(chdir 仍为 workspace 根),cat f.txt 找不到 → 测试失败。
#[tokio::test]
async fn exec_with_custom_cwd_runs_there() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // 建子目录(直接 mkdir,不经 sh -c 多命令——避免无关 fork 噪音)
    let r0 = m
        .exec_session(
            &id,
            req(vec!["/bin/mkdir".into(), "-p".into(), "sub".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(r0.status, JobStatus::Completed),
        "mkdir sub: {:?} {:?}",
        r0.status,
        String::from_utf8_lossy(&r0.stderr)
    );
    // cwd=sub + pwd → 验证 chdir 到 workspace/sub
    let mut r = req(vec!["/bin/pwd".into()]);
    r.cwd = Some("sub".to_string());
    let res = m
        .exec_session(&id, r, CancellationToken::new(), None)
        .await
        .unwrap();
    assert!(
        matches!(res.status, JobStatus::Completed),
        "pwd in cwd: {:?} {:?}",
        res.status,
        String::from_utf8_lossy(&res.stderr)
    );
    let pwd = String::from_utf8_lossy(&res.stdout);
    assert!(
        pwd.trim().ends_with("/sub"),
        "pwd should be workspace/sub: {pwd:?}"
    );
}

/// cr-085 M3: cwd 含 `..` 被拒(sanitize_relpath 圈定,防越狱)。
#[tokio::test]
async fn exec_cwd_rejects_parent_dir() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let mut r = req(vec!["/bin/true".into()]);
    r.cwd = Some("../etc".to_string());
    let res = m
        .exec_session(&id, r, CancellationToken::new(), None)
        .await;
    assert!(res.is_err(), "cwd with .. must be rejected by sanitize");
}

/// cr-085 M4: update_session 替换 alias/timeout/metadata,get_info 回读新值。
#[tokio::test]
async fn update_session_replaces_alias_timeout_metadata() {
    let (_tmp, m) = mgr().await;
    let id = m
        .create_session_with_opts(
            "shell",
            HashMap::new(),
            None,
            vec![],
            Some("old".into()),
            HashMap::new(),
            Some(60),
        )
        .unwrap();
    let mut md = HashMap::new();
    md.insert("k".to_string(), "v".to_string());
    m.update_session(&id, Some("new".into()), Some(300), md)
        .unwrap();
    let info = m.get_session(&id).unwrap();
    assert_eq!(info.alias.as_deref(), Some("new"));
    assert_eq!(info.timeout_secs, Some(300));
    assert_eq!(
        info.metadata.get("k").map(|s| s.as_str()),
        Some("v")
    );
}

/// cr-085 M4: update_session 写入持久化,跨重启 rebuild 保留。
#[tokio::test]
async fn update_session_persists_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1
        .create_session_with_opts("shell", HashMap::new(), None, vec![], None, HashMap::new(), None)
        .unwrap();
    let mut md = HashMap::new();
    md.insert("team".to_string(), "a".to_string());
    sm1.update_session(&id, Some("alias1".into()), Some(120), md)
        .unwrap();
    drop(sm1);
    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    sm2.rebuild_from_disk().unwrap();
    let info = sm2.get_session(&id).unwrap();
    assert_eq!(info.alias.as_deref(), Some("alias1"));
    assert_eq!(info.timeout_secs, Some(120));
    assert_eq!(info.metadata.get("team").map(|s| s.as_str()), Some("a"));
}

/// cr-085 M5: make_dir 建 nested 目录,file_exists 检查文件/目录存在。
#[tokio::test]
async fn make_dir_creates_nested_and_exists_checks() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    m.make_dir(&id, "a/b/c").unwrap();
    assert!(m.file_exists(&id, "a/b/c"), "nested dir exists");
    assert!(m.file_exists(&id, "a"), "parent exists");
    assert!(
        !m.file_exists(&id, "a/b/c/missing"),
        "missing not exists"
    );
    m.put_file(&id, "a/f.txt", b"x").unwrap();
    assert!(m.file_exists(&id, "a/f.txt"), "file exists");
}

/// cr-085 M5: make_dir 拒 `..`(sanitize 圈定)。
#[tokio::test]
async fn make_dir_rejects_parent_dir() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    assert!(m.make_dir(&id, "../etc").is_err(), "mkdir with .. rejected");
}

// ==================== cr-085 M6: find / search ====================

use sandbox_server::session::SearchOpts;

/// cr-085 M6: find 用 glob(`**/*.py`)递归匹配嵌套文件;txt 排除。
#[tokio::test]
async fn find_glob_matches_nested_files() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    m.put_file(&id, "a.py", b"x").unwrap();
    m.put_file(&id, "pkg/b.py", b"y").unwrap();
    m.put_file(&id, "pkg/sub/c.txt", b"z").unwrap();
    let res = m.find_session_files(&id, "", "**/*.py", 100).unwrap();
    let paths: Vec<&str> = res.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"a.py"), "root py: {paths:?}");
    assert!(paths.contains(&"pkg/b.py"), "nested py: {paths:?}");
    assert!(
        !paths.iter().any(|p| p.ends_with(".txt")),
        "txt excluded: {paths:?}"
    );
    assert!(!res.truncated, "under limit, not truncated");
}

/// cr-085 M6: find 拒越界路径(sanitize_relpath 圈定,防越狱)。
#[tokio::test]
async fn find_respects_path_sandbox() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    assert!(
        m.find_session_files(&id, "../etc", "*", 100).is_err(),
        "find with .. must be rejected"
    );
}

/// cr-085 M6: find 截断到 limit 并标 truncated。
#[tokio::test]
async fn find_truncates_at_limit() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    for i in 0..10 {
        m.put_file(&id, &format!("f{i}.txt"), b"x").unwrap();
    }
    let res = m.find_session_files(&id, "", "f*.txt", 3).unwrap();
    assert_eq!(res.files.len(), 3, "capped at limit");
    assert!(res.truncated, "over limit → truncated flag");
    // 不截断时返回全部
    let res2 = m.find_session_files(&id, "", "f*.txt", 100).unwrap();
    assert_eq!(res2.files.len(), 10);
    assert!(!res2.truncated);
}

/// cr-085 M6: search 正则匹配内容,返回 1-based 行号 + 命中文本。
#[tokio::test]
async fn search_regex_finds_content() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    m.put_file(
        &id,
        "code.py",
        b"def foo():\n    return 'TODO fix'\n    pass\n",
    )
    .unwrap();
    let res = m
        .search_session_files(&id, "", "TODO", &SearchOpts::default())
        .unwrap();
    assert_eq!(res.results.len(), 1, "one file matched");
    let r = &res.results[0];
    assert_eq!(r.path, "code.py");
    assert_eq!(r.matches.len(), 1);
    assert_eq!(r.matches[0].line, 2, "1-based line number");
    assert!(r.matches[0].text.contains("TODO fix"));
    assert!(!res.truncated);
}

/// cr-085 M6: search 跳过二进制文件(含 NUL 字节)。
#[tokio::test]
async fn search_skips_binary_files() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let bin: Vec<u8> = vec![b'T', b'O', b'D', b'O', 0u8, b'x']; // NUL → binary
    m.put_file(&id, "blob.bin", &bin).unwrap();
    m.put_file(&id, "txt.txt", b"TODO in text\n").unwrap();
    let res = m
        .search_session_files(&id, "", "TODO", &SearchOpts::default())
        .unwrap();
    let paths: Vec<&str> = res.results.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"txt.txt"), "text matched");
    assert!(
        !paths.contains(&"blob.bin"),
        "binary should be skipped: {paths:?}"
    );
}

/// cr-085 M6: search 跳过超过 max_file_bytes 的单文件(性能护栏)。
#[tokio::test]
async fn search_enforces_size_limit() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // 大文件(远超 100B 上限)+ 含 TODO;小文件含 TODO
    let big = format!("{}\n", "TODO big".repeat(10000));
    m.put_file(&id, "big.txt", big.as_bytes()).unwrap();
    m.put_file(&id, "small.txt", b"TODO small\n").unwrap();
    let opts = SearchOpts {
        max_file_bytes: 100,
        ..Default::default()
    };
    let res = m.search_session_files(&id, "", "TODO", &opts).unwrap();
    let paths: Vec<&str> = res.results.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"small.txt"), "small matched");
    assert!(
        !paths.contains(&"big.txt"),
        "big file skipped by size limit: {paths:?}"
    );
}

#[tokio::test]
async fn create_session_unknown_profile_errors() {
    let (_tmp, m) = mgr().await;
    assert!(m.create_session("nope", HashMap::new(), None, vec![]).is_err());
}

#[tokio::test]
async fn snapshot_then_restore_forks_session() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    // exec 写文件
    m.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo forked > f.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    // 快照
    let snap = m.snapshot_session(&id).await.unwrap();
    // 从快照建新会话(fork)
    let id2 = m
        .create_session("shell", HashMap::new(), Some(snap.clone()), vec![])
        .unwrap();
    let r = m
        .exec_session(
            &id2,
            req(vec!["/bin/cat".into(), "f.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("forked"),
        "forked session should see snapshot file: {:?}",
        r.stdout
    );
    // list / destroy 快照
    assert!(m.list_snapshots().unwrap().contains(&snap));
    m.destroy_snapshot(&snap).unwrap();
    assert!(!m.list_snapshots().unwrap().contains(&snap));
}

#[tokio::test]
async fn volume_persists_across_sessions() {
    let (_tmp, m) = mgr().await;
    let vol = VolumeMount {
        name: "shared".into(),
        mount: "volumes/shared".into(),
    };
    // 会话 A 挂卷 + 写
    let a = m
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    m.exec_session(
        &a,
        req(vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo persist > volumes/shared/x.txt".into(),
        ]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    m.destroy_session(&a).unwrap();
    // 会话 B 挂同卷 + 读(跨会话持久)
    let b = m
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    let r = m
        .exec_session(
            &b,
            req(vec!["/bin/cat".into(), "volumes/shared/x.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("persist"),
        "volume should persist across sessions: {:?}",
        r.stdout
    );
    m.cleanup_volume("shared").unwrap();
}

#[tokio::test]
async fn session_survives_restart_via_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    // SM1: 建会话 + 写文件
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    sm1.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo survived > keep.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    drop(sm1);
    // SM2: 新 manager(同 base_dir)= "重启"
    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    let n = sm2.rebuild_from_disk().unwrap();
    assert_eq!(n, 1, "one session should be rebuilt");
    assert!(sm2.get_session(&id).is_some(), "session should be reconnectable");
    // exec 读到重启前写入的文件
    let r = sm2
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "keep.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&r.stdout).contains("survived"),
        "rebuilt session should see pre-restart file: {:?}",
        r.stdout
    );
}

/// cr-085 M2: started_at 跨重启保留(rebuild 从 .session-meta.json 的 started_at_secs 恢复,
/// 而非用 rebuild 时刻的 now)。若实现回退为 SystemTime::now(),断言失败。
#[tokio::test]
async fn session_started_at_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let started_before = sm1.get_session(&id).unwrap().started_at.unwrap();
    drop(sm1);
    // "重启":新 manager 同 base_dir + rebuild
    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    sm2.rebuild_from_disk().unwrap();
    let started_after = sm2
        .get_session(&id)
        .expect("rebuilt session reconnectable")
        .started_at
        .expect("started_at present after rebuild");
    assert_eq!(
        started_before, started_after,
        "started_at must be restored from meta, not rebuild time"
    );
}

/// cr-029 bug 修复:带卷会话重启后仍可写卷(landlock 重新授权)。
#[tokio::test]
async fn volume_survives_restart_with_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let vol = VolumeMount {
        name: "persist".into(),
        mount: "volumes/persist".into(),
    };
    let runner1 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm1 = SessionManager::new(runner1, Arc::new(AuditLogger::noop()));
    let id = sm1
        .create_session("shell", HashMap::new(), None, vec![vol.clone()])
        .unwrap();
    sm1.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo old > volumes/persist/v.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    drop(sm1);

    let runner2 = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let sm2 = SessionManager::new(runner2, Arc::new(AuditLogger::noop()));
    sm2.rebuild_from_disk().unwrap();
    // 重启后仍可写卷
    // 重启后仍可写卷(builtin echo,无 fork);经 file API 读取(无 fork,避开 nproc)
    sm2.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo new >> volumes/persist/v.txt".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    let data = sm2.get_file(&id, "volumes/persist/v.txt").unwrap();
    let out = String::from_utf8_lossy(&data);
    assert!(
        out.contains("old") && out.contains("new"),
        "volume should persist + be writable post-restart: {out}"
    );
}

/// 会话内 exec 串行(exec_lock 互斥):A1 与 A2 必相邻(A 原子执行,B 不插中间)。
#[tokio::test]
async fn session_exec_is_serialized() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let a = m.exec_session(
        &id,
        req(vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo A1 >> out.txt; i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done; echo A2 >> out.txt".into(),
        ]),
        CancellationToken::new(),
        None,
    );
    let b = m.exec_session(
        &id,
        req(vec!["/bin/sh".into(), "-c".into(), "echo B >> out.txt".into()]),
        CancellationToken::new(),
        None,
    );
    let (ra, rb) = tokio::join!(a, b);
    assert!(
        ra.as_ref().is_ok_and(|r| matches!(r.status, JobStatus::Completed)),
        "concurrent exec A should complete: {ra:?}"
    );
    assert!(
        rb.as_ref().is_ok_and(|r| matches!(r.status, JobStatus::Completed)),
        "concurrent exec B should complete: {rb:?}"
    );
    let r = m
        .exec_session(
            &id,
            req(vec!["/bin/cat".into(), "out.txt".into()]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    let out = String::from_utf8_lossy(&r.stdout).into_owned();
    let lines: Vec<&str> = out.lines().collect();
    let a1 = lines.iter().position(|l| *l == "A1").expect("A1 present");
    assert_eq!(
        lines.get(a1 + 1),
        Some(&"A2"),
        "exec not serialized (B interleaved between A1/A2): {:?}",
        lines
    );
}

/// rebuild 跳过 profile 已不存在的会话。
#[tokio::test]
async fn rebuild_skips_session_with_missing_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 0,
    };
    // 预置一个 meta 指向不存在的 profile
    let dir = tmp.path().join("sessions").join("ghost");
    std::fs::create_dir_all(dir.join("workspace")).unwrap();
    std::fs::write(
        dir.join(".session-meta.json"),
        r#"{"profile_name":"ghost","env":{},"volumes":[]}"#,
    )
    .unwrap();
    let runner = Arc::new(SandboxRunner::new(&cfg).await.unwrap());
    let m = SessionManager::new(runner, Arc::new(AuditLogger::noop()));
    let n = m.rebuild_from_disk().unwrap();
    assert_eq!(n, 0, "session with missing profile should be skipped");
    assert!(m.get_session("ghost").is_none());
}

/// 会话 exec 可被 cancel。
#[tokio::test]
async fn session_exec_cancel() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });
    let r = m
        .exec_session(
            &id,
            req(vec!["/bin/sleep".into(), "10".into()]),
            cancel,
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(r.status, JobStatus::Cancelled),
        "expected Cancelled, got {:?}",
        r.status
    );
}

/// disk_quota 在会话 exec 里生效(看门狗测 workspace.root)。
#[tokio::test]
async fn session_disk_quota_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let mut runner = SandboxRunner::new(&cfg).await.unwrap();
    let mut rlimit = SandboxProfile::shell().rlimit;
    rlimit.nproc = None;
    rlimit.fsize_bytes = Some(1024 * 1024 * 1024);
    runner.register_profile(SandboxProfile {
        name: "quota".to_string(),
        disk_quota_mb: Some(1),
        rlimit,
        ..SandboxProfile::shell()
    });
    let m = SessionManager::new(Arc::new(runner), Arc::new(AuditLogger::noop()));
    let id = m.create_session("quota", HashMap::new(), None, vec![]).unwrap();
    let r = m
        .exec_session(
            &id,
            req(vec![
                "/bin/sh".into(),
                "-c".into(),
                "yes | head -c 200000000 > big; sleep 5".into(),
            ]),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(r.status, JobStatus::DiskQuotaExceeded),
        "session disk quota should be enforced, got {:?}",
        r.status
    );
}

/// cr-031 gap: session exec 也触发 webhook(不只 job 路径)。
#[tokio::test]
async fn session_exec_fires_webhook() {
    use sandbox_server::webhook::WebhookDispatcher;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(body_partial_json(serde_json::json!({"event_type": "JobCompleted"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let (_tmp, _base_mgr) = mgr().await;
    // Rebuild with webhook
    let runner = Arc::new(SandboxRunner::new(&SandboxConfig {
        sandbox_base_dir: _tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    }).await.unwrap());
    let wh = WebhookDispatcher::new(vec![format!("{}/hook", mock.uri())]);
    let m = SessionManager::new(runner, Arc::new(AuditLogger::noop()))
        .with_webhooks(Arc::new(wh));

    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();
    m.exec_session(
        &id,
        req(vec!["/bin/echo".into(), "wh-session".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    mock.verify().await;
    let _ = m.destroy_session(&id);
}

// ==================== cr-040: 会话 TTL 自动清理 ====================

/// cr-040: reap_expired(0) 清理所有会话(ttl=0 立即过期)。
#[tokio::test]
async fn reap_expired_zero_ttl_cleans_all() {
    let (_tmp, m) = mgr().await;
    let id1 = m
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();
    let id2 = m
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();

    let reaped = m.reap_expired(0);
    assert_eq!(reaped.len(), 2);
    assert!(reaped.contains(&id1));
    assert!(reaped.contains(&id2));
    assert!(m.get_session(&id1).is_none());
    assert!(m.get_session(&id2).is_none());
    assert!(m.list_sessions().is_empty());
}

/// cr-040: reap_expired 在 ttl 很大时保留所有未过期会话。
#[tokio::test]
async fn reap_expired_large_ttl_keeps_all() {
    let (_tmp, m) = mgr().await;
    let id = m
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();

    let reaped = m.reap_expired(u64::MAX);
    assert!(reaped.is_empty());
    assert!(m.get_session(&id).is_some());
}

/// cr-040: reap_expired 只清理超过 TTL 的会话,保留未过期的。
#[tokio::test]
async fn reap_expired_only_cleans_expired_sessions() {
    let (_tmp, m) = mgr().await;
    // 创建会话 A
    let id_a = m
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();
    // 等待 2s 让 A 变"旧"
    tokio::time::sleep(Duration::from_secs(2)).await;
    // 创建会话 B(刚创建,活跃)
    let id_b = m
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();

    // TTL=1s:A 过期,B 未过期
    let reaped = m.reap_expired(1);
    assert!(reaped.contains(&id_a), "session A should be expired");
    assert!(!reaped.contains(&id_b), "session B should NOT be expired");
    assert!(m.get_session(&id_a).is_none(), "A should be destroyed");
    assert!(m.get_session(&id_b).is_some(), "B should survive");
}

/// cr-040: reaper 后台任务定时扫描并清理过期会话。
#[tokio::test]
async fn reaper_periodically_cleans_expired_sessions() {
    let (_tmp, m) = mgr().await;
    let sm = Arc::new(m);
    let id = sm
        .create_session("shell", HashMap::new(), None, vec![])
        .unwrap();

    // TTL=1s,每 2s 扫描一次。首次 tick 立即触发(此时会话未过期),
    // 第二个 tick(2s 后)会话已超 1s TTL,应被清理。
    let handle = sm.spawn_reaper(1, 2);

    // 等待足够时间让 reaper 至少执行两次 tick(t=0 + t=2s)
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert!(
        sm.get_session(&id).is_none(),
        "expired session should be reaped by background reaper"
    );

    handle.abort();
}

/// cr-041: session exec 应 emit Prometheus 指标(job_started/job_finished 递增)。
#[tokio::test]
async fn session_exec_emits_metrics() {
    let (_tmp, m) = mgr().await;
    let id = m.create_session("shell", HashMap::new(), None, vec![]).unwrap();

    // 读执行前计数器(从 prometheus 默认 registry)
    fn counter_val(name: &str) -> f64 {
        for mf in prometheus::gather() {
            if mf.get_name() == name && !mf.get_metric().is_empty() {
                return mf.get_metric()[0].get_counter().get_value();
            }
        }
        0.0
    }
    let started_before = counter_val("sandbox_job_started_total");
    let finished_before = counter_val("sandbox_job_finished_total");

    m.exec_session(
        &id,
        req(vec!["/bin/echo".into(), "metrics-test".into()]),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    // exec 后计数器应 +1
    assert!(
        counter_val("sandbox_job_started_total") > started_before,
        "JOB_STARTED_TOTAL should increment"
    );
    assert!(
        counter_val("sandbox_job_finished_total") > finished_before,
        "JOB_FINISHED_TOTAL should increment"
    );
}
