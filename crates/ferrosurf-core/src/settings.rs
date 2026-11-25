//! Per-profile user settings. Every field has a default so that older/newer settings
//! files always load.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::search::SearchEngine;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TabLayout {
    /// Classic tab strip on top.
    #[default]
    Horizontal,
    /// Vertical tab sidebar on the left (Edge/Arc style).
    Vertical,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StartupBehavior {
    NewTab,
    #[default]
    RestoreSession,
    Homepage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: ThemeMode,
    /// Accent color `#rrggbb`. Empty → use profile color.
    pub accent: String,
    pub density: Density,
    pub tab_layout: TabLayout,
    pub show_bookmarks_bar: bool,
    pub show_home_button: bool,
    /// Global UI scale multiplier applied on top of the OS scale factor.
    pub ui_scale: f32,
    pub corner_radius: f32,
    /// Show the status/link-preview bubble at the bottom.
    pub show_status_bubble: bool,
    /// Hide the toolbar in fullscreen / compact mode.
    pub compact_mode: bool,
    /// Ask web content to render dark (prefers-color-scheme).
    pub web_dark_mode: bool,
    pub ui_font_size: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            accent: String::new(),
            density: Density::Comfortable,
            tab_layout: TabLayout::Horizontal,
            show_bookmarks_bar: true,
            show_home_button: false,
            ui_scale: 1.0,
            corner_radius: 8.0,
            show_status_bubble: true,
            compact_mode: false,
            web_dark_mode: false,
            ui_font_size: 13.5,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Privacy {
    /// Block ads and trackers with the built-in content blocker.
    pub block_trackers: bool,
    /// Upgrade http:// navigations to https://.
    pub https_only: bool,
    pub do_not_track: bool,
    /// Send search-suggestion requests while typing in the address bar.
    pub search_suggestions: bool,
    pub clear_history_on_exit: bool,
    pub clear_cookies_on_exit: bool,
    pub remember_history: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            block_trackers: true,
            https_only: false,
            do_not_track: true,
            search_suggestions: true,
            clear_history_on_exit: false,
            clear_cookies_on_exit: false,
            remember_history: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Downloads {
    pub directory: PathBuf,
    pub ask_where_to_save: bool,
    pub open_panel_on_start: bool,
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            directory: crate::paths::default_download_dir(),
            ask_where_to_save: false,
            open_panel_on_start: true,
        }
    }
}

/// Built-in MCP server exposing the browser to AI agents.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpServerSettings {
    pub enabled: bool,
    /// Loopback port; 0 picks a free port. The chosen endpoint+token are written to
    /// `mcp-endpoint.json` inside the profile directory for the stdio bridge.
    pub port: u16,
    /// Allow tools that act on pages (click, type, run JavaScript).
    pub allow_page_control: bool,
    /// Allow tools that read browsing data (history, bookmarks).
    pub allow_data_access: bool,
    /// Ask in the UI before an agent runs a tool that changes something.
    pub confirm_actions: bool,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8765,
            allow_page_control: true,
            allow_data_access: true,
            confirm_actions: false,
        }
    }
}

/// An external MCP server the assistant (and extensions) can use.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantProvider {
    #[default]
    Anthropic,
    /// Any OpenAI-compatible chat completions endpoint (OpenAI, Ollama, LM Studio,
    /// llama.cpp server, OpenRouter, vLLM…).
    OpenAiCompatible,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AssistantSettings {
    pub enabled: bool,
    pub provider: AssistantProvider,
    pub model: String,
    /// Base URL; empty means the provider default.
    pub base_url: String,
    /// API key. Prefer `api_key_env`; this is stored in plain text in the profile.
    pub api_key: String,
    /// Name of an environment variable holding the API key.
    pub api_key_env: String,
    pub max_tokens: u32,
    /// Let the assistant use the browser's own tools (read page, navigate, …).
    pub browser_tools: bool,
    pub system_prompt: String,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: AssistantProvider::Anthropic,
            model: "claude-sonnet-5-5".into(),
            base_url: String::new(),
            api_key: String::new(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            max_tokens: 4096,
            browser_tools: true,
            system_prompt: String::new(),
        }
    }
}

impl AssistantSettings {
    pub fn resolved_api_key(&self) -> Option<String> {
        if !self.api_key.trim().is_empty() {
            return Some(self.api_key.trim().to_string());
        }
        if !self.api_key_env.is_empty()
            && let Ok(v) = std::env::var(&self.api_key_env)
            && !v.is_empty()
        {
            return Some(v);
        }
        None
    }
}

/// A page pinned to the right sidebar (like Vivaldi/Edge web panels).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WebPanel {
    pub id: String,
    pub title: String,
    pub url: String,
    /// Mobile user agent for this panel (most sites lay out better in a narrow panel).
    #[serde(default)]
    pub mobile: bool,
}

impl WebPanel {
    pub fn new(title: &str, url: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            title: title.to_string(),
            url: url.to_string(),
            mobile: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SidebarSettings {
    /// Show the icon rail on the right edge.
    pub enabled: bool,
    pub panels: Vec<WebPanel>,
    /// Width of the open panel in points.
    pub width: f32,
    /// Open this panel (web panel id or `ext:<extension id>`) with Ctrl+J instead of the
    /// built-in AI assistant.
    pub assistant_panel: Option<String>,
}

impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            panels: vec![
                WebPanel::new("Wikipedia", "https://es.m.wikipedia.org/"),
                WebPanel::new("Traductor", "https://www.deepl.com/translator"),
            ],
            width: 380.0,
            assistant_panel: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub startup: StartupBehavior,
    pub homepage: String,
    pub new_tab_url: String,
    pub search_engine: String,
    /// User-defined search engines in addition to the built-in ones.
    pub custom_search_engines: Vec<SearchEngine>,
    pub appearance: Appearance,
    pub privacy: Privacy,
    pub downloads: Downloads,
    /// Default page zoom (1.0 = 100%).
    pub default_zoom: f32,
    /// Per-host zoom overrides.
    pub site_zoom: BTreeMap<String, f32>,
    pub user_agent: String,
    pub smooth_scrolling: bool,
    pub enable_devtools: bool,
    pub devtools_port: u16,
    pub mcp_server: McpServerSettings,
    pub mcp_servers: Vec<McpServerConfig>,
    pub assistant: AssistantSettings,
    /// Custom keyboard shortcuts: action id → accelerator ("Ctrl+Shift+K").
    pub keybindings: BTreeMap<String, String>,
    /// Ask before closing a window with several tabs.
    pub confirm_close_many_tabs: bool,
    /// Suspend background tabs after this many minutes (0 = never) to save memory.
    pub tab_sleep_minutes: u32,
    pub language: String,
    pub sidebar: SidebarSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup: StartupBehavior::RestoreSession,
            homepage: "ferro://newtab".into(),
            new_tab_url: "ferro://newtab".into(),
            search_engine: "duckduckgo".into(),
            custom_search_engines: Vec::new(),
            appearance: Appearance::default(),
            privacy: Privacy::default(),
            downloads: Downloads::default(),
            default_zoom: 1.0,
            site_zoom: BTreeMap::new(),
            user_agent: String::new(),
            smooth_scrolling: true,
            enable_devtools: false,
            devtools_port: 6080,
            mcp_server: McpServerSettings::default(),
            mcp_servers: Vec::new(),
            assistant: AssistantSettings::default(),
            keybindings: BTreeMap::new(),
            confirm_close_many_tabs: true,
            tab_sleep_minutes: 30,
            language: String::new(),
            sidebar: SidebarSettings::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &std::path::Path) -> Self {
        crate::storage::read_json_or_default(path)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        crate::storage::write_json(path, self)
    }

    /// All search engines: built-ins followed by custom ones.
    pub fn search_engines(&self) -> Vec<SearchEngine> {
        let mut v = crate::search::builtin_engines();
        v.extend(self.custom_search_engines.iter().cloned());
        v
    }

    pub fn current_search_engine(&self) -> SearchEngine {
        let engines = self.search_engines();
        engines
            .iter()
            .find(|e| e.id == self.search_engine)
            .cloned()
            .unwrap_or_else(|| engines[0].clone())
    }

    pub fn zoom_for_host(&self, host: &str) -> f32 {
        self.site_zoom
            .get(host)
            .copied()
            .unwrap_or(self.default_zoom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_load() {
        let s: Settings =
            serde_json::from_str(r#"{"appearance":{"theme":"dark"},"unknown_field":1}"#).unwrap();
        assert_eq!(s.appearance.theme, ThemeMode::Dark);
        assert!(s.appearance.show_bookmarks_bar);
        assert_eq!(s.search_engine, "duckduckgo");
    }

    #[test]
    fn mcp_config_serde() {
        let c: McpServerConfig = serde_json::from_str(
            r#"{"name":"fs","transport":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/tmp"]}"#,
        )
        .unwrap();
        assert!(c.enabled);
        assert!(matches!(c.transport, McpTransportConfig::Stdio { .. }));
    }
}
