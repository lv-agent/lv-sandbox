//! # sandbox-mcp
//!
//! MCP（Model Context Protocol）网关。
//!
//! 作为薄网关运行：接收 AI Agent（Claude Code、Hermes-Agent）的 MCP/stdio 请求，
//! 转换为 HTTP 调用转发给独立运行的 sandbox-server。
//!
//! 详见 `veps/cr-014-MCP集成设计.md`。

pub mod client;
pub mod server;
pub mod tools;

pub use client::SandboxHttpClient;
pub use server::SandboxMcpServer;
