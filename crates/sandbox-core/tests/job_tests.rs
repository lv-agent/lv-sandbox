//! JobStatus 序列化测试(cr-022)。
use sandbox_core::job::JobStatus;

#[test]
fn job_status_disk_quota_exceeded_serializes() {
    let json = serde_json::to_string(&JobStatus::DiskQuotaExceeded).unwrap();
    assert_eq!(json, "\"DiskQuotaExceeded\"");
}
