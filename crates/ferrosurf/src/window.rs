//! A browser window: winit window + GL contexts + egui chrome + tabs.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use euclid::Scale;
use servo::{
    EmbedderControl, InputEvent, LoadStatus, OffscreenRenderingContext, RenderingContext, Servo,
    UserContentManager, WebView, WebViewBuilder, WindowRenderingContext,
};
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

use crate::actions::{Accelerator, Action, Keymap};
use crate::delegate::Delegate;
use crate::profile_data::ProfileData;
use crate::tab::{Tab, TabId};
use crate::ui::{self, Chrome, SharedUi};

/// Things only the [`crate::app::App`] can do.
pub enum AppCmd {
    NewWindow {
        urls: Vec<Url>,
    },
    NewGuestWindow,
    OpenProfile(String),
    CloseWindow(WindowId),
    Quit,
    ExtensionsChanged,
    SettingsChanged,
    ClearBrowsingData {
        history: bool,
        cookies: bool,
        cache: bool,
    },
    StartAssistant {
        text: String,
    },
    StopAssistant,
    ReconnectMcpClients,
    RestartMcpServer,
    ResolveMcpApproval {
        allow: bool,
        always: bool,
    },
    /// Download an extension package from a store page / link and install it.
    InstallFromStore(crate::store::StoreItem),
    /// Install a local `.zip` / `.xpi` / `.crx` file.
    InstallExtensionFile(std::path::PathBuf),
}

/// References to app-wide state handed to window methods.
pub struct Env<'a> {
    pub servo: &'a Servo,
    pub delegate: &'a Rc<Delegate>,
    pub ucm: &'a Rc<UserContentManager>,
    pub profile: &'a mut ProfileData,
    pub keymap: &'a Keymap,
    pub shared: &'a mut SharedUi,
    pub app_cmds: &'a mut Vec<AppCmd>,
}

/// Where to open a URL.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    CurrentTab,
    NewForegroundTab,
    NewBackgroundTab,
}

/// Commands produced by the UI, executed after the egui frame.
pub enum Cmd {
    Action(Action),
    /// Raw omnibox input or URL.
    Navigate {
        input: String,
        disposition: Disposition,
    },
    OpenUrl {
        url: Url,
        disposition: Disposition,
    },
    ActivateTab(TabId),
    CloseTab(TabId),
    MoveTab {
        from: usize,
        to: usize,
    },
    TogglePin(TabId),
    DuplicateTab(TabId),
    CloseOtherTabs(TabId),
    CloseTabsToRight(TabId),
    ReloadTab(TabId),
    FindInPage {
        query: String,
        forward: bool,
    },
    ClearFind,
    Download {
        url: Url,
    },
    EvalInActive(String),
    /// Evaluate JS in the active tab and store the JSON result (or error) in `slot`.
    DevtoolsEval {
        script: String,
        slot: crate::ui::devtools::EvalSlot,
    },
    /// Open/close a web panel of the right sidebar.
    ToggleSidebarPanel(String),
    CloseSidebar,
    /// Open the sidebar panel's current page in a tab.
    SidebarToTab,
    SidebarReload,
    App(AppCmd),
}

/// The web panel open in the right sidebar: its own webview and framebuffer.
pub struct SidebarView {
    pub panel_id: String,
    pub webview: WebView,
    pub offscreen: Rc<OffscreenRenderingContext>,
    /// Area in physical pixels (set by the UI each frame; empty while hidden).
    pub rect_px: egui::Rect,
    pub focused: bool,
}

pub struct BrowserWindow {
    pub window: Window,
    pub rendering_context: Rc<WindowRenderingContext>,
    pub offscreen: Rc<OffscreenRenderingContext>,
    pub egui_ctx: egui::Context,
    /// `None` once the window is destroyed: its clipboard (smithay-clipboard on Wayland)
    /// must be dropped while the Wayland connection is still alive.
    egui_winit: Option<egui_winit::State>,
    painter: egui_glow::Painter,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub chrome: Chrome,
    pub guest: bool,
    /// Content area in physical pixels within the window.
    pub content_rect_px: egui::Rect,
    modifiers: ModifiersState,
    cursor_px: Option<PhysicalPosition<f64>>,
    mouse_in_content: bool,
    pending_repaint: Option<Instant>,
    pub needs_redraw: bool,
    pub fullscreen_by_page: bool,
    last_title: String,
    /// Tab ids to wake up after their webviews are dropped (sleeping).
    pub controls: Vec<(TabId, EmbedderControl)>,
    pub screenshot_requested: bool,
    pub sidebar: Option<SidebarView>,
    sidebar_hover: bool,
}

impl BrowserWindow {
    pub fn new(
        event_loop: &ActiveEventLoop,
        profile: &ProfileData,
        size: Option<(u32, u32)>,
        guest: bool,
    ) -> anyhow::Result<Self> {
        let (w, h) = size.unwrap_or((1280, 820));
        let title = if guest {
            "FerroSurf — Invitado"
        } else {
            "FerroSurf"
        };
        let mut attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(w, h))
            .with_min_inner_size(winit::dpi::LogicalSize::new(420, 300));
        if let Some(icon) = load_window_icon() {
            attrs = attrs.with_window_icon(Some(icon));
        }
        #[cfg(target_os = "linux")]
        {
            // Matches StartupWMClass / the desktop file id (dev.ferro.ferrosurf).
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attrs =
                WindowAttributesExtWayland::with_name(attrs, "dev.ferro.ferrosurf", "ferrosurf");
            attrs = WindowAttributesExtX11::with_name(attrs, "dev.ferro.ferrosurf", "ferrosurf");
        }
        let window = event_loop.create_window(attrs)?;
        let display_handle = event_loop.display_handle()?;
        let window_handle = window.window_handle()?;
        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .map_err(|e| anyhow::anyhow!("could not create GL context: {e:?}"))?,
        );
        rendering_context
            .make_current()
            .map_err(|e| anyhow::anyhow!("make_current failed: {e:?}"))?;
        let size = window.inner_size();
        let offscreen = Rc::new(
            rendering_context
                .offscreen_context(PhysicalSize::new(size.width.max(1), size.height.max(1))),
        );

        let egui_ctx = egui::Context::default();
        ui::theme::install_fonts(&egui_ctx);
        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let painter = egui_glow::Painter::new(rendering_context.glow_gl_api(), "", None, false)
            .map_err(|e| anyhow::anyhow!("egui painter: {e}"))?;

        let mut me = Self {
            window,
            rendering_context,
            offscreen,
            egui_ctx,
            egui_winit: Some(egui_winit),
            painter,
            tabs: Vec::new(),
            active: 0,
            chrome: Chrome::default(),
            guest,
            content_rect_px: egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w as f32, h as f32),
            ),
            modifiers: ModifiersState::empty(),
            cursor_px: None,
            mouse_in_content: false,
            pending_repaint: None,
            needs_redraw: true,
            fullscreen_by_page: false,
            last_title: String::new(),
            controls: Vec::new(),
            screenshot_requested: false,
            sidebar: None,
            sidebar_hover: false,
        };
        ui::theme::apply(&me.egui_ctx, profile, me.window.theme());
        let _ = &mut me;
        Ok(me)
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn active_webview(&self) -> Option<&WebView> {
        self.active_tab().and_then(|t| t.webview.as_ref())
    }

    pub fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn is_sidebar_webview(&self, webview: &WebView) -> bool {
        self.sidebar
            .as_ref()
            .is_some_and(|s| s.webview.id() == webview.id())
    }

    /// Open (or toggle off) a web panel of the right sidebar.
    pub fn toggle_sidebar_panel(&mut self, env: &Env, panel_id: &str) {
        if self
            .sidebar
            .as_ref()
            .is_some_and(|s| s.panel_id == panel_id)
        {
            self.sidebar = None;
            self.needs_redraw = true;
            return;
        }
        let Some(panel) = env
            .profile
            .settings
            .sidebar
            .panels
            .iter()
            .find(|p| p.id == panel_id)
            .cloned()
        else {
            return;
        };
        let Some(url) = interpret_input(env, &panel.url) else {
            return;
        };
        let _ = self.rendering_context.make_current();
        let size = PhysicalSize::new(
            (env.profile.settings.sidebar.width * self.scale()).max(1.0) as u32,
            self.content_size().height,
        );
        let offscreen = Rc::new(self.rendering_context.offscreen_context(size));
        let webview = WebViewBuilder::new(env.servo, offscreen.clone() as Rc<dyn RenderingContext>)
            .url(url)
            .hidpi_scale_factor(Scale::new(self.scale()))
            .delegate(env.delegate.clone())
            .user_content_manager(env.ucm.clone())
            .build();
        webview.resize(size);
        webview.notify_theme_change(if env.profile.settings.appearance.web_dark_mode {
            servo::Theme::Dark
        } else {
            servo::Theme::Light
        });
        self.sidebar = Some(SidebarView {
            panel_id: panel.id,
            webview,
            offscreen,
            rect_px: egui::Rect::NOTHING,
            focused: false,
        });
        self.needs_redraw = true;
    }

    pub fn set_sidebar_rect(&mut self, rect_points: egui::Rect) {
        let s = self.egui_ctx.pixels_per_point();
        if let Some(sb) = &mut self.sidebar {
            sb.rect_px = egui::Rect::from_min_max(
                (rect_points.min.to_vec2() * s).round().to_pos2(),
                (rect_points.max.to_vec2() * s).round().to_pos2(),
            );
        }
    }

    /// Route pointer/keyboard events to the sidebar webview. Returns true if consumed.
    fn sidebar_input(&mut self, event: &WindowEvent) -> bool {
        let Some(sb) = &mut self.sidebar else {
            return false;
        };
        if sb.rect_px.width() < 1.0 {
            return false;
        }
        let over = |p: Option<PhysicalPosition<f64>>, ctx: &egui::Context, chrome: &Chrome| {
            p.map(|p| egui::pos2(p.x as f32, p.y as f32))
                .filter(|p| sb.rect_px.contains(*p) && !chrome.blocks_content_at(ctx, *p, 1.0))
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(p) = over(Some(*position), &self.egui_ctx, &self.chrome) {
                    let local = p - sb.rect_px.min;
                    sb.webview
                        .notify_input_event(crate::input::mouse_move(local.x, local.y));
                    self.sidebar_hover = true;
                } else if self.sidebar_hover {
                    self.sidebar_hover = false;
                    sb.webview.notify_input_event(InputEvent::MouseLeftViewport(
                        servo::MouseLeftViewportEvent {
                            focus_moving_to_another_iframe: false,
                        },
                    ));
                }
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(p) = over(self.cursor_px, &self.egui_ctx, &self.chrome) else {
                    if *state == ElementState::Pressed {
                        sb.focused = false;
                    }
                    return false;
                };
                let Some(b) = crate::input::mouse_button(*button) else {
                    return true;
                };
                let local = p - sb.rect_px.min;
                let pressed = *state == ElementState::Pressed;
                if pressed {
                    sb.focused = true;
                    sb.webview.set_focused(true);
                    self.egui_ctx.memory_mut(|m| {
                        if let Some(id) = m.focused() {
                            m.surrender_focus(id);
                        }
                    });
                }
                sb.webview
                    .notify_input_event(crate::input::mouse_button_event(
                        pressed, b, local.x, local.y,
                    ));
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(p) = over(self.cursor_px, &self.egui_ctx, &self.chrome) else {
                    return false;
                };
                let local = p - sb.rect_px.min;
                sb.webview
                    .notify_input_event(crate::input::wheel(*delta, local.x, local.y));
                true
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if sb.focused && !self.egui_ctx.egui_wants_keyboard_input() => {
                sb.webview
                    .notify_input_event(InputEvent::Keyboard(crate::input::keyboard_event(
                        key_event,
                        self.modifiers,
                    )));
                true
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text))
                if sb.focused && !self.egui_ctx.egui_wants_keyboard_input() =>
            {
                sb.webview
                    .notify_input_event(InputEvent::Ime(servo::ImeEvent::Composition(
                        servo::CompositionEvent {
                            state: servo::CompositionState::End,
                            data: text.clone(),
                        },
                    )));
                true
            }
            _ => false,
        }
    }

    pub fn tab_by_webview(&self, webview: &WebView) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.webview.as_ref().is_some_and(|w| w.id() == webview.id()))
    }

    fn content_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(
            (self.content_rect_px.width().round() as u32).max(1),
            (self.content_rect_px.height().round() as u32).max(1),
        )
    }

    fn build_webview(&self, env: &Env, url: Url) -> WebView {
        let webview = WebViewBuilder::new(
            env.servo,
            self.offscreen.clone() as Rc<dyn RenderingContext>,
        )
        .url(url)
        .hidpi_scale_factor(Scale::new(self.scale()))
        .delegate(env.delegate.clone())
        .user_content_manager(env.ucm.clone())
        .build();
        self.configure_webview(env, &webview);
        webview
    }

    fn configure_webview(&self, env: &Env, webview: &WebView) {
        webview.resize(self.content_size());
        let web_dark = env.profile.settings.appearance.web_dark_mode;
        webview.notify_theme_change(if web_dark {
            servo::Theme::Dark
        } else {
            servo::Theme::Light
        });
    }

    pub fn ensure_awake(&mut self, env: &Env, idx: usize) {
        self.wake_tab(env, idx);
    }

    /// Ensure the tab at `idx` has a live webview (wakes sleeping tabs).
    fn wake_tab(&mut self, env: &Env, idx: usize) {
        if self.tabs[idx].webview.is_some() {
            return;
        }
        let url = self.tabs[idx].url.clone().unwrap_or_else(|| {
            Url::parse(&env.profile.settings.new_tab_url)
                .unwrap_or_else(|_| Url::parse("ferro://newtab").unwrap())
        });
        let webview = self.build_webview(env, url);
        let zoom = self.tabs[idx].zoom;
        if (zoom - 1.0).abs() > 0.001 {
            webview.set_page_zoom(zoom);
        }
        self.tabs[idx].webview = Some(webview);
        self.tabs[idx].load_status = LoadStatus::Started;
    }

    pub fn new_tab_url(env: &Env) -> Url {
        Url::parse(&env.profile.settings.new_tab_url)
            .unwrap_or_else(|_| Url::parse("ferro://newtab").unwrap())
    }

    /// Open a new tab. Returns its id.
    pub fn open_tab(
        &mut self,
        env: &Env,
        url: Option<Url>,
        foreground: bool,
        opener: Option<TabId>,
    ) -> TabId {
        let url = url.unwrap_or_else(|| Self::new_tab_url(env));
        let mut tab = Tab::new(Some(url.clone()));
        tab.opener = opener;
        if let Some(host) = url.host_str() {
            tab.zoom = env.profile.settings.zoom_for_host(host);
        } else {
            tab.zoom = env.profile.settings.default_zoom;
        }
        let id = tab.id;
        let idx = match opener.and_then(|o| self.tab_index(o)) {
            Some(oi) => {
                // After the opener and any tabs it already opened.
                let mut i = oi + 1;
                while i < self.tabs.len() && self.tabs[i].opener == opener {
                    i += 1;
                }
                i
            }
            None => self.tabs.len(),
        };
        self.tabs.insert(idx, tab);
        if idx <= self.active && !self.tabs.is_empty() && self.tabs.len() > 1 {
            self.active += 1;
        }
        self.wake_tab(env, idx);
        if foreground || self.tabs.len() == 1 {
            self.activate(env, idx);
            if url.scheme() == "ferro" && url.host_str() == Some("newtab") {
                self.chrome.focus_omnibox = true;
            }
        } else if let Some(w) = &self.tabs[idx].webview {
            w.hide();
        }
        self.needs_redraw = true;
        id
    }

    /// Adopt a webview that Servo created (popup / window.open).
    pub fn adopt_webview(
        &mut self,
        env: &Env,
        webview: WebView,
        opener: Option<TabId>,
        foreground: bool,
    ) {
        let mut tab = Tab::new(webview.url());
        tab.opener = opener;
        self.configure_webview(env, &webview);
        tab.webview = Some(webview);
        tab.load_status = LoadStatus::Started;
        let idx = opener
            .and_then(|o| self.tab_index(o))
            .map(|i| i + 1)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(idx, tab);
        if idx <= self.active && self.tabs.len() > 1 {
            self.active += 1;
        }
        if foreground {
            self.activate(env, idx);
        } else if let Some(w) = &self.tabs[idx].webview {
            w.hide();
        }
    }

    pub fn activate(&mut self, env: &Env, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        if let Some(prev) = self.tabs.get(self.active).and_then(|t| t.webview.as_ref())
            && self.active != idx
        {
            prev.hide();
            prev.set_focused(false);
        }
        self.active = idx;
        self.wake_tab(env, idx);
        let size = self.content_size();
        let tab = &mut self.tabs[idx];
        tab.last_active = Instant::now();
        if let Some(w) = &tab.webview {
            w.show();
            w.resize(size);
            w.set_focused(true);
        }
        self.chrome.on_tab_switched();
        self.needs_redraw = true;
    }

    pub fn close_tab(&mut self, env: &mut Env, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(idx);
        env.profile.passwords.cancel_for_tab(tab.id);
        if let Some(url) = &tab.url
            && !(url.scheme() == "ferro" && url.host_str() == Some("newtab"))
        {
            env.profile
                .closed_tabs
                .push(ferrosurf_core::session::TabState {
                    url: url.to_string(),
                    title: tab.title.clone(),
                    pinned: tab.pinned,
                    zoom: tab.zoom,
                    ..Default::default()
                });
        }
        self.controls.retain(|(t, _)| *t != tab.id);
        drop(tab); // dropping the WebView closes it
        if self.tabs.is_empty() {
            env.app_cmds.push(AppCmd::CloseWindow(self.id()));
            return;
        }
        let new_active = if idx < self.active {
            self.active - 1
        } else if idx == self.active {
            idx.min(self.tabs.len() - 1)
        } else {
            self.active
        };
        self.active = usize::MAX;
        self.activate(env, new_active);
    }

    /// Navigate a tab using raw omnibox input (URL or search).
    pub fn navigate_input(&mut self, env: &mut Env, input: &str, disposition: Disposition) {
        let engines = env.profile.settings.search_engines();
        let default = env.profile.settings.current_search_engine();
        let Some(interp) = ferrosurf_core::omnibox::interpret(input, &default, &engines) else {
            return;
        };
        let mut url = interp.url().clone();
        // Internal "native" pages.
        if url.scheme() == "ferro" {
            match url.host_str() {
                Some("settings") => return self.run_action(env, Action::ShowSettings),
                Some("history") => return self.run_action(env, Action::ShowHistory),
                Some("bookmarks") => return self.run_action(env, Action::ShowBookmarks),
                Some("downloads") => return self.run_action(env, Action::ShowDownloads),
                Some("extensions") => return self.run_action(env, Action::ShowExtensions),
                _ => {}
            }
        }
        if env.profile.settings.privacy.https_only
            && let Some(u) = ferrosurf_core::omnibox::https_upgrade(&url)
        {
            url = u;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.typed_navigation = true;
        }
        self.open_url(env, url, disposition);
    }

    pub fn open_url(&mut self, env: &mut Env, url: Url, disposition: Disposition) {
        match disposition {
            Disposition::CurrentTab if !self.tabs.is_empty() => {
                let idx = self.active;
                if self.tabs[idx].webview.is_none() {
                    self.tabs[idx].url = Some(url);
                    self.wake_tab(env, idx);
                } else if let Some(w) = &self.tabs[idx].webview {
                    w.load(url.clone());
                    self.tabs[idx].url = Some(url);
                }
                self.tabs[idx].omnibox_edit = None;
                if let Some(w) = &self.tabs[idx].webview {
                    w.set_focused(true);
                }
            }
            Disposition::NewBackgroundTab => {
                let opener = self.active_tab().map(|t| t.id);
                self.open_tab(env, Some(url), false, opener);
            }
            _ => {
                self.open_tab(env, Some(url), true, None);
            }
        }
        self.needs_redraw = true;
    }

    pub fn set_zoom(&mut self, env: &mut Env, zoom: f32) {
        let zoom = zoom.clamp(0.25, 5.0);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.zoom = zoom;
            if let Some(w) = &tab.webview {
                w.set_page_zoom(zoom);
            }
            if let Some(host) = tab.host() {
                if (zoom - env.profile.settings.default_zoom).abs() < 0.001 {
                    env.profile.settings.site_zoom.remove(&host);
                } else {
                    env.profile.settings.site_zoom.insert(host, zoom);
                }
                env.profile.mark_settings_dirty();
            }
            self.chrome.show_zoom_bubble();
        }
    }

    /// Execute a keyboard/menu action.
    pub fn run_action(&mut self, env: &mut Env, action: Action) {
        use Action::*;
        let active_wv = self.active_webview().cloned();
        match action {
            NewTab => {
                self.open_tab(env, None, true, None);
            }
            CloseTab => self.close_tab(env, self.active),
            ReopenClosedTab => {
                if let Some(state) = env.profile.closed_tabs.pop()
                    && let Ok(url) = Url::parse(&state.url)
                {
                    let id = self.open_tab(env, Some(url), true, None);
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].pinned = state.pinned;
                    }
                }
            }
            NextTab if !self.tabs.is_empty() => {
                let n = (self.active + 1) % self.tabs.len();
                self.activate(env, n);
            }
            PrevTab if !self.tabs.is_empty() => {
                let n = (self.active + self.tabs.len() - 1) % self.tabs.len();
                self.activate(env, n);
            }
            SelectTab(n) => {
                let i = (n as usize).saturating_sub(1);
                if i < self.tabs.len() {
                    self.activate(env, i);
                }
            }
            SelectLastTab if !self.tabs.is_empty() => self.activate(env, self.tabs.len() - 1),
            NewWindow => env.app_cmds.push(AppCmd::NewWindow { urls: vec![] }),
            NewGuestWindow => env.app_cmds.push(AppCmd::NewGuestWindow),
            FocusOmnibox => {
                self.chrome.focus_omnibox = true;
                self.chrome.select_all_omnibox = true;
            }
            Reload => {
                if let Some(w) = &active_wv {
                    w.reload();
                }
            }
            HardReload => {
                if let Some(w) = &active_wv {
                    env.servo.network_manager().clear_cache();
                    w.reload();
                }
            }
            Stop => {
                if self.chrome.close_transient() {
                    return;
                }
                // Servo has no stop(); reloading the current URL isn't wanted. Best effort:
                // ask the page to stop loading.
                if let Some(w) = &active_wv {
                    w.evaluate_javascript("window.stop()", |_| {});
                }
            }
            Back => {
                if let Some(w) = &active_wv
                    && w.can_go_back()
                {
                    w.go_back(1);
                }
            }
            Forward => {
                if let Some(w) = &active_wv
                    && w.can_go_forward()
                {
                    w.go_forward(1);
                }
            }
            Home => {
                let home = env.profile.settings.homepage.clone();
                self.navigate_input(env, &home, Disposition::CurrentTab);
            }
            Find => {
                self.chrome.find.open = true;
                self.chrome.find.focus = true;
            }
            BookmarkPage => {
                if let Some(tab) = self.active_tab() {
                    let url = tab.url_string();
                    let title = tab.display_title();
                    if !url.is_empty() {
                        let added = env.profile.bookmarks.toggle(&title, &url);
                        if added {
                            self.chrome.bookmark_editor = env
                                .profile
                                .bookmarks
                                .find_by_url(&url)
                                .map(ui::BookmarkEditor::new);
                        }
                        env.shared.toast(if added {
                            "Añadido a marcadores"
                        } else {
                            "Marcador eliminado"
                        });
                    }
                }
            }
            ToggleBookmarksBar => {
                let s = &mut env.profile.settings.appearance.show_bookmarks_bar;
                *s = !*s;
                env.profile.mark_settings_dirty();
            }
            ShowBookmarks => self.chrome.open_library(ui::LibraryTab::Bookmarks),
            ShowHistory => self.chrome.open_library(ui::LibraryTab::History),
            ShowDownloads => self.chrome.open_library(ui::LibraryTab::Downloads),
            ShowExtensions => self.chrome.open_settings(ui::SettingsPage::Extensions),
            ShowSettings => self.chrome.open_settings(ui::SettingsPage::Appearance),
            ToggleAssistant
                if let Some(panel) = env.profile.settings.sidebar.assistant_panel.clone() =>
            {
                self.toggle_sidebar_panel(env, &panel);
            }
            ToggleAssistant if !env.profile.settings.assistant.enabled => {
                self.chrome.assistant_open = false;
                env.shared
                    .toast("El asistente IA está desactivado (Ajustes → Asistente IA)");
            }
            ToggleAssistant => {
                self.chrome.assistant_open = !self.chrome.assistant_open;
                if self.chrome.assistant_open {
                    env.shared.assistant.focus_input = true;
                }
            }
            CommandPalette => self.chrome.palette.open(),
            ZoomIn => {
                let z = self.active_tab().map(|t| t.zoom).unwrap_or(1.0);
                self.set_zoom(env, next_zoom(z, true));
            }
            ZoomOut => {
                let z = self.active_tab().map(|t| t.zoom).unwrap_or(1.0);
                self.set_zoom(env, next_zoom(z, false));
            }
            ZoomReset => {
                let z = env.profile.settings.default_zoom;
                self.set_zoom(env, z);
            }
            Fullscreen => {
                if self.window.fullscreen().is_some() {
                    self.window.set_fullscreen(None);
                } else {
                    self.window
                        .set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
                }
            }
            ToggleVerticalTabs => {
                use ferrosurf_core::settings::TabLayout;
                let l = &mut env.profile.settings.appearance.tab_layout;
                *l = if *l == TabLayout::Vertical {
                    TabLayout::Horizontal
                } else {
                    TabLayout::Vertical
                };
                env.profile.mark_settings_dirty();
            }
            DuplicateTab => {
                if let Some(tab) = self.active_tab() {
                    let url = tab.url.clone();
                    let opener = Some(tab.id);
                    self.open_tab(env, url, true, opener);
                }
            }
            PinTab => {
                if let Some(id) = self.active_tab().map(|t| t.id) {
                    self.toggle_pin(id);
                }
            }
            ClearBrowsingData => self.chrome.open_settings(ui::SettingsPage::Privacy),
            DevTools => {
                self.chrome.devtools_open = !self.chrome.devtools_open;
            }
            Screenshot => self.screenshot_requested = true,
            ViewSource => {
                if let Some(url) = self.active_tab().and_then(|t| t.url.clone())
                    && let Ok(u) = Url::parse(&format!("view-source:{url}"))
                {
                    self.open_tab(env, Some(u), true, None);
                }
            }
            Quit => env.app_cmds.push(AppCmd::Quit),
            ProfileManager => self.chrome.open_settings(ui::SettingsPage::Profiles),
            About => {
                self.open_url(
                    env,
                    Url::parse("ferro://about").unwrap(),
                    Disposition::NewForegroundTab,
                );
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    pub fn toggle_pin(&mut self, id: TabId) {
        let Some(idx) = self.tab_index(id) else {
            return;
        };
        let active_id = self.active_tab().map(|t| t.id);
        let mut tab = self.tabs.remove(idx);
        tab.pinned = !tab.pinned;
        let pinned_count = self.tabs.iter().filter(|t| t.pinned).count();
        let new_idx = if tab.pinned {
            pinned_count
        } else {
            pinned_count.max(idx.min(self.tabs.len()))
        };
        self.tabs.insert(new_idx.min(self.tabs.len()), tab);
        if let Some(a) = active_id.and_then(|a| self.tab_index(a)) {
            self.active = a;
        }
    }

    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.active_tab().map(|t| t.id);
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        if let Some(a) = active_id.and_then(|a| self.tab_index(a)) {
            self.active = a;
        }
    }

    pub fn exec(&mut self, env: &mut Env, cmd: Cmd) {
        match cmd {
            Cmd::Action(a) => self.run_action(env, a),
            Cmd::ToggleSidebarPanel(id) => self.toggle_sidebar_panel(env, &id),
            Cmd::CloseSidebar => {
                self.sidebar = None;
                self.needs_redraw = true;
            }
            Cmd::SidebarReload => {
                if let Some(sb) = &self.sidebar {
                    sb.webview.reload();
                }
            }
            Cmd::SidebarToTab => {
                if let Some(url) = self.sidebar.as_ref().and_then(|s| s.webview.url()) {
                    self.open_url(env, url, Disposition::NewForegroundTab);
                }
            }
            Cmd::Navigate { input, disposition } => self.navigate_input(env, &input, disposition),
            Cmd::OpenUrl { url, disposition } => self.open_url(env, url, disposition),
            Cmd::ActivateTab(id) => {
                if let Some(i) = self.tab_index(id) {
                    self.activate(env, i);
                }
            }
            Cmd::CloseTab(id) => {
                if let Some(i) = self.tab_index(id) {
                    self.close_tab(env, i);
                }
            }
            Cmd::MoveTab { from, to } => self.move_tab(from, to),
            Cmd::TogglePin(id) => self.toggle_pin(id),
            Cmd::DuplicateTab(id) => {
                if let Some(i) = self.tab_index(id) {
                    let url = self.tabs[i].url.clone();
                    self.open_tab(env, url, true, Some(id));
                }
            }
            Cmd::CloseOtherTabs(id) => {
                let ids: Vec<_> = self
                    .tabs
                    .iter()
                    .filter(|t| t.id != id && !t.pinned)
                    .map(|t| t.id)
                    .collect();
                for other in ids {
                    if let Some(i) = self.tab_index(other) {
                        self.close_tab(env, i);
                    }
                }
            }
            Cmd::CloseTabsToRight(id) => {
                if let Some(i) = self.tab_index(id) {
                    let ids: Vec<_> = self.tabs[i + 1..].iter().map(|t| t.id).collect();
                    for other in ids {
                        if let Some(j) = self.tab_index(other) {
                            self.close_tab(env, j);
                        }
                    }
                }
            }
            Cmd::ReloadTab(id) => {
                if let Some(w) = self
                    .tab_index(id)
                    .and_then(|i| self.tabs[i].webview.clone())
                {
                    w.reload();
                }
            }
            Cmd::FindInPage { query, forward } => {
                if let Some(w) = self.active_webview().cloned() {
                    let script = ui::find::find_script(&query, forward);
                    let proxy_state = self.chrome.find.result_slot.clone();
                    w.evaluate_javascript(script, move |res| {
                        if let Ok(servo::JSValue::String(s)) = res {
                            *proxy_state.lock() = Some(s);
                        }
                    });
                }
            }
            Cmd::ClearFind => {
                if let Some(w) = self.active_webview() {
                    w.evaluate_javascript(ui::find::CLEAR_SCRIPT, |_| {});
                }
            }
            Cmd::Download { url } => {
                start_download(env, &url, self.active_tab().and_then(|t| t.url.clone()))
            }
            Cmd::EvalInActive(js) => {
                if let Some(w) = self.active_webview() {
                    w.evaluate_javascript(js, |_| {});
                }
            }
            Cmd::DevtoolsEval { script, slot } => match self.active_webview() {
                Some(w) => {
                    let wake = env.shared.wake.clone();
                    w.evaluate_javascript(script, move |res| {
                        *slot.lock() = Some(match res {
                            Ok(v) => Ok(crate::mcp_host::js_to_json(v)),
                            Err(e) => Err(format!("{e:?}")),
                        });
                        if let Some(wake) = wake {
                            wake();
                        }
                    });
                }
                None => *slot.lock() = Some(Err("No hay ninguna página cargada".into())),
            },
            Cmd::App(c) => env.app_cmds.push(c),
        }
        self.needs_redraw = true;
    }

    /// Put long-unused background tabs to sleep (drop their WebView).
    pub fn sleep_idle_tabs(&mut self, minutes: u32) {
        if minutes == 0 {
            return;
        }
        let limit = Duration::from_secs(minutes as u64 * 60);
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i != active
                && tab.webview.is_some()
                && !tab.audible
                && tab.last_active.elapsed() > limit
            {
                log::info!("Putting tab {} to sleep", tab.display_title());
                tab.webview = None;
                tab.load_status = LoadStatus::Complete;
            }
        }
    }

    // ---------------------------------------------------------------- input

    /// Returns true if the event was consumed and a redraw is needed.
    pub fn handle_window_event(&mut self, env: &mut Env, event: &WindowEvent) {
        // Global shortcuts first.
        if let WindowEvent::KeyboardInput {
            event: key_event,
            is_synthetic: false,
            ..
        } = event
            && key_event.state == ElementState::Pressed
            && let Some(acc) = Accelerator::from_key(&key_event.logical_key, self.modifiers)
            && let Some(action) = env.keymap.lookup(&acc)
        {
            let typing_in_ui = self.egui_ctx.egui_wants_keyboard_input();
            // Plain keys (Escape, F-keys) always work; Ctrl/Alt shortcuts too.
            // Don't steal editing shortcuts from egui text fields.
            let editing_combo = typing_in_ui
                && acc.ctrl
                && !acc.shift
                && matches!(
                    acc.key.as_str(),
                    "a" | "c" | "v" | "x" | "z" | "y" | "left" | "right" | "backspace" | "delete"
                );
            let stop_in_ui = typing_in_ui && action == Action::Stop;
            if !editing_combo && !stop_in_ui {
                self.run_action(env, action);
                self.needs_redraw = true;
                return;
            }
        }

        let Some(egui_winit) = self.egui_winit.as_mut() else {
            return;
        };
        let response = egui_winit.on_window_event(&self.window, event);
        if response.repaint {
            self.needs_redraw = true;
        }

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor_px = Some(*position);
        }
        if self.sidebar_input(event) {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && let Some(sb) = &mut self.sidebar
        {
            sb.focused = false;
        }

        match event {
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Resized(size) => {
                self.rendering_context.resize(*size);
                self.needs_redraw = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                for tab in &self.tabs {
                    if let Some(w) = &tab.webview {
                        w.set_hidpi_scale_factor(Scale::new(*scale_factor as f32));
                    }
                }
                self.needs_redraw = true;
            }
            WindowEvent::ThemeChanged(_) => {
                ui::theme::apply(&self.egui_ctx, env.profile, self.window.theme());
                for tab in &self.tabs {
                    if let Some(w) = &tab.webview {
                        self.configure_webview(env, w);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = Some(*position);
                let p = egui::pos2(position.x as f32, position.y as f32);
                let inside = self.content_rect_px.contains(p)
                    && !self
                        .chrome
                        .blocks_content_at(&self.egui_ctx, p, self.scale());
                if inside {
                    self.mouse_in_content = true;
                    if let Some(w) = self.active_webview() {
                        let local = p - self.content_rect_px.min;
                        w.notify_input_event(crate::input::mouse_move(local.x, local.y));
                    }
                } else if self.mouse_in_content {
                    self.mouse_in_content = false;
                    if let Some(w) = self.active_webview() {
                        w.notify_input_event(InputEvent::MouseLeftViewport(
                            servo::MouseLeftViewportEvent {
                                focus_moving_to_another_iframe: false,
                            },
                        ));
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.mouse_in_content {
                    self.mouse_in_content = false;
                    if let Some(w) = self.active_webview() {
                        w.notify_input_event(InputEvent::MouseLeftViewport(
                            servo::MouseLeftViewportEvent {
                                focus_moving_to_another_iframe: false,
                            },
                        ));
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let (Some(pos), Some(b)) = (self.cursor_px, crate::input::mouse_button(*button))
                {
                    let p = egui::pos2(pos.x as f32, pos.y as f32);
                    if self.content_rect_px.contains(p)
                        && !self
                            .chrome
                            .blocks_content_at(&self.egui_ctx, p, self.scale())
                    {
                        let local = p - self.content_rect_px.min;
                        let pressed = *state == ElementState::Pressed;
                        if pressed {
                            self.chrome.on_content_click();
                            // Clicking the page takes focus away from egui widgets.
                            self.egui_ctx.memory_mut(|m| {
                                if let Some(id) = m.focused() {
                                    m.surrender_focus(id);
                                }
                            });
                        }
                        // Mouse back/forward buttons navigate.
                        if pressed && *button == winit::event::MouseButton::Back {
                            self.run_action(env, Action::Back);
                        } else if pressed && *button == winit::event::MouseButton::Forward {
                            self.run_action(env, Action::Forward);
                        }
                        if let Some(w) = self.active_webview() {
                            w.set_focused(true);
                            w.notify_input_event(crate::input::mouse_button_event(
                                pressed, b, local.x, local.y,
                            ));
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(pos) = self.cursor_px {
                    let p = egui::pos2(pos.x as f32, pos.y as f32);
                    if self.content_rect_px.contains(p)
                        && !self
                            .chrome
                            .blocks_content_at(&self.egui_ctx, p, self.scale())
                    {
                        if self.modifiers.control_key() {
                            let dy = match delta {
                                winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f64,
                                winit::event::MouseScrollDelta::PixelDelta(d) => d.y / 50.0,
                            };
                            if dy.abs() > 0.0 {
                                let z = self.active_tab().map(|t| t.zoom).unwrap_or(1.0);
                                self.set_zoom(env, next_zoom(z, dy > 0.0));
                            }
                        } else if let Some(w) = self.active_webview() {
                            let local = p - self.content_rect_px.min;
                            w.notify_input_event(crate::input::wheel(*delta, local.x, local.y));
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if !self.egui_ctx.egui_wants_keyboard_input()
                    && !self.chrome.has_modal()
                    && let Some(w) = self.active_webview()
                {
                    w.notify_input_event(InputEvent::Keyboard(crate::input::keyboard_event(
                        key_event,
                        self.modifiers,
                    )));
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if !self.egui_ctx.egui_wants_keyboard_input()
                    && let Some(w) = self.active_webview()
                {
                    w.notify_input_event(InputEvent::Ime(servo::ImeEvent::Composition(
                        servo::CompositionEvent {
                            state: servo::CompositionState::End,
                            data: text.clone(),
                        },
                    )));
                }
            }
            WindowEvent::Focused(focused) => {
                if let Some(w) = self.active_webview() {
                    w.set_focused(*focused);
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Ok(url) = Url::from_file_path(path) {
                    self.open_url(env, url, Disposition::NewForegroundTab);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- frame

    pub fn wants_repaint_at(&self) -> Option<Instant> {
        self.pending_repaint
    }

    pub fn redraw(&mut self, env: &mut Env) {
        self.needs_redraw = false;
        self.pending_repaint = None;

        let Some(egui_winit) = self.egui_winit.as_mut() else {
            return;
        };
        let raw_input = egui_winit.take_egui_input(&self.window);
        let mut cmds: Vec<Cmd> = Vec::new();
        let offscreen = self.offscreen.clone();
        let full_output = {
            let ctx = self.egui_ctx.clone();
            ctx.run_ui(raw_input, |root| {
                ui::draw(root, self, env, &offscreen, &mut cmds);
            })
        };
        if let Some(egui_winit) = self.egui_winit.as_mut() {
            egui_winit.handle_platform_output(&self.window, full_output.platform_output);
        }

        // Resize the web content if the chrome layout changed.
        let size = self.content_size();
        if self.offscreen.size() != size {
            if let Some(w) = self.active_webview() {
                w.resize(size);
            } else {
                self.offscreen.resize(size);
            }
            self.needs_redraw = true;
        }
        if let Some(sb) = &self.sidebar {
            let size = PhysicalSize::new(
                (sb.rect_px.width().round() as u32).max(1),
                (sb.rect_px.height().round() as u32).max(1),
            );
            if sb.rect_px.width() >= 1.0 && sb.offscreen.size() != size {
                sb.webview.resize(size);
                self.needs_redraw = true;
            }
        }

        // Paint: Servo renders into the offscreen FBO, then egui composites everything.
        if let Err(e) = self.rendering_context.make_current() {
            log::warn!("make_current: {e:?}");
        }
        if let Some(w) = self.active_webview() {
            w.paint();
        }
        if let Some(sb) = &self.sidebar
            && sb.rect_px.width() >= 1.0
        {
            sb.webview.paint();
        }
        self.rendering_context.prepare_for_rendering();
        let gl = self.rendering_context.glow_gl_api();
        let window_size = self.window.inner_size();
        unsafe {
            use glow::HasContext;
            let bg = ui::theme::window_bg(&self.egui_ctx);
            gl.disable(glow::SCISSOR_TEST);
            gl.viewport(0, 0, window_size.width as i32, window_size.height as i32);
            gl.clear_color(bg[0], bg[1], bg[2], 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        let ppp = full_output.pixels_per_point;
        let primitives = self.egui_ctx.tessellate(full_output.shapes, ppp);
        let mut textures = full_output.textures_delta;
        self.painter.paint_and_update_textures(
            [window_size.width, window_size.height],
            ppp,
            &primitives,
            &mut textures,
        );
        self.rendering_context.present();

        if let Some(repaint) = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
        {
            if repaint.is_zero() {
                self.needs_redraw = true;
            } else if repaint < Duration::from_secs(3600) {
                self.pending_repaint = Some(Instant::now() + repaint);
            }
        }

        // Window title follows the active tab.
        let title = match self.active_tab() {
            Some(t) => format!(
                "{} — FerroSurf{}",
                t.display_title(),
                if self.guest { " (Invitado)" } else { "" }
            ),
            None => "FerroSurf".into(),
        };
        if title != self.last_title {
            self.window.set_title(&title);
            self.last_title = title;
        }

        for cmd in cmds {
            self.exec(env, cmd);
        }
        if self.needs_redraw {
            self.window.request_redraw();
        }
    }

    pub fn set_content_rect(&mut self, rect_points: egui::Rect) {
        let s = self.egui_ctx.pixels_per_point();
        self.content_rect_px = egui::Rect::from_min_max(
            (rect_points.min.to_vec2() * s).round().to_pos2(),
            (rect_points.max.to_vec2() * s).round().to_pos2(),
        );
    }

    pub fn destroy(&mut self) {
        // Drop the clipboard worker first, while the display connection is alive.
        self.egui_winit = None;
        let _ = self.rendering_context.make_current();
        self.sidebar = None;
        self.tabs.clear();
        self.painter.destroy();
    }
}

/// Interpret omnibox-style input as a URL (search or address), honoring HTTPS-only mode.
pub fn interpret_input(env: &Env, input: &str) -> Option<Url> {
    let engines = env.profile.settings.search_engines();
    let default = env.profile.settings.current_search_engine();
    let interp = ferrosurf_core::omnibox::interpret(input, &default, &engines)?;
    let mut url = interp.url().clone();
    if env.profile.settings.privacy.https_only
        && let Some(u) = ferrosurf_core::omnibox::https_upgrade(&url)
    {
        url = u;
    }
    Some(url)
}

pub fn next_zoom(current: f32, up: bool) -> f32 {
    const STEPS: &[f32] = &[
        0.3, 0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
    ];
    if up {
        STEPS
            .iter()
            .copied()
            .find(|s| *s > current + 0.001)
            .unwrap_or(5.0)
    } else {
        STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < current - 0.001)
            .unwrap_or(0.3)
    }
}

pub fn start_download(env: &mut Env, url: &Url, referrer: Option<Url>) {
    let mut headers = Vec::new();
    if matches!(url.scheme(), "http" | "https") {
        let cookies = env
            .servo
            .site_data_manager()
            .cookies_for_url(url.clone(), servo::CookieSource::HTTP);
        if !cookies.is_empty() {
            let header = cookies
                .iter()
                .map(|c| format!("{}={}", c.name(), c.value()))
                .collect::<Vec<_>>()
                .join("; ");
            headers.push(("Cookie".to_string(), header));
        }
        if let Some(r) = referrer.filter(|r| matches!(r.scheme(), "http" | "https")) {
            headers.push(("Referer".to_string(), r.to_string()));
        }
        headers.push(("User-Agent".to_string(), servo::pref!(user_agent)));
    }
    let dir = env.profile.settings.downloads.directory.clone();
    env.profile
        .downloads
        .start(url.as_str(), &dir, None, headers);
    env.shared.toast("Descarga iniciada");
    env.shared.downloads_attention = true;
}

fn load_window_icon() -> Option<winit::window::Icon> {
    let bytes = include_bytes!("../resources/icons/ferrosurf-64.png");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

pub type WakeFn = Arc<dyn Fn() + Send + Sync>;
