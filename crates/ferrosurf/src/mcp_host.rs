//! Native MCP: the browser as an MCP server for external agents, MCP client connections for
//! the built-in assistant, and the assistant's agent loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use ferrosurf_core::settings::{AssistantProvider, McpTransportConfig};
use ferrosurf_mcp::assistant::{AssistantConfig, AssistantEvent, Provider};
use ferrosurf_mcp::browser::CommandEnvelope;
use ferrosurf_mcp::{
    BrowserBridge, ClientTransportConfig, CommandReply, EndpointFile, HttpServerHandle, McpClientManager,
    ToolHandler, ToolPolicy,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::profile_data::ProfileData;
use crate::tab::TabId;
use crate::ui::SharedUi;

pub struct PendingLoad {
    pub tab: TabId,
    pub deadline: Instant,
    pub reply: oneshot::Sender<CommandReply>,
}

pub struct McpHost {
    /// Bridge exposed over HTTP to external agents (policy from settings).
    external: BrowserBridge,
    external_rx: UnboundedReceiver<CommandEnvelope>,
    /// Bridge used by the built-in assistant (always full access).
    internal: BrowserBridge,
    internal_rx: UnboundedReceiver<CommandEnvelope>,
    server: Option<HttpServerHandle>,
    pub clients: McpClientManager,
    pub pending_loads: Vec<PendingLoad>,
    assistant_rx: Option<UnboundedReceiver<AssistantEvent>>,
    cancel: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

fn policy(profile: &ProfileData) -> ToolPolicy {
    ToolPolicy {
        allow_page_control: profile.settings.mcp_server.allow_page_control,
        allow_data_access: profile.settings.mcp_server.allow_data_access,
    }
}

impl McpHost {
    pub fn start(rt: &tokio::runtime::Runtime, profile: &mut ProfileData, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (external, external_rx) = BrowserBridge::new(wake.clone(), policy(profile));
        let (internal, internal_rx) =
            BrowserBridge::new(wake.clone(), ToolPolicy { allow_page_control: true, allow_data_access: true });
        let mut host = Self {
            external,
            external_rx,
            internal,
            internal_rx,
            server: None,
            clients: McpClientManager::new(),
            pending_loads: Vec::new(),
            assistant_rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            wake,
        };
        host.start_server(rt, profile);
        host
    }

    fn start_server(&mut self, rt: &tokio::runtime::Runtime, profile: &mut ProfileData) {
        if !profile.settings.mcp_server.enabled {
            let _ = std::fs::remove_file(profile.paths.mcp_token());
            return;
        }
        let handler: Arc<dyn ToolHandler> = Arc::new(self.external.clone());
        match ferrosurf_mcp::spawn_http_server(rt.handle(), handler, profile.settings.mcp_server.port, None) {
            Ok(handle) => {
                log::info!("MCP server listening on {}", handle.url);
                let ep = EndpointFile { url: handle.url.clone(), token: handle.token.clone(), pid: std::process::id() };
                if let Err(e) = ep.write(&profile.paths.mcp_token()) {
                    log::error!("Could not write MCP endpoint file: {e}");
                }
                self.server = Some(handle);
            },
            Err(e) => log::error!("Could not start MCP server: {e}"),
        }
    }

    pub fn restart_server(&mut self, rt: &tokio::runtime::Runtime, profile: &mut ProfileData) {
        if let Some(s) = self.server.take() {
            s.shutdown();
        }
        self.external.set_policy(policy(profile));
        self.start_server(rt, profile);
    }

    pub fn url(&self) -> String {
        self.server.as_ref().map(|s| s.url.clone()).unwrap_or_default()
    }

    pub fn update_policy(&mut self, profile: &ProfileData) {
        self.external.set_policy(policy(profile));
    }

    /// Commands queued by either bridge.
    pub fn take_commands(&mut self) -> Vec<CommandEnvelope> {
        let mut out = Vec::new();
        while let Ok(c) = self.external_rx.try_recv() {
            out.push(c);
        }
        while let Ok(c) = self.internal_rx.try_recv() {
            out.push(c);
        }
        out
    }

    pub fn on_load_complete(&mut self, tab: TabId) {
        let mut i = 0;
        while i < self.pending_loads.len() {
            if self.pending_loads[i].tab == tab {
                let p = self.pending_loads.remove(i);
                let _ = p.reply.send(Ok(serde_json::json!({"loaded": true})));
            } else {
                i += 1;
            }
        }
    }

    pub fn expire_pending(&mut self) {
        let now = Instant::now();
        let mut i = 0;
        while i < self.pending_loads.len() {
            if self.pending_loads[i].deadline <= now {
                let p = self.pending_loads.remove(i);
                let _ = p.reply.send(Ok(serde_json::json!({"loaded": false, "timeout": true})));
            } else {
                i += 1;
            }
        }
    }

    /// (Re)connect the external MCP servers configured in settings and by extensions.
    pub fn connect_clients(&self, rt: &tokio::runtime::Runtime, profile: &ProfileData) {
        let mut configs: Vec<(String, ClientTransportConfig)> = profile
            .settings
            .mcp_servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| (s.name.clone(), transport(&s.transport)))
            .collect();
        for ext in profile.extensions.list().iter().filter(|e| e.enabled) {
            if let Some(fs) = &ext.manifest.ferrosurf {
                for v in &fs.mcp_servers {
                    if let Ok(cfg) = serde_json::from_value::<ferrosurf_core::settings::McpServerConfig>(v.clone()) {
                        configs.push((format!("{}-{}", ext.manifest.name, cfg.name), transport(&cfg.transport)));
                    }
                }
            }
        }
        let clients = self.clients.clone();
        let wake = self.wake.clone();
        rt.spawn(async move {
            clients.shutdown().await;
            clients.connect_all(configs).await;
            let _ = clients.all_tools().await;
            wake();
        });
    }

    pub fn start_assistant(
        &mut self,
        rt: &tokio::runtime::Runtime,
        profile: &ProfileData,
        shared: &mut SharedUi,
        text: String,
        page: Option<String>,
    ) {
        if shared.assistant.running {
            return;
        }
        let s = &profile.settings.assistant;
        let cfg = AssistantConfig {
            provider: match s.provider {
                AssistantProvider::Anthropic => Provider::Anthropic,
                AssistantProvider::OpenAiCompatible => Provider::OpenAiCompatible,
            },
            model: s.model.clone(),
            base_url: s.base_url.clone(),
            api_key: s.resolved_api_key(),
            max_tokens: s.max_tokens,
            system_prompt: if s.system_prompt.trim().is_empty() {
                AssistantConfig::default().system_prompt
            } else {
                s.system_prompt.clone()
            },
            max_tool_rounds: AssistantConfig::default().max_tool_rounds,
        };
        let mut tools: Vec<Arc<dyn ToolHandler>> = Vec::new();
        if s.browser_tools {
            tools.push(Arc::new(self.internal.clone()));
        }
        tools.push(Arc::new(self.clients.clone()));
        let history = shared.assistant.transcript.clone();
        shared.assistant.begin_turn(&text);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.assistant_rx = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let wake = self.wake.clone();
        // Wake the UI whenever an event is produced.
        let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<AssistantEvent>();
        rt.spawn(async move {
            while let Some(ev) = fwd_rx.recv().await {
                if tx.send(ev).is_err() {
                    break;
                }
                wake();
            }
        });
        rt.spawn(ferrosurf_mcp::assistant::run_turn(cfg, history, text, page, tools, fwd_tx, cancel));
    }

    pub fn stop_assistant(&mut self, shared: &mut SharedUi) {
        self.cancel.store(true, Ordering::SeqCst);
        shared.assistant.running = false;
    }

    /// Drain assistant events into the shared UI state. Returns true if anything changed.
    pub fn poll_assistant(&mut self, shared: &mut SharedUi) -> bool {
        let mut changed = false;
        if let Some(rx) = &mut self.assistant_rx {
            while let Ok(ev) = rx.try_recv() {
                shared.assistant.apply(ev);
                changed = true;
            }
        }
        let status = self.clients.status();
        if status.len() != shared.mcp_clients.len()
            || status.iter().zip(&shared.mcp_clients).any(|(a, b)| a.connected != b.connected || a.tool_count != b.tool_count)
        {
            shared.mcp_clients = status;
            changed = true;
        }
        changed
    }

    pub fn shutdown(&mut self, profile: &ProfileData) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(s) = self.server.take() {
            s.shutdown();
        }
        let _ = std::fs::remove_file(profile.paths.mcp_token());
    }
}

fn transport(t: &McpTransportConfig) -> ClientTransportConfig {
    match t {
        McpTransportConfig::Stdio { command, args, env } => {
            ClientTransportConfig::Stdio { command: command.clone(), args: args.clone(), env: env.clone() }
        },
        McpTransportConfig::Http { url, headers } => ClientTransportConfig::Http { url: url.clone(), headers: headers.clone() },
    }
}

/// Convert a JS evaluation result to JSON.
pub fn js_to_json(v: servo::JSValue) -> serde_json::Value {
    use serde_json::Value;
    match v {
        servo::JSValue::Undefined | servo::JSValue::Null => Value::Null,
        servo::JSValue::Boolean(b) => Value::Bool(b),
        servo::JSValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Value::from(n as i64)
            } else {
                serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
            }
        },
        servo::JSValue::String(s) => Value::String(s),
        servo::JSValue::Element(s) => serde_json::json!({"element": s}),
        servo::JSValue::ShadowRoot(s) => serde_json::json!({"shadowRoot": s}),
        servo::JSValue::Frame(s) => serde_json::json!({"frame": s}),
        servo::JSValue::Window(s) => serde_json::json!({"window": s}),
        servo::JSValue::Array(a) => Value::Array(a.into_iter().map(js_to_json).collect()),
        servo::JSValue::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, js_to_json(v))).collect()),
    }
}
