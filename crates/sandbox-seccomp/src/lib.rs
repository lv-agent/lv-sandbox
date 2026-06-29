//! # sandbox-seccomp
//!
//! seccomp-BPF 管理：filter 构建器、denylist、clone namespace flag 条件过滤。
//!
//! 核心模式：`prepare()`（fork 前，可分配内存）→ `apply()`（pre_exec 中，纯 syscall）。

pub mod clone_filter;
pub mod error;
pub mod filter;
pub mod profile;
pub mod syscall;

mod allowlist;

pub use error::SeccompError;
pub use filter::PreparedFilter;
pub use profile::{CompareOperator, SeccompAction, SeccompCondition, SeccompProfile, SeccompRule};
pub use syscall::Syscall;
