//! # sandbox-core
//!
//! 轻量级沙箱核心：进程编排、环境/FD 清理、rlimit、workspace 管理、job 状态机。
//!
//! 组合 sandbox-landlock、sandbox-seccomp、sandbox-cgroup 三个安全 crate，
//! 定义完整的沙箱上下文和 job 生命周期。

pub mod capability;
pub mod egress;
pub mod env;
pub mod error;
pub mod fd;
pub mod job;
pub mod process;
pub mod profile;
pub mod recovery;
pub mod rlimit;
pub mod sandbox_context;
pub mod workspace;

// Re-export 子 crate
pub use sandbox_cgroup as cgroup;
pub use sandbox_landlock as landlock;
pub use sandbox_seccomp as seccomp;

pub use capability::CapabilityReport;
pub use egress::{AllowlistMatcher, EgressRule};
pub use error::CoreError;
pub use job::{JobRequest, JobResult, JobStatus, ResourceSummary, SandboxViolation};
pub use process::{PreExecError, PreparedSandboxContext};
pub use profile::{LandlockTemplate, ProfileRegistry, SandboxProfile};
pub use recovery::RecoveryReport;
pub use rlimit::RlimitConfig;
pub use sandbox_context::{SandboxConfig, SandboxRunner};
pub use workspace::JobState;
