//! MCP client: use external MCP servers (stdio or streamable HTTP).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::jsonrpc::{self, RpcError};
use crate::server::{BoxFuture, Content, ToolDef, ToolHandler, ToolOutput, LATEST_PROTOCOL_VERSION};
use crate::sse::SseParser;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientTransportConfig {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Http { url: String, headers: BTreeMap<String, String> },
}

trait Transport: Send + Sync {
    fn request(&self, method: &str, params: Value) -> BoxFuture<'_, Result<Value>>;
    fn notify(&self, method: &str, params: Value) -> BoxFuture<'_, Result<()>>;
    fn close(&self) -> BoxFuture<'_, ()>;
    fn set_protocol_version(&self, _v: &str) {}
}

pub struct McpClient {
    transport: Box<dyn Transport>,
    server_info: Value,
    protocol_version: String,
}

impl McpClient {
    /// Connect and run the initialize handshake.
    pub async fn connect(config: ClientTransportConfig) -> Result<McpClient> {
        let transport: Box<dyn Transport> = match config {
            ClientTransportConfig::Stdio { command, args, env } => {
                Box::new(StdioTransport::spawn(&command, &args, &env)?)
            }
            ClientTransportConfig::Http { url, headers } => Box::new(HttpTransport::new(url, headers)?),
        };
        Self::handshake(transport).await
    }

    /// Run a client over arbitrary newline-delimited JSON streams (used by tests and embedders).
    pub async fn from_io<R, W>(reader: R, writer: W) -> Result<McpClient>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::handshake(Box::new(StdioTransport::new(reader, writer, None))).await
    }

    async fn handshake(transport: Box<dyn Transport>) -> Result<McpClient> {
        let res = transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "FerroSurf", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await
            .context("initialize failed")?;
        let protocol_version = res
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(LATEST_PROTOCOL_VERSION)
            .to_string();
        transport.set_protocol_version(&protocol_version);
        transport.notify("notifications/initialized", json!({})).await.context("initialized notification failed")?;
        Ok(McpClient { transport, server_info: res.get("serverInfo").cloned().unwrap_or(Value::Null), protocol_version })
    }

    pub fn server_info(&self) -> &Value {
        &self.server_info
    }
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolDef>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let res = self.transport.request("tools/list", params).await?;
            if let Some(arr) = res.get("tools").and_then(Value::as_array) {
                out.extend(arr.iter().filter_map(parse_tool_def));
            }
            match res.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => return Ok(out),
            }
        }
        bail!("tools/list pagination did not terminate")
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput> {
        let args = if args.is_null() { json!({}) } else { args };
        let res = self.transport.request("tools/call", json!({"name": name, "arguments": args})).await?;
        Ok(parse_tool_output(&res))
    }

    pub async fn close(&self) {
        self.transport.close().await;
    }
}

fn parse_tool_def(v: &Value) -> Option<ToolDef> {
    let name = v.get("name")?.as_str()?.to_string();
    let ann = v.get("annotations");
    let read_only = ann.and_then(|a| a.get("readOnlyHint")).and_then(Value::as_bool).unwrap_or(false);
    let destructive =
        !read_only && ann.and_then(|a| a.get("destructiveHint")).and_then(Value::as_bool).unwrap_or(true);
    Some(ToolDef {
        name,
        title: v.get("title").and_then(Value::as_str).map(String::from),
        description: v.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
        input_schema: v.get("inputSchema").cloned().unwrap_or_else(|| json!({"type":"object"})),
        read_only,
        destructive,
    })
}

pub fn parse_tool_output(res: &Value) -> ToolOutput {
    let mut content = Vec::new();
    if let Some(arr) = res.get("content").and_then(Value::as_array) {
        for c in arr {
            match c.get("type").and_then(Value::as_str) {
                Some("text") => content.push(Content::Text(c.get("text").and_then(Value::as_str).unwrap_or("").to_string())),
                Some("image") => content.push(Content::Image {
                    data_base64: c.get("data").and_then(Value::as_str).unwrap_or("").to_string(),
                    mime: c.get("mimeType").and_then(Value::as_str).unwrap_or("image/png").to_string(),
                }),
                Some("resource") => {
                    let r = &c["resource"];
                    match r.get("text").and_then(Value::as_str) {
                        Some(t) => content.push(Content::Text(t.to_string())),
                        None => content.push(Content::Text(format!("[resource: {}]", r.get("uri").and_then(Value::as_str).unwrap_or("?")))),
                    }
                }
                Some("resource_link") => content.push(Content::Text(format!(
                    "[resource link: {}]",
                    c.get("uri").and_then(Value::as_str).unwrap_or("?")
                ))),
                Some(other) => content.push(Content::Text(format!("[unsupported {other} content]"))),
                None => {}
            }
        }
    }
    let structured = res.get("structuredContent").cloned().filter(|v| !v.is_null());
    if let (true, Some(s)) = (content.is_empty(), &structured) {
        content.push(Content::Text(s.to_string()));
    }
    ToolOutput { content, is_error: res.get("isError").and_then(Value::as_bool).unwrap_or(false), structured }
}

// ---------------------------------------------------------------------------
// stdio transport
// ---------------------------------------------------------------------------

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;
type SharedWriter = Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

struct StdioTransport {
    writer: SharedWriter,
    pending: Pending,
    next_id: AtomicI64,
    closed: Arc<AtomicBool>,
    reader_task: tokio::task::JoinHandle<()>,
    child: Mutex<Option<tokio::process::Child>>,
}

impl StdioTransport {
    fn spawn(command: &str, args: &[String], env: &BTreeMap<String, String>) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("failed to spawn MCP server '{command}'"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        if let Some(stderr) = child.stderr.take() {
            let name = command.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    log::info!("[mcp:{name}] {l}");
                }
            });
        }
        Ok(Self::new(stdout, stdin, Some(child)))
    }

    fn new<R, W>(reader: R, writer: W, child: Option<tokio::process::Child>) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let writer: SharedWriter = Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let reader_task = tokio::spawn(read_loop(reader, writer.clone(), pending.clone(), closed.clone()));
        Self { writer, pending, next_id: AtomicI64::new(1), closed, reader_task, child: Mutex::new(child) }
    }

    async fn write_msg(&self, msg: &Value) -> Result<()> {
        write_line(&self.writer, msg).await
    }
}

async fn write_line(w: &SharedWriter, msg: &Value) -> Result<()> {
    let mut w = w.lock().await;
    let mut line = msg.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await.context("write to MCP server failed")?;
    w.flush().await?;
    Ok(())
}

async fn read_loop<R: AsyncRead + Unpin>(reader: R, writer: SharedWriter, pending: Pending, closed: Arc<AtomicBool>) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            _ => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            log::debug!("mcp client: non-JSON line from server: {line}");
            continue;
        };
        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            // Server -> client request or notification.
            if let Some(id) = msg.get("id").filter(|i| !i.is_null()) {
                let resp = if method == "ping" {
                    jsonrpc::Response::ok(id.clone(), json!({}))
                } else {
                    jsonrpc::Response::err(id.clone(), jsonrpc::METHOD_NOT_FOUND, format!("method not found: {method}"))
                };
                let _ = write_line(&writer, &resp.to_value()).await;
            }
            continue;
        }
        let Some(id) = msg.get("id").and_then(Value::as_i64) else { continue };
        let Some(tx) = pending.lock().remove(&id) else { continue };
        let result = match msg.get("error") {
            Some(e) => Err(serde_json::from_value::<RpcError>(e.clone())
                .unwrap_or_else(|_| RpcError::new(jsonrpc::INTERNAL_ERROR, e.to_string()))),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }
    closed.store(true, Ordering::SeqCst);
    // Dropping the senders wakes every waiter with a "closed" error.
    pending.lock().clear();
}

impl Transport for StdioTransport {
    fn request(&self, method: &str, params: Value) -> BoxFuture<'_, Result<Value>> {
        let method = method.to_string();
        Box::pin(async move {
            if self.closed.load(Ordering::SeqCst) {
                bail!("MCP server connection closed");
            }
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = oneshot::channel();
            self.pending.lock().insert(id, tx);
            let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            if let Err(e) = self.write_msg(&msg).await {
                self.pending.lock().remove(&id);
                return Err(e);
            }
            match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
                Ok(Ok(Ok(v))) => Ok(v),
                Ok(Ok(Err(e))) => Err(e.into()),
                Ok(Err(_)) => bail!("MCP server connection closed"),
                Err(_) => {
                    self.pending.lock().remove(&id);
                    bail!("MCP request '{method}' timed out")
                }
            }
        })
    }

    fn notify(&self, method: &str, params: Value) -> BoxFuture<'_, Result<()>> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        Box::pin(async move { self.write_msg(&msg).await })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.closed.store(true, Ordering::SeqCst);
            let _ = self.writer.lock().await.shutdown().await;
            let child = self.child.lock().take();
            if let Some(mut child) = child
                && tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err()
            {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            self.reader_task.abort();
        })
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

// ---------------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------------

struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    next_id: AtomicI64,
}

impl HttpTransport {
    fn new(url: String, headers: BTreeMap<String, String>) -> Result<Self> {
        url::Url::parse(&url).with_context(|| format!("invalid MCP server URL: {url}"))?;
        Ok(Self {
            client: crate::util::http_client_for(&url),
            url,
            headers,
            session: Mutex::new(None),
            protocol: Mutex::new(None),
            next_id: AtomicI64::new(1),
        })
    }

    fn builder(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .request(method, &self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        if let Some(s) = self.session.lock().clone() {
            rb = rb.header("Mcp-Session-Id", s);
        }
        if let Some(p) = self.protocol.lock().clone() {
            rb = rb.header("MCP-Protocol-Version", p);
        }
        rb
    }

    async fn post(&self, msg: &Value, want_id: Option<i64>) -> Result<Option<Value>> {
        let resp = self.builder(reqwest::Method::POST).body(msg.to_string()).send().await.context("HTTP request to MCP server failed")?;
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock() = Some(s.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("MCP server returned HTTP {status}: {}", crate::util::truncate_chars(&body, 300));
        }
        let Some(id) = want_id else {
            return Ok(None); // notification: body (if any) is irrelevant
        };
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false);
        if !is_sse {
            let text = resp.text().await?;
            let v: Value = serde_json::from_str(&text).with_context(|| "invalid JSON from MCP server")?;
            return find_response(&v, id).map(Some).ok_or_else(|| anyhow!("no response with id {id} in reply"));
        }
        let mut parser = SseParser::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("error reading SSE stream")?;
            for ev in parser.push(&chunk) {
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data)
                    && let Some(r) = find_response(&v, id)
                {
                    return Ok(Some(r));
                }
            }
        }
        for ev in parser.finish() {
            if let Ok(v) = serde_json::from_str::<Value>(&ev.data)
                && let Some(r) = find_response(&v, id)
            {
                return Ok(Some(r));
            }
        }
        bail!("SSE stream ended without a response to request {id}")
    }
}

/// Find the JSON-RPC response with `id` in a message or batch.
fn find_response(v: &Value, id: i64) -> Option<Value> {
    match v {
        Value::Array(a) => a.iter().find_map(|x| find_response(x, id)),
        _ if v.get("id").and_then(Value::as_i64) == Some(id) && v.get("method").is_none() => Some(v.clone()),
        _ => None,
    }
}

impl Transport for HttpTransport {
    fn request(&self, method: &str, params: Value) -> BoxFuture<'_, Result<Value>> {
        let method = method.to_string();
        Box::pin(async move {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            let reply = tokio::time::timeout(REQUEST_TIMEOUT, self.post(&msg, Some(id)))
                .await
                .map_err(|_| anyhow!("MCP request '{method}' timed out"))??
                .ok_or_else(|| anyhow!("empty reply"))?;
            if let Some(e) = reply.get("error") {
                let e = serde_json::from_value::<RpcError>(e.clone())
                    .unwrap_or_else(|_| RpcError::new(jsonrpc::INTERNAL_ERROR, e.to_string()));
                return Err(e.into());
            }
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        })
    }

    fn notify(&self, method: &str, params: Value) -> BoxFuture<'_, Result<()>> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        Box::pin(async move { self.post(&msg, None).await.map(|_| ()) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.session.lock().is_some() {
                let _ = tokio::time::timeout(Duration::from_secs(3), self.builder(reqwest::Method::DELETE).send()).await;
            }
        })
    }

    fn set_protocol_version(&self, v: &str) {
        *self.protocol.lock() = Some(v.to_string());
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub name: String,
    pub connected: bool,
    pub error: Option<String>,
    pub tool_count: usize,
}

/// Sanitize to `[a-zA-Z0-9_-]`.
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// `"{server}__{tool}"`, sanitized to `[a-zA-Z0-9_-]` and capped at 64 chars.
pub fn namespaced_tool_name(server: &str, tool: &str) -> String {
    let mut n = format!("{}__{}", sanitize(server), sanitize(tool));
    n.truncate(64);
    n
}

struct Entry {
    client: Option<Arc<McpClient>>,
    error: Option<String>,
    tools: Vec<ToolDef>,
}

#[derive(Default)]
struct Snapshot {
    defs: Vec<ToolDef>,
    /// namespaced -> (server, original tool name)
    routes: HashMap<String, (String, String)>,
}

#[derive(Default)]
struct Inner {
    entries: RwLock<BTreeMap<String, Entry>>,
    snapshot: RwLock<Snapshot>,
}

#[derive(Clone, Default)]
pub struct McpClientManager {
    inner: Arc<Inner>,
}

impl McpClientManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect every server concurrently; failures are recorded per server.
    pub async fn connect_all(&self, configs: Vec<(String, ClientTransportConfig)>) {
        let futs = configs.into_iter().map(|(name, cfg)| async move {
            let res: Result<(McpClient, Vec<ToolDef>)> = async {
                let c = tokio::time::timeout(Duration::from_secs(60), McpClient::connect(cfg))
                    .await
                    .map_err(|_| anyhow!("connection timed out"))??;
                let tools = c.list_tools().await?;
                Ok((c, tools))
            }
            .await;
            (name, res)
        });
        for (name, res) in futures_util::future::join_all(futs).await {
            let entry = match res {
                Ok((c, tools)) => Entry { client: Some(Arc::new(c)), error: None, tools },
                Err(e) => {
                    log::warn!("MCP server '{name}' failed to connect: {e:#}");
                    Entry { client: None, error: Some(format!("{e:#}")), tools: vec![] }
                }
            };
            let old = self.inner.entries.write().insert(name, entry);
            if let Some(c) = old.and_then(|e| e.client) {
                c.close().await;
            }
        }
        self.rebuild();
    }

    /// Close and forget all servers.
    pub async fn shutdown(&self) {
        let entries = std::mem::take(&mut *self.inner.entries.write());
        for (_, e) in entries {
            if let Some(c) = e.client {
                c.close().await;
            }
        }
        self.rebuild();
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.inner
            .entries
            .read()
            .iter()
            .map(|(name, e)| ServerStatus {
                name: name.clone(),
                connected: e.client.is_some(),
                error: e.error.clone(),
                tool_count: e.tools.len(),
            })
            .collect()
    }

    /// Refresh each connected server's tool list, then return all namespaced tools.
    pub async fn all_tools(&self) -> Vec<ToolDef> {
        let clients: Vec<(String, Arc<McpClient>)> = self
            .inner
            .entries
            .read()
            .iter()
            .filter_map(|(n, e)| e.client.clone().map(|c| (n.clone(), c)))
            .collect();
        let results = futures_util::future::join_all(clients.into_iter().map(|(n, c)| async move {
            (n, c.list_tools().await)
        }))
        .await;
        {
            let mut entries = self.inner.entries.write();
            for (n, r) in results {
                if let (Some(e), Ok(tools)) = (entries.get_mut(&n), r) {
                    e.tools = tools;
                }
            }
        }
        self.rebuild();
        self.inner.snapshot.read().defs.clone()
    }

    pub async fn call(&self, namespaced: &str, args: Value) -> ToolOutput {
        let route = self.inner.snapshot.read().routes.get(namespaced).cloned();
        let Some((server, tool)) = route else {
            return ToolOutput::error(format!("Unknown MCP tool: {namespaced}"));
        };
        let client = self.inner.entries.read().get(&server).and_then(|e| e.client.clone());
        let Some(client) = client else {
            return ToolOutput::error(format!("MCP server '{server}' is not connected"));
        };
        match client.call_tool(&tool, args).await {
            Ok(o) => o,
            Err(e) => ToolOutput::error(format!("MCP tool call failed: {e:#}")),
        }
    }

    fn rebuild(&self) {
        let entries = self.inner.entries.read();
        let mut snap = Snapshot::default();
        for (server, e) in entries.iter() {
            for t in &e.tools {
                let full = format!("{}__{}", sanitize(server), sanitize(&t.name));
                let mut name = full.chars().take(64).collect::<String>();
                if full.chars().count() > 64 || snap.routes.contains_key(&name) {
                    let h = format!("{:08x}", crate::util::fnv1a(&format!("{server}\u{0}{}", t.name)));
                    name = format!("{}_{h}", full.chars().take(64 - 9).collect::<String>());
                }
                snap.routes.insert(name.clone(), (server.clone(), t.name.clone()));
                let mut def = t.clone();
                def.name = name;
                if def.title.is_none() {
                    def.title = Some(t.name.clone());
                }
                def.description = format!("[{server}] {}", t.description);
                snap.defs.push(def);
            }
        }
        drop(entries);
        *self.inner.snapshot.write() = snap;
    }
}

impl ToolHandler for McpClientManager {
    fn tools(&self) -> Vec<ToolDef> {
        self.inner.snapshot.read().defs.clone()
    }

    fn call(&self, name: &str, args: Value) -> BoxFuture<'static, ToolOutput> {
        let this = self.clone();
        let name = name.to_string();
        Box::pin(async move { McpClientManager::call(&this, &name, args).await })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::Dummy;
    use crate::server::{spawn_http_server, McpServer};
    use tokio::io::{duplex, AsyncBufReadExt, BufReader};

    /// Serve `McpServer` over an in-memory duplex pipe, newline-delimited, with optional extras.
    fn serve_duplex(handler: Arc<dyn ToolHandler>, paginate: bool, chatty: bool) -> McpClientIo {
        let (c_r, s_w) = duplex(1 << 16);
        let (s_r, c_w) = duplex(1 << 16);
        let server = McpServer::new(handler.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(s_r).lines();
            let mut s_w = s_w;
            while let Ok(Some(line)) = lines.next_line().await {
                let msg: Value = serde_json::from_str(&line).unwrap();
                if chatty && msg["method"] == "initialize" {
                    // Server-initiated ping and a notification before replying.
                    s_w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n").await.unwrap();
                    s_w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"srv-1\",\"method\":\"ping\"}\n").await.unwrap();
                }
                let reply = if paginate && msg["method"] == "tools/list" {
                    let tools: Vec<Value> = handler.tools().iter().map(ToolDef::to_json).collect();
                    let cursor = msg["params"]["cursor"].as_str();
                    let (page, next) = match cursor {
                        None => (tools[..1].to_vec(), Some("c1")),
                        Some("c1") => (tools[1..2].to_vec(), Some("c2")),
                        _ => (tools[2..].to_vec(), None),
                    };
                    let mut r = json!({"tools": page});
                    if let Some(n) = next { r["nextCursor"] = json!(n); }
                    Some(json!({"jsonrpc":"2.0","id":msg["id"],"result":r}))
                } else {
                    server.handle(msg).await
                };
                if let Some(r) = reply {
                    s_w.write_all(format!("{r}\n").as_bytes()).await.unwrap();
                }
            }
        });
        McpClientIo { r: c_r, w: c_w }
    }
    struct McpClientIo { r: tokio::io::DuplexStream, w: tokio::io::DuplexStream }

    #[tokio::test]
    async fn stdio_client_against_fake_server() {
        let io = serve_duplex(Arc::new(Dummy), false, true);
        let c = McpClient::from_io(io.r, io.w).await.unwrap();
        assert_eq!(c.server_info()["name"], "FerroSurf");
        assert_eq!(c.protocol_version(), "2025-06-18");
        let tools = c.list_tools().await.unwrap();
        assert_eq!(tools.len(), 3);
        assert!(tools[0].read_only && !tools[0].destructive);
        let out = c.call_tool("echo", json!({"text":"x"})).await.unwrap();
        assert_eq!(out.text_content(), "echo:x");
        let out = c.call_tool("boom", json!({})).await.unwrap();
        assert!(out.is_error);
        let out = c.call_tool("pic", json!({})).await.unwrap();
        assert_eq!(out.content, vec![Content::Image { data_base64: "AAAA".into(), mime: "image/png".into() }]);
        // protocol error surfaces as Err
        assert!(c.call_tool("missing", json!({})).await.is_err());
        c.close().await;
        assert!(c.list_tools().await.is_err());
    }

    #[tokio::test]
    async fn pagination_is_followed() {
        let io = serve_duplex(Arc::new(Dummy), true, false);
        let c = McpClient::from_io(io.r, io.w).await.unwrap();
        let names: Vec<_> = c.list_tools().await.unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["echo", "boom", "pic"]);
    }

    #[tokio::test]
    async fn server_eof_fails_pending() {
        let (c_r, mut s_w) = duplex(4096);
        let (s_r, c_w) = duplex(4096);
        tokio::spawn(async move {
            let mut lines = BufReader::new(s_r).lines();
            // answer initialize, then die on next request
            let l = lines.next_line().await.unwrap().unwrap();
            let id = serde_json::from_str::<Value>(&l).unwrap()["id"].clone();
            s_w.write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18"}})).as_bytes()).await.unwrap();
            let _ = lines.next_line().await; // initialized
            let _ = lines.next_line().await; // tools/list
            drop(s_w);
        });
        let c = McpClient::from_io(c_r, c_w).await.unwrap();
        let err = c.list_tools().await.unwrap_err();
        assert!(format!("{err:#}").contains("closed"), "{err:#}");
    }

    #[tokio::test]
    async fn http_client_against_our_server() {
        let h = spawn_http_server(&tokio::runtime::Handle::current(), Arc::new(Dummy), 0, None).unwrap();
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), format!("Bearer {}", h.token));
        let c = McpClient::connect(ClientTransportConfig::Http { url: h.url.clone(), headers }).await.unwrap();
        assert_eq!(c.list_tools().await.unwrap().len(), 3);
        let out = c.call_tool("echo", json!({"text":"http"})).await.unwrap();
        assert_eq!(out.text_content(), "echo:http");
        c.close().await;

        // wrong token => connect error
        let bad = McpClient::connect(ClientTransportConfig::Http { url: h.url.clone(), headers: BTreeMap::new() }).await;
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn http_client_parses_sse_replies() {
        use axum::body::Body;
        use axum::response::Response;
        let app = axum::Router::new().fallback(|body: axum::body::Bytes| async move {
            let msg: Value = serde_json::from_slice(&body).unwrap();
            let Some(id) = msg.get("id").cloned() else {
                return Response::builder().status(202).body(Body::empty()).unwrap();
            };
            let result = match msg["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"sse"}}),
                "tools/list" => json!({"tools":[{"name":"t","description":"d","inputSchema":{"type":"object"}}]}),
                _ => json!({"content":[{"type":"text","text":"via sse"}]}),
            };
            let body = format!(
                "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}}),
                json!({"jsonrpc":"2.0","id":id,"result":result})
            );
            Response::builder().header("content-type", "text/event-stream").body(Body::from(body)).unwrap()
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = McpClient::connect(ClientTransportConfig::Http { url, headers: BTreeMap::new() }).await.unwrap();
        assert_eq!(c.server_info()["name"], "sse");
        assert_eq!(c.list_tools().await.unwrap()[0].name, "t");
        assert_eq!(c.call_tool("t", json!({})).await.unwrap().text_content(), "via sse");
    }

    #[tokio::test]
    async fn stdio_spawn_failure_is_error() {
        let r = McpClient::connect(ClientTransportConfig::Stdio {
            command: "/nonexistent/definitely-not-a-binary".into(),
            args: vec![],
            env: BTreeMap::new(),
        })
        .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn stdio_real_process() {
        // A tiny scripted server in sh: answers initialize and one tools/list.
        let script = r#"read l; id=$(echo "$l" | sed 's/.*"id":\([0-9]*\).*/\1/'); echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-06-18\",\"serverInfo\":{\"name\":\"sh\"}}}"; read l2; read l3; id=$(echo "$l3" | sed 's/.*"id":\([0-9]*\).*/\1/'); echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[]}}"; read l4"#;
        let c = McpClient::connect(ClientTransportConfig::Stdio {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: BTreeMap::new(),
        })
        .await
        .unwrap();
        assert_eq!(c.server_info()["name"], "sh");
        assert!(c.list_tools().await.unwrap().is_empty());
        c.close().await;
    }

    #[test]
    fn namespacing() {
        assert_eq!(namespaced_tool_name("my server", "do.thing"), "my_server__do_thing");
        let long = namespaced_tool_name(&"s".repeat(40), &"t".repeat(40));
        assert_eq!(long.len(), 64);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[tokio::test]
    async fn manager_end_to_end() {
        let h = spawn_http_server(&tokio::runtime::Handle::current(), Arc::new(Dummy), 0, None).unwrap();
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), format!("Bearer {}", h.token));
        let m = McpClientManager::new();
        m.connect_all(vec![
            ("good srv".into(), ClientTransportConfig::Http { url: h.url.clone(), headers }),
            ("bad".into(), ClientTransportConfig::Stdio { command: "/nonexistent/x".into(), args: vec![], env: BTreeMap::new() }),
        ])
        .await;
        let st = m.status();
        assert_eq!(st.len(), 2);
        let bad = st.iter().find(|s| s.name == "bad").unwrap();
        assert!(!bad.connected && bad.error.is_some() && bad.tool_count == 0);
        let good = st.iter().find(|s| s.name == "good srv").unwrap();
        assert!(good.connected && good.error.is_none() && good.tool_count == 3);

        let tools = m.all_tools().await;
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["good_srv__echo", "good_srv__boom", "good_srv__pic"]);
        assert_eq!(ToolHandler::tools(&m).len(), 3);

        let out = ToolHandler::call(&m, "good_srv__echo", json!({"text":"m"})).await;
        assert_eq!(out.text_content(), "echo:m");
        assert!(m.call("nope__x", json!({})).await.is_error);
        m.shutdown().await;
        assert!(m.status().is_empty());
    }

    #[test]
    fn long_and_colliding_names_are_unique() {
        let m = McpClientManager::new();
        let mk = |n: &str| ToolDef { name: n.into(), title: None, description: String::new(), input_schema: json!({}), read_only: false, destructive: false };
        let long_a = format!("{}a", "x".repeat(70));
        let long_b = format!("{}b", "x".repeat(70));
        m.inner.entries.write().insert("s".into(), Entry { client: None, error: None, tools: vec![mk(&long_a), mk(&long_b), mk("a.b"), mk("a_b")] });
        m.rebuild();
        let defs = ToolHandler::tools(&m);
        let set: std::collections::HashSet<_> = defs.iter().map(|d| d.name.clone()).collect();
        assert_eq!(set.len(), 4);
        assert!(defs.iter().all(|d| d.name.len() <= 64));
        // routes recover the original (unsanitized) tool names
        let snap = m.inner.snapshot.read();
        let orig: std::collections::HashSet<_> = snap.routes.values().map(|(_, t)| t.clone()).collect();
        assert!(orig.contains("a.b") && orig.contains(&long_a));
    }

    #[test]
    fn tool_output_parsing() {
        let o = parse_tool_output(&json!({"content":[{"type":"text","text":"a"},{"type":"resource_link","uri":"file:///x"},{"type":"audio","data":"","mimeType":"a/b"}],"isError":true,"structuredContent":{"k":1}}));
        assert!(o.is_error);
        assert_eq!(o.content.len(), 3);
        assert_eq!(o.structured, Some(json!({"k":1})));
        let o = parse_tool_output(&json!({"structuredContent":{"k":1}}));
        assert_eq!(o.text_content(), "{\"k\":1}");
    }
}
