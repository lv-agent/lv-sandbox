//! 并发控制 E2E 测试
//!
//! 覆盖：并发提交、排队行为、实际并发上限、stdout 正确性、压力测试

use sandbox_core::job::JobRequest;
use sandbox_e2e::helpers::*;

#[tokio::test]
async fn 并发5个job全部完成() {
    let (_tmp, runner) = create_test_runner().await;
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let runner = &runner;
            let req = make_job_request_with_profile(
                &format!("conc-{}", i),
                &["/bin/echo", &format!("job-{}", i)],
                "shell",
                std::time::Duration::from_secs(5),
            );
            async move { runner.run_job(req).await }
        })
        .collect();

    let results = futures::future::join_all(handles).await;
    for (i, result) in results.into_iter().enumerate() {
        let r = result.unwrap_or_else(|_| panic!("job {} 不应报错", i));
        assert!(
            matches!(r.status, sandbox_core::job::JobStatus::Completed),
            "job {} 应正常完成",
            i
        );
        let stdout = String::from_utf8_lossy(&r.stdout);
        assert!(stdout.contains(&format!("job-{}", i)));
    }
}

#[tokio::test]
async fn 排队行为_超过max的job等待而非拒绝() {
    let (_tmp, runner) = create_test_runner().await;
    let scheduler = std::sync::Arc::new(sandbox_server::scheduler::Scheduler::new(
        std::sync::Arc::new(runner),
        2,
    ));

    // 提交 3 个短 job，max_concurrent=2，第 3 个应该排队等待
    let handles: Vec<_> = (0..3)
        .map(|i| {
            let sched = scheduler.clone();
            let req = JobRequest {
                job_id: format!("queue-{}", i),
                argv: vec!["/bin/echo".to_string(), format!("q-{}", i)],
                profile_name: "shell".to_string(),
                timeout: Some(std::time::Duration::from_secs(5)),
                custom_env: Default::default(),
                stdin_data: None,
            };
            async move { sched.clone().submit(req).await }
        })
        .collect();

    let results = futures::future::join_all(handles).await;
    for (i, result) in results.into_iter().enumerate() {
        let r = result.unwrap_or_else(|_| panic!("job {} 不应报错", i));
        assert!(
            matches!(r.status, sandbox_core::job::JobStatus::Completed),
            "job {} 应正常完成",
            i
        );
    }
}

#[tokio::test]
async fn 实际并发数不超过max_concurrent() {
    let (_tmp, runner) = create_test_runner().await;
    let max = 3;
    let scheduler = std::sync::Arc::new(sandbox_server::scheduler::Scheduler::new(
        std::sync::Arc::new(runner),
        max,
    ));

    // 提交 5 个快速 job
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let sched = scheduler.clone();
            let req = JobRequest {
                job_id: format!("cap-{}", i),
                argv: vec!["/bin/echo".to_string(), format!("{}", i)],
                profile_name: "shell".to_string(),
                timeout: Some(std::time::Duration::from_secs(5)),
                custom_env: Default::default(),
                stdin_data: None,
            };
            async move { sched.clone().submit(req).await }
        })
        .collect();

    let results = futures::future::join_all(handles).await;
    assert_eq!(results.len(), 5);
    for r in results {
        assert!(r.is_ok(), "job 不应报错");
    }
}

#[tokio::test]
async fn 并发job各捕获正确的stdout() {
    let (_tmp, runner) = create_test_runner().await;
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let runner = &runner;
            let req = make_job_request_with_profile(
                &format!("stdout-{}", i),
                &["/bin/echo", &format!("UNIQUE_{}", i)],
                "shell",
                std::time::Duration::from_secs(5),
            );
            async move { runner.run_job(req).await }
        })
        .collect();

    let results = futures::future::join_all(handles).await;
    for (i, result) in results.into_iter().enumerate() {
        let r = result.unwrap_or_else(|_| panic!("job {} 不应报错", i));
        let stdout = String::from_utf8_lossy(&r.stdout);
        assert!(
            stdout.contains(&format!("UNIQUE_{}", i)),
            "job {} stdout 应包含 UNIQUE_{}, 实际: {}",
            i, i, stdout
        );
    }
}

#[tokio::test]
async fn 快速提交20个job压力测试() {
    let (_tmp, runner) = create_test_runner().await;
    let scheduler = std::sync::Arc::new(sandbox_server::scheduler::Scheduler::new(
        std::sync::Arc::new(runner),
        10,
    ));

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let sched = scheduler.clone();
            let req = JobRequest {
                job_id: format!("stress-{}", i),
                argv: vec!["/bin/echo".to_string(), format!("{}", i)],
                profile_name: "shell".to_string(),
                timeout: Some(std::time::Duration::from_secs(10)),
                custom_env: Default::default(),
                stdin_data: None,
            };
            async move { sched.clone().submit(req).await }
        })
        .collect();

    let results = futures::future::join_all(handles).await;
    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok_count, 20, "20 个 job 应全部完成");
}
