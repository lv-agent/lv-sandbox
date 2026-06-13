//! 审计日志集成测试
//!
//! TDD RED：验证审计日志在 job 执行路径中被正确记录

use std::io::Write as IoWrite;
use std::sync::Arc;

/// 测试 AuditEvent 结构体可正确序列化
#[test]
fn audit_event序列化为json() {
    let event = sandbox_server::audit::AuditEvent {
        timestamp: "2026-06-13T12:00:00Z".to_string(),
        event_type: sandbox_server::audit::AuditEventType::JobStarted,
        job_id: "test-001".to_string(),
        profile: "shell".to_string(),
        detail: None,
        duration_ms: None,
    };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("JobStarted"));
    assert!(json.contains("test-001"));
    assert!(json.contains("shell"));
}

/// 测试 AuditEventType 所有变体可序列化
#[test]
fn audit_event_type所有变体可序列化() {
    use sandbox_server::audit::AuditEventType;

    let variants = vec![
        AuditEventType::JobStarted,
        AuditEventType::JobCompleted,
        AuditEventType::JobTimeout,
        AuditEventType::JobFailed,
    ];

    for v in &variants {
        let json = serde_json::to_string(v).unwrap();
        assert!(!json.is_empty(), "变体 {:?} 应可序列化", v);
    }
}

/// 测试 AuditLogger 写入到文件
#[test]
fn audit_logger写入文件() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("audit.jsonl");

    let logger = sandbox_server::audit::AuditLogger::file(&log_path).unwrap();

    logger.log(sandbox_server::audit::AuditEvent {
        timestamp: "2026-06-13T12:00:00Z".to_string(),
        event_type: sandbox_server::audit::AuditEventType::JobStarted,
        job_id: "audit-001".to_string(),
        profile: "shell".to_string(),
        detail: None,
        duration_ms: None,
    });

    logger.log(sandbox_server::audit::AuditEvent {
        timestamp: "2026-06-13T12:00:01Z".to_string(),
        event_type: sandbox_server::audit::AuditEventType::JobCompleted,
        job_id: "audit-001".to_string(),
        profile: "shell".to_string(),
        detail: None,
        duration_ms: Some(1000),
    });

    // 读取文件验证
    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "应有 2 行日志");

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event_type"], "JobStarted");
    assert_eq!(first["job_id"], "audit-001");

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["event_type"], "JobCompleted");
    assert_eq!(second["duration_ms"], 1000);
}

/// 测试 AuditLogger::noop 不崩溃
#[test]
fn audit_logger_noop不崩溃() {
    let logger = sandbox_server::audit::AuditLogger::noop();
    // 多次调用不 panic
    for i in 0..100 {
        logger.log(sandbox_server::audit::AuditEvent {
            timestamp: format!("2026-06-13T12:00:{:02}Z", i),
            event_type: sandbox_server::audit::AuditEventType::JobStarted,
            job_id: format!("noop-{}", i),
            profile: "shell".to_string(),
            detail: None,
            duration_ms: None,
        });
    }
}
