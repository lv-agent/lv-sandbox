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
