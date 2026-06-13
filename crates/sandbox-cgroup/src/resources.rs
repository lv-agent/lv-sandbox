use serde::{Deserialize, Serialize};

/// cgroup 资源限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CgroupResources {
    /// 内存上限（字节）
    pub memory_max: Option<u64>,
    /// CPU 配额（微秒）
    pub cpu_max_quota: Option<u64>,
    /// CPU 周期（微秒）
    pub cpu_max_period: Option<u64>,
    /// 最大进程数
    pub pids_max: Option<u64>,
    /// IO 限制
    pub io_max: Option<IoMax>,
}

/// IO 限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoMax {
    pub major: u64,
    pub minor: u64,
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
    pub read_iops: Option<u64>,
    pub write_iops: Option<u64>,
}

/// 资源使用统计
#[derive(Debug, Clone, Serialize)]
pub struct ResourceUsage {
    /// 当前内存使用（字节）
    pub memory_current: Option<u64>,
    /// 内存峰值（字节）
    pub memory_peak: Option<u64>,
    /// CPU 使用时间（微秒）
    pub cpu_usage_usec: Option<u64>,
    /// 当前进程数
    pub pids_current: Option<u64>,
}
