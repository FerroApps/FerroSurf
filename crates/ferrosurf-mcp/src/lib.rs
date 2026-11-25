//! Native MCP (Model Context Protocol) support for FerroSurf.
//!
//! * [`server`]: expose browser tools to external agents (HTTP + stdio bridge).
//! * [`browser`]: the browser's own tool set, executed on the UI thread via a channel.
//! * [`client`]: consume external MCP servers (stdio / streamable HTTP).
//! * [`assistant`]: the in-browser AI sidebar agent loop (Anthropic / OpenAI-compatible).

pub mod assistant;
pub mod browser;
pub mod client;
pub mod jsonrpc;
pub mod server;
pub mod sse;
mod util;

pub use browser::{BrowserBridge, BrowserCommand, CommandReply, ToolPolicy};
pub use client::{ClientTransportConfig, McpClient, McpClientManager, ServerStatus};
pub use server::{
    Content, EndpointFile, HttpServerHandle, McpServer, ToolDef, ToolHandler, ToolOutput,
    run_stdio_bridge, spawn_http_server,
};
