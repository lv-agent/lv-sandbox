//! cr-020 e2e: python profile 真跑脚本(验证 landlock 放行 stdlib)。

use std::collections::HashMap;

use axum::http::StatusCode;
use sandbox_e2e::helpers::*;

/// python profile 跑 `import json`(stdlib)——验证探测路径注入后 stdlib 可读。
#[tokio::test]
async fn python_profile_真跑stdlib脚本() {
    let (_tmp, app) = create_test_app().await;
    let argv = [
        "/usr/bin/python3",
        "-c",
        "import json,sys; print('py-ok', sys.version.split()[0])",
    ];
    let (status, job) =
        submit_and_wait(app, "rt-py", &argv, "python", "10s", HashMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        job["status"].as_str().unwrap(),
        "Completed",
        "python 应跑通(landlock 放行 stdlib),实际 job: {}",
        job
    );
    assert!(
        job["stdout"].as_str().unwrap().contains("py-ok"),
        "stdout 应含 py-ok,实际: {}",
        job["stdout"]
    );
}
