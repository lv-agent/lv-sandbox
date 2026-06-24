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

/// cr-019 HTTPS: python helper 经代理走 TLS(https)拿 200。
///
/// 自签证书(openssl 生成)+ Python TLS server(子进程,ssl.wrap_socket)
/// + JobProxy(测试进程内)+ python helper 子进程。SSL_CERT_FILE 让 helper
/// 信任自签证书。验证 helper 的 TLS 分支(ssl.wrap_socket over SOCKS5 relay)。
#[tokio::test]
async fn python_helper经代理走https() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    // 跳过门:openssl + python3
    if Command::new("openssl").arg("version").output().await.is_err()
        || Command::new("python3").arg("--version").output().await.is_err()
    {
        eprintln!("跳过:环境无 openssl/python3");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");

    // 1) 生成自签证书(CN=localhost)
    let gen = Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048",
            "-keyout", key.to_str().unwrap(),
            "-out", cert.to_str().unwrap(),
            "-days", "1", "-nodes", "-subj", "/CN=localhost",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        gen.status.success(),
        "openssl 生成证书失败: {}",
        String::from_utf8_lossy(&gen.stderr)
    );

    // 2) Python TLS server 子进程:bind 0,打印端口,accept 一个连接,TLS 回 200
    let py_server = r#"
import ssl, socket, sys
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(sys.argv[1], sys.argv[2])
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0)); s.listen(1)
print(s.getsockname()[1], flush=True)
cs, _ = s.accept()
cs = ctx.wrap_socket(cs, server_side=True)
cs.recv(4096)
cs.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
cs.close()
"#;
    let mut server = Command::new("python3")
        .arg("-c")
        .arg(py_server)
        .arg(cert.to_str().unwrap())
        .arg(key.to_str().unwrap())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(server.stdout.take().unwrap());
    let mut port_line = String::new();
    reader
        .read_line(&mut port_line)
        .await
        .expect("读取 TLS server 端口失败");
    let port: u16 = port_line.trim().parse().expect("端口解析失败");

    // 3) JobProxy(allowlist localhost:port)
    let proxy_tmp = tempfile::tempdir().unwrap();
    let matcher = sandbox_core::egress::AllowlistMatcher::new(vec![
        sandbox_core::egress::EgressRule {
            host: "localhost".into(),
            port: Some(port),
        },
    ]);
    let (proxy, sock_path) =
        sandbox_core::proxy::JobProxy::start(proxy_tmp.path(), matcher).await.unwrap();

    // 4) python helper 子进程走 https(SSL_CERT_FILE 信任自签证书)
    let helper_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../helpers/python");
    let script = format!(
        "import sys; sys.path.insert(0, r'{d}'); import sandbox_net; \
         r = sandbox_net.request('GET', 'https://localhost:{p}/'); print(r.status)",
        d = helper_dir.display(),
        p = port,
    );
    let output = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .env("SANDBOX_PROXY_SOCK", sock_path.to_str().unwrap())
        .env("SSL_CERT_FILE", cert.to_str().unwrap())
        .output()
        .await
        .expect("执行 python helper 失败");

    proxy.stop().await;
    let _ = server.kill().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("200"),
        "helper 经 https 应拿 200,stdout={:?} stderr={:?}",
        stdout,
        stderr
    );
}

/// cr-019 HTTPS(node): node helper 经代理走 TLS 拿 200。
/// 与 python HTTPS 同一份自签证书 + Python TLS server,换 node helper +
/// NODE_EXTRA_CA_CERTS 信任自签。验证 node helper 的 tls.connect 分支。
#[tokio::test]
async fn node_helper经代理走https() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    if Command::new("openssl").arg("version").output().await.is_err()
        || Command::new("node").arg("--version").output().await.is_err()
    {
        eprintln!("跳过:环境无 openssl/node");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");
    let gen = Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048",
            "-keyout", key.to_str().unwrap(),
            "-out", cert.to_str().unwrap(),
            "-days", "1", "-nodes", "-subj", "/CN=localhost",
        ])
        .output().await.unwrap();
    assert!(gen.status.success(), "openssl 生成证书失败: {}", String::from_utf8_lossy(&gen.stderr));

    let py_server = r#"
import ssl, socket, sys
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(sys.argv[1], sys.argv[2])
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0)); s.listen(1)
print(s.getsockname()[1], flush=True)
cs, _ = s.accept()
cs = ctx.wrap_socket(cs, server_side=True)
cs.recv(4096)
cs.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
cs.close()
"#;
    let mut server = Command::new("python3")
        .arg("-c").arg(py_server)
        .arg(cert.to_str().unwrap()).arg(key.to_str().unwrap())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true).spawn().unwrap();
    let mut reader = BufReader::new(server.stdout.take().unwrap());
    let mut port_line = String::new();
    reader.read_line(&mut port_line).await.expect("读取端口失败");
    let port: u16 = port_line.trim().parse().expect("端口解析失败");

    let proxy_tmp = tempfile::tempdir().unwrap();
    let matcher = sandbox_core::egress::AllowlistMatcher::new(vec![
        sandbox_core::egress::EgressRule { host: "localhost".into(), port: Some(port) },
    ]);
    let (proxy, sock_path) =
        sandbox_core::proxy::JobProxy::start(proxy_tmp.path(), matcher).await.unwrap();

    let helper = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../helpers/node/sandbox-net.js");
    let script = format!(
        "const {{request}}=require('{h}'); request('GET','https://localhost:{p}/',null,null,(e,r)=>{{if(e){{console.error(e.message);process.exit(1)}}console.log(r.status)}})",
        h = helper.display(), p = port,
    );
    let output = Command::new("node")
        .arg("-e").arg(&script)
        .env("SANDBOX_PROXY_SOCK", sock_path.to_str().unwrap())
        .env("NODE_EXTRA_CA_CERTS", cert.to_str().unwrap())
        .output().await.expect("执行 node 失败");

    proxy.stop().await;
    let _ = server.kill().await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("200"),
        "node helper 经 https 应拿 200,stdout={:?} stderr={:?}",
        stdout, stderr
    );
}
