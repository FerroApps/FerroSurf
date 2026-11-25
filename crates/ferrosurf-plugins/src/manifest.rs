//! `plugin.toml`: identity, permissions, hooks, settings, commands, tools
//! and content scripts. Everything a plugin can do is declared here, so the
//! host can show it before running any plugin code.

use ferrosurf_extensions::MatchPattern;
use ferrosurf_plugin_sdk::abi::{ABI_VERSION, SettingValue};
use serde::{Deserialize, Serialize};

/// Largest `plugin.toml` we parse.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SETTINGS: usize = 32;
const MAX_COMMANDS: usize = 16;
const MAX_TOOLS: usize = 16;
const MAX_CONTENT: usize = 8;
const MAX_HOSTS: usize = 16;
const MAX_FILTERS: usize = 32;
const MAX_CHOICES: usize = 16;
const MAX_STRING_SETTING: usize = 4000;
const MAX_SCHEMA_BYTES: usize = 16 * 1024;
/// Manifest `events` names.
pub const EVENT_NAMES: [&str; 1] = ["navigation"];

/// A capability a plugin asks for. Host calls and hooks that need one fail
/// (`DENIED` / never invoked) unless the user granted it when enabling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// The `request` hook: block or redirect network requests.
    Requests,
    /// `navigation` events, `get_active_tab`, `open_url`.
    Tabs,
    /// `[[tools]]` exposed to the MCP server and the AI assistant.
    McpTools,
    /// Pages may call the plugin through `window.__ferrosurf.call`.
    PageMessages,
    /// Static content scripts and `inject_js`.
    PageScripting,
    /// `http_fetch` to `network_hosts`.
    Network,
    /// Private key-value store.
    Storage,
    /// `clipboard_write`.
    Clipboard,
    /// `notify`.
    Notifications,
}

impl Permission {
    pub const ALL: [Permission; 9] = [
        Self::Requests,
        Self::Tabs,
        Self::McpTools,
        Self::PageMessages,
        Self::PageScripting,
        Self::Network,
        Self::Storage,
        Self::Clipboard,
        Self::Notifications,
    ];

    /// What the permission prompt says (Spanish: user-facing).
    pub fn describe(self) -> &'static str {
        match self {
            Self::Requests => "Bloquear o redirigir peticiones de red de las páginas",
            Self::Tabs => "Ver la pestaña activa y sus navegaciones, y abrir pestañas",
            Self::McpTools => "Ofrecer herramientas al asistente de IA y al servidor MCP",
            Self::PageMessages => "Responder a mensajes de las páginas web indicadas",
            Self::PageScripting => "Ejecutar código en las páginas que visitas",
            Self::Network => "Conectarse a los servidores de internet indicados",
            Self::Storage => "Guardar un pequeño almacén privado en este equipo",
            Self::Clipboard => "Copiar texto al portapapeles cuando ejecutas uno de sus comandos",
            Self::Notifications => "Mostrar notificaciones del sistema",
        }
    }

    /// Permissions that act on pages or leave the machine.
    pub fn sensitive(self) -> bool {
        matches!(
            self,
            Self::Requests | Self::Tabs | Self::PageScripting | Self::Network | Self::McpTools
        )
    }
}

/// Type and constraints of one setting.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    Int { min: i64, max: i64 },
    Str { max_len: usize },
    Choice { choices: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub default: SettingValue,
}

impl SettingSpec {
    /// Ok if `v` fits this setting, else why not.
    pub fn check(&self, v: &SettingValue) -> Result<(), String> {
        match (&self.kind, v) {
            (SettingKind::Bool, SettingValue::Bool(_)) => Ok(()),
            (SettingKind::Int { min, max }, SettingValue::Int(i)) if (*min..=*max).contains(i) => {
                Ok(())
            }
            (SettingKind::Int { min, max }, SettingValue::Int(_)) => {
                Err(format!("debe estar entre {min} y {max}"))
            }
            (SettingKind::Str { max_len }, SettingValue::Str(s))
                if s.chars().count() <= *max_len =>
            {
                Ok(())
            }
            (SettingKind::Str { max_len }, SettingValue::Str(_)) => {
                Err(format!("como máximo {max_len} caracteres"))
            }
            (SettingKind::Choice { choices }, SettingValue::Str(s)) if choices.contains(s) => {
                Ok(())
            }
            _ => Err("tipo incorrecto".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
    pub shortcut: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunAt {
    DocumentStart,
    #[default]
    DocumentEnd,
    DocumentIdle,
}

impl RunAt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentStart => "document_start",
            Self::DocumentEnd => "document_end",
            Self::DocumentIdle => "document_idle",
        }
    }
}

/// A static content script entry (`[[content]]`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContentSpec {
    pub matches: Vec<String>,
    pub js: Option<String>,
    pub css: Option<String>,
    pub run_at: RunAt,
    pub all_frames: bool,
}

/// One `request_filters` entry: decides, without running plugin code,
/// whether the `request` hook is called for a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestFilter {
    /// `*`: every request.
    All,
    /// `host:example.com`: the host or any subdomain of it.
    Host(String),
    /// Anything else: a case-insensitive substring of the full URL.
    Substring(String),
}

impl RequestFilter {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "*" {
            return Ok(Self::All);
        }
        if let Some(h) = s.strip_prefix("host:") {
            let h = h.trim().to_ascii_lowercase();
            if !valid_host(&h) {
                return Err(format!("request filter {s:?}: bad host"));
            }
            return Ok(Self::Host(h));
        }
        if s.len() < 3 || s.len() > 128 || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(format!(
                "request filter {s:?}: use `*`, `host:<domain>` or a 3-128 character URL substring"
            ));
        }
        Ok(Self::Substring(s.to_ascii_lowercase()))
    }

    /// `host` and `url_lower` come from the same URL.
    pub fn matches(&self, host: &str, url_lower: &str) -> bool {
        match self {
            Self::All => true,
            Self::Host(h) => {
                host == h
                    || host
                        .strip_suffix(h.as_str())
                        .is_some_and(|p| p.ends_with('.'))
            }
            Self::Substring(s) => url_lower.contains(s.as_str()),
        }
    }
}

/// A validated manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub api_version: u32,
    /// Sorted, without duplicates.
    pub permissions: Vec<Permission>,
    /// Exact lowercase host names `http_fetch` may reach.
    pub network_hosts: Vec<String>,
    /// Hook subscriptions beyond what permissions imply (`navigation`).
    pub events: Vec<String>,
    pub request_filters: Vec<RequestFilter>,
    pub omnibox_keyword: Option<String>,
    /// Match patterns of the pages allowed to call `page_message`.
    pub page_message_origins: Vec<String>,
    pub settings: Vec<SettingSpec>,
    pub commands: Vec<CommandSpec>,
    pub tools: Vec<ToolSpec>,
    pub content: Vec<ContentSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    api_version: u32,
    #[serde(default)]
    permissions: Vec<Permission>,
    #[serde(default)]
    network_hosts: Vec<String>,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    request_filters: Vec<String>,
    omnibox_keyword: Option<String>,
    #[serde(default)]
    page_message_origins: Vec<String>,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    commands: Vec<RawCommand>,
    #[serde(default)]
    tools: Vec<RawTool>,
    #[serde(default)]
    content: Vec<RawContent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    description: String,
    default: toml::Value,
    min: Option<i64>,
    max: Option<i64>,
    max_len: Option<usize>,
    #[serde(default)]
    choices: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: String,
    title: String,
    shortcut: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    name: String,
    description: String,
    #[serde(default)]
    input_schema: Option<String>,
    #[serde(default)]
    read_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContent {
    matches: Vec<String>,
    js: Option<String>,
    css: Option<String>,
    run_at: Option<String>,
    #[serde(default)]
    all_frames: bool,
}

/// `[a-z0-9]` then `[a-z0-9]` plus `extra`, 1..=max bytes.
fn valid_ident(s: &str, max: usize, extra: &[char]) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

fn short(field: &str, s: &str, max: usize) -> Result<String, String> {
    let s = s.trim();
    if s.chars().count() > max {
        return Err(format!("`{field}` is longer than {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("`{field}` contains control characters"));
    }
    Ok(s.to_owned())
}

/// A bare DNS name: no scheme, port, path, wildcard or IP literal.
pub fn valid_host(h: &str) -> bool {
    h.len() <= 253
        && h.contains('.')
        && !h.starts_with('.')
        && !h.ends_with('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && !h
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
}

/// A relative path inside the plugin folder: no `..`, no absolute paths.
pub fn valid_asset_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && !p.starts_with('/')
        && !p.contains('\\')
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(':'))
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_MANIFEST_BYTES {
            return Err("plugin.toml is too large".into());
        }
        let raw: Raw = toml::from_str(text).map_err(|e| format!("plugin.toml: {}", e.message()))?;
        if !valid_ident(&raw.id, 64, &['.', '_', '-']) {
            return Err("`id` must be 1-64 characters of a-z, 0-9, '.', '_' or '-'".into());
        }
        if raw.api_version != ABI_VERSION {
            return Err(format!(
                "built for plugin API {}, this FerroSurf supports {ABI_VERSION}",
                raw.api_version
            ));
        }
        let name = short("name", &raw.name, 64)?;
        if name.is_empty() {
            return Err("`name` is empty".into());
        }
        let mut permissions = raw.permissions;
        permissions.sort();
        permissions.dedup();
        let has = |p| permissions.contains(&p);

        // Network.
        if raw.network_hosts.len() > MAX_HOSTS {
            return Err(format!("at most {MAX_HOSTS} `network_hosts`"));
        }
        let mut hosts = Vec::new();
        for h in &raw.network_hosts {
            if !valid_host(h) {
                return Err(format!(
                    "network host {h:?} must be a lowercase domain name (no scheme, port, IP or wildcard)"
                ));
            }
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
        if has(Permission::Network) != !hosts.is_empty() {
            return Err("the `network` permission and `network_hosts` go together".into());
        }

        // Events.
        let mut events = Vec::new();
        for e in &raw.events {
            if !EVENT_NAMES.contains(&e.as_str()) {
                return Err(format!("unknown event {e:?}"));
            }
            if !has(Permission::Tabs) {
                return Err(format!("event {e:?} needs the `tabs` permission"));
            }
            if !events.contains(e) {
                events.push(e.clone());
            }
        }

        // Request hook.
        if raw.request_filters.len() > MAX_FILTERS {
            return Err(format!("at most {MAX_FILTERS} `request_filters`"));
        }
        let mut request_filters = Vec::new();
        for f in &raw.request_filters {
            let f = RequestFilter::parse(f)?;
            if !request_filters.contains(&f) {
                request_filters.push(f);
            }
        }
        if has(Permission::Requests) != !request_filters.is_empty() {
            return Err(
                "the `requests` permission and `request_filters` go together (use `*` to see everything)"
                    .into(),
            );
        }

        // Omnibox.
        let omnibox_keyword = match raw.omnibox_keyword {
            Some(k) => {
                if !valid_ident(&k, 16, &['-']) {
                    return Err("`omnibox_keyword` must be 1-16 characters of a-z, 0-9, '-'".into());
                }
                Some(k)
            }
            None => None,
        };

        // Page messages.
        if raw.page_message_origins.len() > MAX_HOSTS {
            return Err(format!("at most {MAX_HOSTS} `page_message_origins`"));
        }
        for p in &raw.page_message_origins {
            MatchPattern::parse(p).map_err(|e| format!("page_message_origins {p:?}: {e}"))?;
        }
        if has(Permission::PageMessages) != !raw.page_message_origins.is_empty() {
            return Err(
                "the `page_messages` permission and `page_message_origins` go together".into(),
            );
        }

        // Settings.
        if raw.settings.len() > MAX_SETTINGS {
            return Err(format!("at most {MAX_SETTINGS} settings"));
        }
        let mut settings: Vec<SettingSpec> = Vec::new();
        for s in raw.settings {
            if !valid_ident(&s.key, 32, &['_']) || settings.iter().any(|o| o.key == s.key) {
                return Err(format!("bad or duplicate setting key {:?}", s.key));
            }
            let kind = match s.kind.as_str() {
                "bool" => SettingKind::Bool,
                "int" => {
                    let (min, max) = (s.min.unwrap_or(i64::MIN), s.max.unwrap_or(i64::MAX));
                    if min > max {
                        return Err(format!("setting {:?}: min > max", s.key));
                    }
                    SettingKind::Int { min, max }
                }
                "string" => SettingKind::Str {
                    max_len: s.max_len.unwrap_or(200).min(MAX_STRING_SETTING),
                },
                "choice" => {
                    if s.choices.is_empty() || s.choices.len() > MAX_CHOICES {
                        return Err(format!("setting {:?}: 1-{MAX_CHOICES} choices", s.key));
                    }
                    for c in &s.choices {
                        short("choice", c, 40)?;
                    }
                    SettingKind::Choice { choices: s.choices }
                }
                other => return Err(format!("setting {:?}: unknown type {other:?}", s.key)),
            };
            let default = match (&s.default, &kind) {
                (toml::Value::Boolean(b), SettingKind::Bool) => SettingValue::Bool(*b),
                (toml::Value::Integer(i), SettingKind::Int { .. }) => SettingValue::Int(*i),
                (toml::Value::String(t), SettingKind::Str { .. } | SettingKind::Choice { .. }) => {
                    SettingValue::Str(t.clone())
                }
                _ => return Err(format!("setting {:?}: `default` has the wrong type", s.key)),
            };
            let spec = SettingSpec {
                label: short("label", &s.label, 80)?,
                description: short("description", &s.description, 300)?,
                key: s.key,
                kind,
                default,
            };
            spec.check(&spec.default)
                .map_err(|e| format!("setting {:?}: default {e}", spec.key))?;
            settings.push(spec);
        }

        // Commands.
        if raw.commands.len() > MAX_COMMANDS {
            return Err(format!("at most {MAX_COMMANDS} commands"));
        }
        let mut commands: Vec<CommandSpec> = Vec::new();
        for c in raw.commands {
            if !valid_ident(&c.id, 48, &['_', '-', '.']) || commands.iter().any(|o| o.id == c.id) {
                return Err(format!("bad or duplicate command id {:?}", c.id));
            }
            let title = short("command title", &c.title, 64)?;
            if title.is_empty() {
                return Err(format!("command {:?} has no title", c.id));
            }
            let shortcut = match c.shortcut {
                Some(s) => {
                    let s = short("shortcut", &s, 32)?;
                    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '+') {
                        return Err(format!(
                            "command {:?}: shortcut must look like `Ctrl+Shift+K`",
                            c.id
                        ));
                    }
                    Some(s)
                }
                None => None,
            };
            commands.push(CommandSpec {
                id: c.id,
                title,
                shortcut,
            });
        }

        // Tools.
        if raw.tools.len() > MAX_TOOLS {
            return Err(format!("at most {MAX_TOOLS} tools"));
        }
        if has(Permission::McpTools) != !raw.tools.is_empty() {
            return Err("the `mcp_tools` permission and `[[tools]]` go together".into());
        }
        let mut tools: Vec<ToolSpec> = Vec::new();
        for t in raw.tools {
            if !valid_ident(&t.name, 40, &['_']) || tools.iter().any(|o| o.name == t.name) {
                return Err(format!("bad or duplicate tool name {:?}", t.name));
            }
            let description = short("tool description", &t.description, 1000)?;
            let schema_text = t.input_schema.as_deref().unwrap_or(r#"{"type":"object"}"#);
            if schema_text.len() > MAX_SCHEMA_BYTES {
                return Err(format!("tool {:?}: input_schema is too large", t.name));
            }
            let input_schema: serde_json::Value = serde_json::from_str(schema_text)
                .map_err(|e| format!("tool {:?}: input_schema is not JSON: {e}", t.name))?;
            if !input_schema.is_object() {
                return Err(format!(
                    "tool {:?}: input_schema must be a JSON object",
                    t.name
                ));
            }
            tools.push(ToolSpec {
                name: t.name,
                description,
                input_schema,
                read_only: t.read_only,
            });
        }

        // Content scripts.
        if raw.content.len() > MAX_CONTENT {
            return Err(format!("at most {MAX_CONTENT} `[[content]]` entries"));
        }
        if !raw.content.is_empty() && !has(Permission::PageScripting) {
            return Err("`[[content]]` needs the `page_scripting` permission".into());
        }
        let mut content = Vec::new();
        for c in raw.content {
            if c.matches.is_empty() || c.matches.len() > MAX_HOSTS {
                return Err("`[[content]]` needs 1-16 `matches`".into());
            }
            for m in &c.matches {
                MatchPattern::parse(m).map_err(|e| format!("content match {m:?}: {e}"))?;
            }
            if c.js.is_none() && c.css.is_none() {
                return Err("`[[content]]` needs `js` and/or `css`".into());
            }
            for (field, p, ext) in [("js", &c.js, ".js"), ("css", &c.css, ".css")] {
                if let Some(p) = p
                    && (!valid_asset_path(p) || !p.ends_with(ext))
                {
                    return Err(format!(
                        "content {field} {p:?} must be a relative path ending in {ext}"
                    ));
                }
            }
            let run_at = match c.run_at.as_deref() {
                None | Some("document_end") => RunAt::DocumentEnd,
                Some("document_start") => RunAt::DocumentStart,
                Some("document_idle") => RunAt::DocumentIdle,
                Some(o) => return Err(format!("unknown run_at {o:?}")),
            };
            content.push(ContentSpec {
                matches: c.matches,
                js: c.js,
                css: c.css,
                run_at,
                all_frames: c.all_frames,
            });
        }

        Ok(Self {
            id: raw.id,
            name,
            version: short("version", &raw.version, 32)?,
            author: short("author", &raw.author, 64)?,
            description: short("description", &raw.description, 300)?,
            api_version: raw.api_version,
            permissions,
            network_hosts: hosts,
            events,
            request_filters,
            omnibox_keyword,
            page_message_origins: raw.page_message_origins,
            settings,
            commands,
            tools,
            content,
        })
    }

    pub fn setting(&self, key: &str) -> Option<&SettingSpec> {
        self.settings.iter().find(|s| s.key == key)
    }

    pub fn subscribes_navigation(&self) -> bool {
        self.events.iter().any(|e| e == "navigation")
    }

    /// Name under which a tool is exposed through MCP:
    /// `plugin_<id with . and - as _>__<tool>`.
    pub fn mcp_tool_name(&self, tool: &str) -> String {
        let id: String = self
            .id
            .chars()
            .map(|c| if c == '.' || c == '-' { '_' } else { c })
            .collect();
        format!("plugin_{id}__{tool}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "id = \"p\"\nname = \"P\"\nversion = \"1\"\napi_version = 1\n";

    fn parse(extra: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("{BASE}{extra}"))
    }

    #[test]
    fn minimal_and_full_manifest() {
        let m = parse("").unwrap();
        assert!(m.permissions.is_empty() && m.commands.is_empty());
        let m = parse(
            r#"
permissions = ["requests", "tabs", "mcp_tools", "page_messages", "page_scripting", "network", "storage"]
network_hosts = ["api.example.com"]
events = ["navigation"]
request_filters = ["host:twitter.com", "youtube.com/watch"]
omnibox_keyword = "gh"
page_message_origins = ["https://*.example.com/*"]

[[settings]]
key = "host"
type = "string"
label = "Host"
default = "nitter.net"

[[commands]]
id = "save"
title = "Guardar"
shortcut = "Ctrl+Shift+K"

[[tools]]
name = "list_notes"
description = "Lists notes"
input_schema = '{"type":"object","properties":{}}'

[[content]]
matches = ["https://*.example.com/*"]
js = "inject.js"
css = "style.css"
run_at = "document_start"
"#,
        )
        .unwrap();
        assert_eq!(m.permissions.len(), 7);
        assert_eq!(m.request_filters.len(), 2);
        assert_eq!(m.mcp_tool_name("list_notes"), "plugin_p__list_notes");
        assert_eq!(m.content[0].run_at, RunAt::DocumentStart);
        assert!(m.subscribes_navigation());
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(parse("typo = 1").unwrap_err().contains("unknown field"));
        assert!(Manifest::parse(&BASE.replace("api_version = 1", "api_version = 9")).is_err());
        assert!(Manifest::parse(&BASE.replace("\"p\"", "\"P!\"")).is_err());
        // permission/declaration pairs
        assert!(parse("permissions = [\"requests\"]").is_err());
        assert!(parse("request_filters = [\"abc\"]").is_err());
        assert!(parse("permissions = [\"network\"]").is_err());
        assert!(parse("permissions = [\"network\"]\nnetwork_hosts = [\"http://x.com\"]").is_err());
        assert!(parse("permissions = [\"network\"]\nnetwork_hosts = [\"1.2.3.4\"]").is_err());
        assert!(parse("events = [\"navigation\"]").is_err());
        assert!(parse("events = [\"nope\"]\npermissions = [\"tabs\"]").is_err());
        assert!(parse("permissions = [\"mcp_tools\"]").is_err());
        assert!(parse("permissions = [\"page_messages\"]").is_err());
        assert!(parse("[[content]]\nmatches = [\"https://a.com/*\"]\njs = \"a.js\"").is_err());
        // content paths
        let c = |js: &str| {
            parse(&format!(
                "permissions = [\"page_scripting\"]\n[[content]]\nmatches = [\"https://a.com/*\"]\njs = \"{js}\""
            ))
        };
        assert!(c("a.js").is_ok());
        assert!(c("../a.js").is_err());
        assert!(c("/etc/a.js").is_err());
        assert!(c("a.txt").is_err());
        // filters, keyword, tools, commands
        assert!(parse("permissions=[\"requests\"]\nrequest_filters=[\"host:bad host\"]").is_err());
        assert!(parse("omnibox_keyword = \"A B\"").is_err());
        assert!(
            parse("permissions=[\"mcp_tools\"]\n[[tools]]\nname=\"t\"\ndescription=\"d\"\ninput_schema=\"[1]\"")
                .is_err()
        );
        assert!(parse("[[commands]]\nid=\"a\"\ntitle=\"A\"\nshortcut=\"Ctrl + K\"").is_err());
        assert!(
            parse("[[commands]]\nid=\"a\"\ntitle=\"A\"\n[[commands]]\nid=\"a\"\ntitle=\"B\"")
                .is_err()
        );
        // setting default must fit
        assert!(
            parse("[[settings]]\nkey=\"k\"\ntype=\"int\"\nlabel=\"K\"\ndefault=5\nmax=3").is_err()
        );
    }

    #[test]
    fn request_filters_match() {
        let host = RequestFilter::parse("host:twitter.com").unwrap();
        assert!(host.matches("twitter.com", "https://twitter.com/x"));
        assert!(host.matches("mobile.twitter.com", "https://mobile.twitter.com/"));
        assert!(!host.matches("nottwitter.com", "https://nottwitter.com/"));
        let sub = RequestFilter::parse("/WATCH?v=").unwrap();
        assert!(sub.matches("youtube.com", "https://youtube.com/watch?v=1"));
        assert!(RequestFilter::All.matches("a.b", "x"));
    }
}
