//! cr-12 G2 reference swap-proxy library。
//!
//! - [`config`] : 全 env 配置解析。
//! - [`swap`]   : 纯函数头部改写(单测友好)。
//! - [`server`] : TLS-terminate → swap → TLS-forward 的 accept 循环与单连接处理。
//!
//! binary 入口见 `src/main.rs`(`fixus-egress-swap-proxy`)。
//! Task 4 把 accept 循环 + 单连接处理从 main.rs 搬到此处(`server` 模块),
//! 让 E2E 测试可直接 `server::serve(listener, ...)` 驱动,无需 spawn 子进程。

pub mod config;
pub mod server;
pub mod swap;
