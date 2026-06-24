//! 性能基准测试：fork→exec 延迟、runner 常驻内存、单 job 额外开销
//!
//! 目标（cr-002）：
//! - fork→exec P99 ≤ 10ms
//! - runner RSS < 100MB
//! - per-job overhead ≤ 2MB

use criterion::{criterion_group, criterion_main, Criterion};
use std::collections::HashMap;
use std::fs;
use std::time::Duration;

use sandbox_core::job::JobRequest;
use sandbox_core::sandbox_context::{SandboxConfig, SandboxRunner};

/// 创建测试用 SandboxRunner
async fn create_bench_runner() -> (tempfile::TempDir, SandboxRunner) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let config = SandboxConfig {
        sandbox_base_dir: tmp.path().to_path_buf(),
        disk_watermark_bytes: 1024 * 1024 * 1024,
    };
    let runner = SandboxRunner::new(&config)
        .await
        .expect("failed to create runner");
    (tmp, runner)
}

fn make_request(job_id: &str, argv: &[&str]) -> JobRequest {
    JobRequest {
        job_id: job_id.to_string(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        profile_name: "shell".to_string(),
        timeout: Some(Duration::from_secs(5)),
        custom_env: HashMap::new(),
        stdin_data: None,
    }
}

/// 读取当前进程 VmRSS（KB）
fn read_vm_rss_kb() -> u64 {
    let status = fs::read_to_string("/proc/self/status").expect("failed to read /proc/self/status");
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // VmRSS:    12345 kB
            return parts[1].parse().expect("failed to parse VmRSS value");
        }
    }
    panic!("VmRSS not found");
}

/// 基准 1：fork→exec 延迟
///
/// 测量从提交 job 到完成（echo hello）的端到端延迟。
/// 这是 fork + pre_exec + exec + wait 的完整开销。
fn bench_fork_exec_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let (_tmp, runner) = rt.block_on(create_bench_runner());

    let mut group = c.benchmark_group("fork_exec");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("echo_hello", |b| {
        b.iter(|| {
            let req = make_request("bench-echo", &["/bin/echo", "hello"]);
            rt.block_on(runner.run_job(req)).expect("run_job should not fail");
        });
    });

    group.finish();
}

/// 基准 2：runner 常驻内存
///
/// 测量空载 runner 的 RSS。
/// 这不是传统意义上的 benchmark，而是利用 criterion 的输出报告机制。
fn bench_runner_rss(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let (_tmp, _runner) = rt.block_on(create_bench_runner());

    let mut group = c.benchmark_group("memory");
    group.sample_size(10);

    group.bench_function("runner_rss", |b| {
        b.iter(|| {
            let rss_kb = read_vm_rss_kb();
            rss_kb
        });
    });

    // 报告 RSS
    let rss_kb = read_vm_rss_kb();
    let rss_mb = rss_kb as f64 / 1024.0;
    eprintln!("\n=== Runner idle RSS: {:.1} MB ({}) KB ===\n", rss_mb, rss_kb);
    assert!(rss_mb < 100.0, "runner RSS should be < 100MB, actual {:.1}MB", rss_mb);

    group.finish();
}

/// 基准 3：per-job 额外内存开销
///
/// 提交 N 个轻量 job，测量 RSS 增量。
fn bench_per_job_overhead(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let (_tmp, runner) = rt.block_on(create_bench_runner());

    let mut group = c.benchmark_group("memory");
    group.sample_size(10);

    let rss_before_kb = read_vm_rss_kb();

    // 顺序执行 20 个轻量 job
    let n_jobs: u64 = 20;
    for i in 0..n_jobs {
        let req = make_request(
            &format!("mem-{i}"),
            &["/bin/sh", "-c", "echo ok"],
        );
        rt.block_on(runner.run_job(req)).expect("run_job should not fail");
    }

    let rss_after_kb = read_vm_rss_kb();
    let delta_kb = rss_after_kb.saturating_sub(rss_before_kb);
    let per_job_kb = delta_kb / n_jobs;
    let per_job_mb = per_job_kb as f64 / 1024.0;

    eprintln!(
        "\n=== Per-job memory overhead: {:.2} MB/job ({} KB/job) ===",
        per_job_mb, per_job_kb
    );
    eprintln!(
        "    RSS before: {} KB, after: {} KB, delta: {} KB",
        rss_before_kb, rss_after_kb, delta_kb
    );

    group.finish();
}

criterion_group!(
    benches,
    bench_fork_exec_latency,
    bench_runner_rss,
    bench_per_job_overhead,
);
criterion_main!(benches);
