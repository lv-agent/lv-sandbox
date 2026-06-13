//! # sandbox-cgroup
//!
//! cgroup v2 管理：可用性检测、资源限制、进程迁移、资源统计。

pub mod detect;
pub mod error;
pub mod manager;
pub mod resources;

pub use detect::{CgroupAvailability, CgroupController};
pub use error::CgroupError;
pub use manager::JobCgroup;
pub use resources::{CgroupResources, IoMax, ResourceUsage};

/// 检测当前环境中 cgroup v2 的可用性。runner 启动时调用一次。
pub fn detect() -> CgroupAvailability {
    detect::detect()
}
