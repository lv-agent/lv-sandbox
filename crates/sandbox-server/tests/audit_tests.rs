//! 审计日志测试(cr-021:扩 enum 对齐 JobStatus + argv/exit_code 字段)

use sandbox_server::audit::{AuditEvent, AuditEventType, AuditLogger};

/// AuditEventType 6 变体序列化(对齐 JobStatus)
#[test]
fn audit_event_type_all_variants_serialize() {
    let variants = [
        (AuditEventType::JobStarted, "\"JobStarted\""),
        (AuditEventType::JobCompleted, "\"JobCompleted\""),
        (AuditEventType::JobTimedOut, "\"JobTimedOut\""),
        (AuditEventType::JobKilled, "\"JobKilled\""),
        (AuditEventType::JobCancelled, "\"JobCancelled\""),
        (AuditEventType::JobFailed, "\"JobFailed\""),
    ];
    for (v, expected) in variants {
        assert_eq!(serde_json::to_string(&v).unwrap(), expected);
    }
}

/// AuditEvent 携带 argv + exit_code + 自动 timestamp
#[test]
fn audit_event_carries_argv_and_exit_code() {
    let ev = AuditEvent::new(
        AuditEventType::JobCompleted,
        "j-1",
        "shell",
        vec!["/bin/echo".into(), "hi".into()],
        Some(0),
        None,
        Some(3),
        None,
    );
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"argv\":[\"/bin/echo\",\"hi\"]"), "argv: {json}");
    assert!(json.contains("\"exit_code\":0"), "exit_code: {json}");
    assert!(json.contains("\"timestamp\""), "timestamp: {json}");
}

/// AuditLogger 写 JSONL 到文件
#[test]
fn audit_logger_writes_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("audit.jsonl");
    let logger = AuditLogger::file(&log_path).unwrap();

    logger.log(AuditEvent::new(
        AuditEventType::JobStarted,
        "audit-001",
        "shell",
        vec!["/bin/echo".into()],
        None,
        None,
        None,
        None,
    ));
    logger.log(AuditEvent::new(
        AuditEventType::JobCompleted,
        "audit-001",
        "shell",
        vec!["/bin/echo".into()],
        Some(0),
        None,
        Some(1000),
        None,
    ));

    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "should have 2 log lines: {content}");

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event_type"], "JobStarted");
    assert_eq!(first["job_id"], "audit-001");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["event_type"], "JobCompleted");
    assert_eq!(second["duration_ms"], 1000);
}

/// AuditLogger::noop 不崩溃(且静默——不写文件)
#[test]
fn audit_logger_noop_does_not_crash() {
    let logger = AuditLogger::noop();
    for i in 0..100 {
        logger.log(AuditEvent::new(
            AuditEventType::JobStarted,
            format!("noop-{i}"),
            "shell",
            vec![],
            None,
            None,
            None,
            None,
        ));
    }
}
