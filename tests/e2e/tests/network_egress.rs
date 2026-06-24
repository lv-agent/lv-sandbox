//! cr-019 e2e:AF_UNIX-only 放行、INET 阻断、SANDBOX_PROXY_SOCK 注入。

use std::collections::HashMap;

use axum::http::StatusCode;
use sandbox_core::egress::EgressRule;
use sandbox_core::profile::SandboxProfile;
use sandbox_e2e::helpers::*; // glob 导入,同 profile_python.rs

/// 有白名单的 profile:起代理,任务能 AF_UNIX 连上 proxy.sock。
#[tokio::test]
async fn 有白名单时任务能连代理socket() {
    let profile = SandboxProfile {
        name: "egress_on".into(),
        egress_allowlist: vec![EgressRule {
            host: "localhost".into(),
            port: None,
        }],
        ..SandboxProfile::python()
    };
    let (_tmp, app) = create_test_app_with_profiles(vec![profile]).await;

    // 任务:AF_UNIX connect 到 SANDBOX_PROXY_SOCK(证明 seccomp 放行 + env 注入 + socket 存在)
    let argv = [
        "/usr/bin/python3",
        "-c",
        "import socket,os; s=socket.socket(socket.AF_UNIX); s.connect(os.environ['SANDBOX_PROXY_SOCK']); print('connected')",
    ];
    let (status, job) =
        submit_and_wait(app, "egress-afunix", &argv, "egress_on", "10s", HashMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        job["stdout"].as_str().unwrap().contains("connected"),
        "AF_UNIX 应放行,实际 job: {}",
        job
    );
}

/// INET socket 仍被 seccomp KILL(用 python profile 跑 python3,landlock 放行解释器)。
#[tokio::test]
async fn inet_socket仍被阻断() {
    let (_tmp, app) = create_test_app().await;
    let argv = [
        "/usr/bin/python3",
        "-c",
        "import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)",
    ];
    let (status, job) =
        submit_and_wait(app, "egress-inet", &argv, "python", "10s", HashMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job["status"], "Killed", "INET socket 应被 seccomp 杀死");
}

/// 空白名单 profile:不注入 SANDBOX_PROXY_SOCK。
#[tokio::test]
async fn 空白名单时不注入proxy_env() {
    let (_tmp, app) = create_test_app().await;
    let argv = [
        "/bin/sh",
        "-c",
        "test -z \"$SANDBOX_PROXY_SOCK\" && echo no_proxy_env",
    ];
    let (status, job) =
        submit_and_wait(app, "egress-noenv", &argv, "shell", "10s", HashMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(job["stdout"]
        .as_str()
        .unwrap()
        .contains("no_proxy_env"));
}

/// python helper sandbox_net 经代理往返 loopback 上游。
#[tokio::test]
async fn python_helper经代理往返上游() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // mock 上游:期望 "GET / HTTP/1.1",回 200 + body
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = upstream.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = upstream.accept().await.unwrap();
        let mut buf = [0u8; 128];
        let _ = s.read(&mut buf).await;
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nHELLO")
            .await
            .unwrap();
    });

    // helper 路径(仓库内),通过 extra_readonly_paths 暴露给任务
    let helpers_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../helpers/python");
    let profile = SandboxProfile {
        name: "egress_helper".into(),
        egress_allowlist: vec![EgressRule {
            host: "localhost".into(),
            port: Some(port),
        }],
        extra_readonly_paths: vec![helpers_dir.clone()],
        ..SandboxProfile::python()
    };
    let (_tmp, app) = create_test_app_with_profiles(vec![profile]).await;

    let script = format!(
        "import sys; sys.path.insert(0, r'{dir}'); import sandbox_net; \
         r = sandbox_net.request('GET', 'http://localhost:{port}/'); print(r.status)",
        dir = helpers_dir.display(),
        port = port,
    );
    let argv = ["/usr/bin/python3", "-c", &script];
    let (status, job) =
        submit_and_wait(app, "egress-helper", &argv, "egress_helper", "15s", HashMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        job["stdout"].as_str().unwrap().contains("200"),
        "helper 经代理应拿到 200,实际 job: {}",
        job
    );
}

/// cr-019 gap4: node helper 经代理往返 loopback 上游。
///
/// 注:本环境 node 装在 nvm,不在 sandbox 的 Node landlock 白名单
/// (/usr/bin、/usr/lib/node_modules)内,无法作为"被沙箱化任务"运行。
/// 故改用宿主 node 直接运行 helper,对真实的 per-job proxy(JobProxy)发请求,
/// 验证 node helper 的 SOCKS5h+HTTP 客户端逻辑(代理代码与 python e2e 同一份)。
#[tokio::test]
async fn node_helper经代理往返上游() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    // node 不存在则跳过
    if Command::new("node")
        .arg("--version")
        .output()
        .await
        .is_err()
    {
        eprintln!("跳过:环境无 node");
        return;
    }

    // mock 上游
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = upstream.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = upstream.accept().await.unwrap();
        let mut buf = [0u8; 128];
        let _ = s.read(&mut buf).await;
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
            .await
            .unwrap();
    });

    // 真实 per-job 代理(不经 server,直接用 JobProxy)
    let tmp = tempfile::tempdir().unwrap();
    let matcher = sandbox_core::egress::AllowlistMatcher::new(vec![
        sandbox_core::egress::EgressRule {
            host: "localhost".into(),
            port: Some(port),
        },
    ]);
    let (proxy, sock_path) =
        sandbox_core::proxy::JobProxy::start(tmp.path(), matcher).await.unwrap();

    let helper = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../helpers/node/sandbox-net.js");
    let script = format!(
        "const {{request}}=require('{h}'); request('GET','http://localhost:{p}/',null,null,(e,r)=>{{if(e){{console.error(e.message);process.exit(1)}}console.log(r.status)}})",
        h = helper.display(),
        p = port,
    );

    // 用 tokio::process(异步),否则 std 阻塞 output 会饿死单线程 runtime 里的代理 task
    let output = Command::new("node")
        .arg("-e")
        .arg(&script)
        .env("SANDBOX_PROXY_SOCK", sock_path.to_str().unwrap())
        .output()
        .await
        .expect("执行 node 失败");

    proxy.stop().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("200"),
        "node helper 经代理应拿到 200,stdout={:?} stderr={:?}",
        stdout,
        stderr
    );
}
