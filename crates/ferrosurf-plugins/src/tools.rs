//! Plugin tools as an MCP `ToolHandler`: what the MCP server and the AI
//! assistant see of `[[tools]]`.

use ferrosurf_mcp::server::{BoxFuture, ToolDef, ToolHandler, ToolOutput};
use ferrosurf_plugin_sdk::abi::{self, McpToolOutput};
use serde_json::Value;

use crate::host::{PluginHost, TOOL_TIMEOUT};

/// Serves the tools of every enabled plugin that holds `mcp_tools`.
/// Cheap to clone; the tool list is read live, so enabling or disabling a
/// plugin changes what the next `tools/list` returns.
#[derive(Clone)]
pub struct PluginToolHandler {
    host: PluginHost,
}

impl PluginToolHandler {
    pub fn new(host: PluginHost) -> Self {
        Self { host }
    }
}

impl ToolHandler for PluginToolHandler {
    fn tools(&self) -> Vec<ToolDef> {
        self.host.mcp_tools()
    }

    fn call(&self, name: &str, args: Value) -> BoxFuture<'static, ToolOutput> {
        let host = self.host.clone();
        let name = name.to_owned();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let reply = Box::new(move |r| {
                let _ = tx.send(r);
            });
            if let Err(e) = host.submit_tool(&name, args, reply) {
                return ToolOutput::error(e);
            }
            match tokio::time::timeout(TOOL_TIMEOUT + std::time::Duration::from_secs(1), rx).await {
                Ok(Ok(Ok(Some(bytes)))) => match abi::decode::<McpToolOutput>(&bytes) {
                    Ok(o) if o.is_error => ToolOutput::error(o.content_text),
                    Ok(o) => ToolOutput::text(o.content_text),
                    Err(_) => ToolOutput::error("la herramienta devolvió una respuesta inválida"),
                },
                Ok(Ok(Ok(None))) => ToolOutput::error("la herramienta no devolvió nada"),
                Ok(Ok(Err(e))) => ToolOutput::error(e),
                _ => ToolOutput::error("la herramienta no respondió a tiempo"),
            }
        })
    }
}
