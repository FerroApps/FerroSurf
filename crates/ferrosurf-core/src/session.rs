//! Session save/restore, recently closed tabs and tab groups.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MAX_TAB_HISTORY: usize = 25;
pub const MAX_CLOSED_TABS: usize = 25;
pub const SESSION_VERSION: u32 = 1;

fn default_zoom() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TabState {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    /// Back/forward entries (capped to [`MAX_TAB_HISTORY`]).
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub history_index: usize,
    #[serde(default)]
    pub last_active_ms: i64,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            url: String::new(),
            title: String::new(),
            pinned: false,
            muted: false,
            group: None,
            zoom: 1.0,
            history: Vec::new(),
            history_index: 0,
            last_active_ms: 0,
        }
    }
}

impl TabState {
    pub fn new(url: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    /// Cap history to the last [`MAX_TAB_HISTORY`] entries keeping `history_index` valid.
    fn normalize(&mut self) {
        if self.history.len() > MAX_TAB_HISTORY {
            let excess = self.history.len() - MAX_TAB_HISTORY;
            self.history.drain(..excess);
            self.history_index = self.history_index.saturating_sub(excess);
        }
        if !self.history.is_empty() && self.history_index >= self.history.len() {
            self.history_index = self.history.len() - 1;
        }
        if !(self.zoom.is_finite() && self.zoom > 0.0) {
            self.zoom = 1.0;
        }
    }
}

fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    800
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WindowState {
    #[serde(default)]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active: usize,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub vertical_tabs_width: Option<f32>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            width: default_width(),
            height: default_height(),
            x: None,
            y: None,
            maximized: false,
            vertical_tabs_width: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SessionState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub windows: Vec<WindowState>,
    #[serde(default)]
    pub saved_ms: i64,
    #[serde(default)]
    pub clean_exit: bool,
}

fn keep_tab(t: &TabState) -> bool {
    !(t.url.is_empty() || t.url == "about:blank")
}

impl SessionState {
    pub fn new(windows: Vec<WindowState>, clean_exit: bool) -> Self {
        Self {
            version: SESSION_VERSION,
            windows,
            saved_ms: crate::now_ms(),
            clean_exit,
        }
    }

    /// Drop about:blank tabs, cap histories, drop empty windows, fix `active`.
    pub fn normalize(&mut self) {
        for w in &mut self.windows {
            w.tabs.retain(keep_tab);
            for t in &mut w.tabs {
                t.normalize();
            }
            if w.active >= w.tabs.len() {
                w.active = w.tabs.len().saturating_sub(1);
            }
        }
        self.windows.retain(|w| !w.tabs.is_empty());
    }

    pub fn tab_count(&self) -> usize {
        self.windows.iter().map(|w| w.tabs.len()).sum()
    }

    pub fn load(path: &Path) -> Option<Self> {
        let mut s: Self = crate::storage::read_json(path).ok().flatten()?;
        s.normalize();
        if s.windows.is_empty() { None } else { Some(s) }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut copy = self.clone();
        copy.version = SESSION_VERSION;
        copy.saved_ms = crate::now_ms();
        copy.normalize();
        crate::storage::write_json(path, &copy)
    }
}

/// Recently closed tabs (most recent last), capped.
#[derive(Clone, Debug, Default)]
pub struct ClosedTabs {
    stack: Vec<TabState>,
}

impl ClosedTabs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, mut tab: TabState) {
        if !keep_tab(&tab) {
            return;
        }
        tab.normalize();
        self.stack.push(tab);
        if self.stack.len() > MAX_CLOSED_TABS {
            let excess = self.stack.len() - MAX_CLOSED_TABS;
            self.stack.drain(..excess);
        }
    }

    pub fn pop(&mut self) -> Option<TabState> {
        self.stack.pop()
    }

    /// Oldest first; the last element is what [`pop`](Self::pop) returns next.
    pub fn list(&self) -> &[TabState] {
        &self.stack
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabGroup {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub collapsed: bool,
}

impl TabGroup {
    pub fn new(name: impl Into<String>, color: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            color: color.into(),
            collapsed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.json");
        let mut t = TabState::new("https://a.test/", "A");
        t.history = (0..40).map(|i| format!("https://a.test/{i}")).collect();
        t.history_index = 39;
        let w = WindowState {
            tabs: vec![
                TabState::new("about:blank", ""),
                t,
                TabState::new("ferro://newtab", "New"),
            ],
            active: 2,
            ..Default::default()
        };
        SessionState::new(vec![w], true).save(&p).unwrap();
        let s = SessionState::load(&p).unwrap();
        assert!(s.clean_exit);
        let w = &s.windows[0];
        assert_eq!(w.tabs.len(), 2);
        assert_eq!(w.tabs[0].history.len(), 25);
        assert_eq!(w.tabs[0].history_index, 24);
        assert_eq!(w.tabs[0].zoom, 1.0);
        assert_eq!(w.tabs[1].url, "ferro://newtab");
    }

    #[test]
    fn load_missing_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SessionState::load(&dir.path().join("none.json")).is_none());
        let p = dir.path().join("s.json");
        std::fs::write(
            &p,
            r#"{"windows":[{"tabs":[{"url":"https://x.test/"}],"active":9}]}"#,
        )
        .unwrap();
        let s = SessionState::load(&p).unwrap();
        assert_eq!(s.windows[0].active, 0);
        assert_eq!(s.windows[0].tabs[0].zoom, 1.0);
        assert_eq!(s.windows[0].width, 1280);
    }

    #[test]
    fn closed_tabs_stack() {
        let mut c = ClosedTabs::new();
        for i in 0..30 {
            c.push(TabState::new(format!("https://x.test/{i}"), ""));
        }
        c.push(TabState::new("about:blank", ""));
        assert_eq!(c.len(), 25);
        assert_eq!(c.list().len(), 25);
        assert_eq!(c.pop().unwrap().url, "https://x.test/29");
        assert_eq!(c.list()[0].url, "https://x.test/5");
    }
}
