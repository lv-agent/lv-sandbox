//! # sandbox-landlock
//!
//! Landlock LSM 封装：ABI 运行时检测、文件系统策略构建、预编译 ruleset。
//!
//! 核心模式：`prepare()`（fork 前，可分配内存）→ `apply()`（pre_exec 中，纯 syscall）。

pub mod abi;
pub mod access;
pub mod error;
pub mod policy;
pub mod ruleset;

pub use abi::LandlockCapabilities;
pub use access::{AccessFs, AccessRule};
pub use error::LandlockError;
pub use policy::{FsPolicy, RuntimeKind};
pub use ruleset::PreparedRuleset;

/// 检测当前内核的 Landlock 能力。runner 启动时调用一次，结果缓存。
pub fn detect_capabilities() -> LandlockCapabilities {
    abi::detect()
}
