//! A browser tab. A tab may be "asleep" (no WebView) to save memory; it is woken on demand.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use servo::{LoadStatus, WebView};
use url::Url;

pub type TabId = u64;

static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_tab_id() -> TabId {
    NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    /// "log" | "debug" | "info" | "warn" | "error" | "trace" | "input" | "result"
    pub level: String,
    pub message: String,
    pub time: std::time::SystemTime,
}

#[derive(Clone, Debug)]
pub struct NetEntry {
    pub url: String,
    pub method: String,
    /// Resource type as shown to developers ("document", "script", "image", ...).
    pub kind: String,
    pub main_frame: bool,
    pub blocked: bool,
    pub time: std::time::SystemTime,
}

pub const DEVTOOLS_LOG_CAP: usize = 2000;

impl Tab {
    pub fn push_console(&mut self, level: &str, message: String) {
        if self.console.len() >= DEVTOOLS_LOG_CAP {
            self.console.pop_front();
        }
        self.console.push_back(ConsoleEntry {
            level: level.to_string(),
            message,
            time: std::time::SystemTime::now(),
        });
    }

    pub fn push_network(&mut self, entry: NetEntry) {
        if self.network.len() >= DEVTOOLS_LOG_CAP {
            self.network.pop_front();
        }
        self.network.push_back(entry);
    }
}

pub struct Tab {
    pub id: TabId,
    pub webview: Option<WebView>,
    /// Current (or, when asleep, last known) URL.
    pub url: Option<Url>,
    pub title: String,
    pub favicon: Option<egui::TextureHandle>,
    pub load_status: LoadStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub pinned: bool,
    pub zoom: f32,
    pub last_active: Instant,
    pub crashed: Option<String>,
    /// Navigation was initiated from the address bar (counts as "typed" in history).
    pub typed_navigation: bool,
    /// Text in the omnibox while the user is editing it for this tab.
    pub omnibox_edit: Option<String>,
    pub status_text: Option<String>,
    pub back_forward: Vec<Url>,
    pub history_index: usize,
    /// Tab opened from another tab (for "open next to opener" ordering).
    pub opener: Option<TabId>,
    pub audible: bool,
    pub load_progress_start: Option<Instant>,
    /// Number of requests blocked by the content blocker on the current page.
    pub blocked_requests: u32,
    /// Console messages of the current page (newest last), for the DevTools panel.
    pub console: std::collections::VecDeque<ConsoleEntry>,
    /// Network requests seen for this tab (newest last), for the DevTools panel.
    pub network: std::collections::VecDeque<NetEntry>,
}

impl Tab {
    pub fn new(url: Option<Url>) -> Self {
        Self {
            id: next_tab_id(),
            webview: None,
            url,
            title: String::new(),
            favicon: None,
            load_status: LoadStatus::Complete,
            can_go_back: false,
            can_go_forward: false,
            pinned: false,
            zoom: 1.0,
            last_active: Instant::now(),
            crashed: None,
            typed_navigation: false,
            omnibox_edit: None,
            status_text: None,
            back_forward: Vec::new(),
            history_index: 0,
            opener: None,
            audible: false,
            load_progress_start: None,
            blocked_requests: 0,
            console: Default::default(),
            network: Default::default(),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.load_status != LoadStatus::Complete
    }

    pub fn is_asleep(&self) -> bool {
        self.webview.is_none()
    }

    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        match &self.url {
            Some(u) if u.as_str() == "ferro://newtab" || u.as_str() == "ferro://newtab/" => {
                "Nueva pestaña".into()
            }
            Some(u) => ferrosurf_core::util::pretty_url(u.as_str()),
            None => "Nueva pestaña".into(),
        }
    }

    pub fn url_string(&self) -> String {
        self.url.as_ref().map(|u| u.to_string()).unwrap_or_default()
    }

    /// The URL to show in the omnibox (internal new tab page shows empty).
    pub fn omnibox_display(&self) -> String {
        match &self.url {
            Some(u) if u.scheme() == "ferro" && u.host_str() == Some("newtab") => String::new(),
            Some(u) => u.to_string(),
            None => String::new(),
        }
    }

    pub fn is_secure(&self) -> Option<bool> {
        self.url.as_ref().map(|u| {
            matches!(
                u.scheme(),
                "https" | "wss" | "ferro" | "about" | "file" | "data"
            )
        })
    }

    pub fn host(&self) -> Option<String> {
        self.url
            .as_ref()
            .and_then(|u| u.host_str().map(str::to_string))
    }
}
