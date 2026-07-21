//! cr-12 G1 步骤3.5:端到端集成测试 —— 真 `git clone` + `git push`
//! 经 helper → UDS 代理 → SOCKS5h → TLS → 本地 git-http-backend(CGI)上游。
//!
//! 链路:
//!   git clone ─spawn─> git-remote-fixus ─UDS─> [测试 SOCKS5h 代理] ─TCP─>
//!   [测试 TLS git-http-backend CGI 服务器] ─CGI─> git http-backend ─> 裸上游仓库
//!
//! 验证:clone 成功 + 文件存在;新增提交 push 后上游收到。
//!
//! 共用脚手架(证书 / SOCKS 代理 / CGI 上游 / git 包装)在 `tests/common/mod.rs`。

mod common;

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{git, path_with_helper, spawn_cgi_tls_server, spawn_proxy, wait_socket};

#[test]
fn clone_and_push_through_helper() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = root.path().join("upstream.git");
    // 裸上游 + 初始提交
    git(root.path(), &["init", "--bare", "upstream.git"]);
    // 往裸仓库注入一个初始提交:用一个临时工作仓库推上去
    let seed = root.path().join("seed");
    git(root.path(), &["init", "seed"]);
    git(&seed, &["config", "user.email", "t@t"]);
    git(&seed, &["config", "user.name", "t"]);
    std::fs::write(seed.join("README"), "hello from upstream\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-m", "init"]);
    git(&seed, &["branch", "-M", "main"]);
    git(&seed, &["remote", "add", "origin", upstream.to_str().unwrap()]);
    git(&seed, &["push", "-q", "origin", "main"]);
    // 裸上游 HEAD 默认指向 master,但只推了 main → 指向不存在的分支。校正为 main。
    git(&upstream, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    // 允许 HTTP push
    git(&upstream, &["config", "http.receivepack", "true"]);
    git(&upstream, &["config", "http.uploadpack", "true"]);

    // TLS git-http-backend 上游(G1 不关心 Authorization,传 throwaway recorder)。
    let auth_throwaway = Arc::new(Mutex::new(None));
    let (port, cert) = spawn_cgi_tls_server(root.path().to_path_buf(), auth_throwaway);
    let ca_file = root.path().join("ca.pem");
    std::fs::write(&ca_file, &cert.cert_pem).unwrap();

    // UDS SOCKS5h 代理
    let sock_path = root.path().join(".proxy.sock");
    let _proxy = spawn_proxy(sock_path.clone(), "localhost".into());
    wait_socket(&sock_path);
    assert!(sock_path.exists(), "proxy socket not ready");

    let path_env = path_with_helper();

    let clone_dir = root.path().join("work");
    let url = format!("fixus::https://localhost:{port}/upstream.git");

    // 给上游一点就绪时间
    std::thread::sleep(Duration::from_millis(80));

    // clone
    let st = Command::new("git")
        .current_dir(root.path())
        .env("PATH", &path_env)
        .env("SANDBOX_PROXY_SOCK", &sock_path)
        .env("SANDBOX_CA_FILE", &ca_file)
        .args([
            "clone",
            "-c",
            "protocol.version=0", // helper 目前支持 v0/v1;强制 v0 避免服务端走 v2
            &url,
            "work",
        ])
        .output()
        .expect("git clone");
    if !st.status.success() {
        panic!(
            "clone failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&st.stdout),
            String::from_utf8_lossy(&st.stderr)
        );
    }
    let readme = std::fs::read_to_string(clone_dir.join("README")).unwrap();
    assert_eq!(readme, "hello from upstream\n", "cloned content mismatch");

    // 新增提交并 push
    git(&clone_dir, &["config", "user.email", "t@t"]);
    git(&clone_dir, &["config", "user.name", "t"]);
    std::fs::write(clone_dir.join("note.txt"), "pushed\n").unwrap();
    git(&clone_dir, &["add", "note.txt"]);
    git(&clone_dir, &["commit", "-q", "-m", "add note"]);
    let push = Command::new("git")
        .current_dir(&clone_dir)
        .env("PATH", &path_env)
        .env("SANDBOX_PROXY_SOCK", &sock_path)
        .env("SANDBOX_CA_FILE", &ca_file)
        .args(["-c", "protocol.version=0", "push", "-q", "origin", "main"])
        .output()
        .expect("git push");
    if !push.status.success() {
        panic!(
            "push failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&push.stdout),
            String::from_utf8_lossy(&push.stderr)
        );
    }

    // 上游应收到新提交:ls-tree 看 note.txt
    let tree = git(&upstream, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        tree.contains("note.txt"),
        "upstream did not receive pushed file; tree:\n{tree}"
    );
    let blob = git(
        &upstream,
        &["show", "main:note.txt"],
    );
    assert_eq!(blob, "pushed\n", "pushed file content mismatch on upstream");
}
