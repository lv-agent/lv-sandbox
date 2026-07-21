//! cr-12 G2 reference swap-proxy library: 牢外 sentinel→real 兑换代理的核心逻辑。
//!
//! - [`swap`] : 纯函数头部改写(单测友好)。
//! - [`config`] : 全 env 配置解析。
//!
//! binary 入口见 `src/main.rs`(`fixus-egress-swap-proxy`)。

pub mod config;
pub mod swap;
