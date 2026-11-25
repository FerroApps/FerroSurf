//! Application: owns Servo, the profile, all windows, the MCP server and the assistant.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrosurf_core::session::{SessionState, TabState, WindowState};
use ferrosurf_core::{Profile, ProfileManager};
use ferrosurf_extensions::{Decision as BlockDecision, RequestInfo, ResourceType};
use servo::{
    EmbedderControl, EventLoopWaker, LoadStatus, Opts, Preferences, RenderingContext, Servo,
    ServoBuilder, UserContentManager, UserScript, WebResourceResponse,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::Cli;
use crate::actions::Keymap;
use crate::delegate::{Delegate, EngineEvent};
use crate::internal_pages::{PageContext, SharedPageContext};
use crate::mcp_host::McpHost;
use crate::profile_data::ProfileData;
use crate::tab::TabId;
use crate::ui::SharedUi;
use crate::window::{AppCmd, BrowserWindow, Disposition, Env};
use ferrosurf_mcp::{BrowserCommand, CommandReply};

#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    Wake,
}

#[derive(Clone)]
struct Waker(EventLoopProxy<AppEvent>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send_event(AppEvent::Wake);
    }
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    // The tokio runtime is entered on the UI thread for the whole run: native file
    // dialogs (rfd over the XDG portal, via zbus on tokio) need a reactor there.
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ferrosurf-rt")
            .enable_all()
            .build()?,
    );
    let _runtime_guard = runtime.enter();
    let mut app = App {
        cli,
        runtime: runtime.clone(),
        proxy: event_loop.create_proxy(),
        state: None,
        failed: None,
    };
    let result = event_loop.run_app(&mut app);
    // The event loop (and with it the Wayland/X11 connection) is gone now. Anything still
    // holding windowing resources must not run its destructor: on Wayland the clipboard
    // worker would touch a dead connection and crash. The process is exiting anyway.
    if let Some(state) = app.state.take() {
        std::mem::forget(state);
    }
    result?;
    if let Some(e) = app.failed {
        return Err(e);
    }
    Ok(())
}

pub struct App {
    cli: Cli,
    runtime: Arc<tokio::runtime::Runtime>,
    proxy: EventLoopProxy<AppEvent>,
    state: Option<Running>,
    failed: Option<anyhow::Error>,
}

struct Running {
    servo: Servo,
    delegate: Rc<Delegate>,
    ucm: Rc<UserContentManager>,
    profile: ProfileData,
    windows: Vec<BrowserWindow>,
    focused: Option<WindowId>,
    keymap: Keymap,
    keymap_rev: u64,
    page_ctx: SharedPageContext,
    shared: SharedUi,
    runtime: Arc<tokio::runtime::Runtime>,
    mcp: McpHost,
    user_scripts: Vec<Rc<UserScript>>,
    ext_revision: u64,
    bookmarks_rev: u64,
    last_session_save: Instant,
    last_maintenance: Instant,
    screenshot: Option<ScreenshotJob>,
    quitting: bool,
    pending_app_cmds: Vec<AppCmd>,
    /// Bridge prompts waiting for an asynchronous answer.
    bridge_pending: Vec<(
        servo::PromptDialog,
        std::sync::mpsc::Receiver<Result<serde_json::Value, String>>,
    )>,
    bridge_script: Option<Rc<UserScript>>,
    passkey_script: Option<Rc<UserScript>>,
    plugin_scripts: Vec<Rc<UserScript>>,
    plugin_scripts_key: Vec<(String, String)>,
    /// Last active tab reported to plugins.
    plugin_active: Option<(u64, String, String)>,
}

struct ScreenshotJob {
    path: std::path::PathBuf,
    delay: Duration,
    loaded_at: Option<Instant>,
    started: Instant,
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .creation_flags(0x08000000)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = pid;
        false
    }
}

/// If another FerroSurf process already runs this profile, send it our URLs via its MCP
/// endpoint and return true.
pub fn forward_to_running_instance(cli: &Cli) -> bool {
    let Ok(profiles) = ProfileManager::open_default() else {
        return false;
    };
    let profile = match &cli.profile {
        Some(p) => match profiles.get(p) {
            Some(p) => p.clone(),
            None => return false,
        },
        None => profiles.default_profile().clone(),
    };
    let path = profiles.paths(&profile).mcp_token();
    let Ok(ep) = ferrosurf_mcp::EndpointFile::read(&path) else {
        return false;
    };
    if !pid_alive(ep.pid) {
        return false;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Authorization".to_string(), format!("Bearer {}", ep.token));
        let cfg = ferrosurf_mcp::ClientTransportConfig::Http {
            url: ep.url.clone(),
            headers,
        };
        let Ok(client) = tokio::time::timeout(
            Duration::from_secs(3),
            ferrosurf_mcp::McpClient::connect(cfg),
        )
        .await
        else {
            return false;
        };
        let Ok(client) = client else { return false };
        let urls = if cli.urls.is_empty() {
            vec![String::new()]
        } else {
            cli.urls.clone()
        };
        for u in urls {
            let args = serde_json::json!({"url": u, "background": false});
            if client.call_tool("browser_open_tab", args).await.is_err() {
                return false;
            }
        }
        log::info!("Handed URLs to running FerroSurf (pid {})", ep.pid);
        true
    })
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let mut profiles = ProfileManager::open_default()?;
        let profile: Profile = if self.cli.guest {
            profiles.create_guest()?
        } else if let Some(p) = &self.cli.profile {
            profiles
                .get(p)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Perfil desconocido: {p}"))?
        } else {
            profiles.default_profile().clone()
        };
        if !profile.ephemeral {
            let _ = profiles.touch(&profile.id);
        }
        let proxy = self.proxy.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(AppEvent::Wake);
        });
        let mut profile_data = ProfileData::load(profiles, profile, wake.clone())?;
        for dir in &self.cli.load_extension {
            let already = profile_data
                .extensions
                .list()
                .iter()
                .any(|e| e.root.canonicalize().ok() == dir.canonicalize().ok());
            if already {
                let _ = profile_data.extensions.reload();
            } else {
                match profile_data.extensions.install_unpacked(dir) {
                    Ok(id) => log::info!("Loaded unpacked extension {id} from {}", dir.display()),
                    Err(e) => log::error!("Could not load extension {}: {e:#}", dir.display()),
                }
            }
        }
        profile_data.rebuild_blocker();

        // First window (needs a GL context before Servo starts rendering).
        let size = self.cli.window_size.as_deref().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        });
        let first_window = BrowserWindow::new(
            event_loop,
            &profile_data,
            size,
            profile_data.profile.ephemeral,
        )?;

        // Servo, configured per profile.
        let opts = Opts {
            config_dir: Some(profile_data.paths.engine()),
            temporary_storage: profile_data.profile.ephemeral,
            ..Opts::default()
        };
        let mut prefs = Preferences::default();
        if !profile_data.settings.user_agent.trim().is_empty() {
            prefs.user_agent = profile_data.settings.user_agent.trim().to_string();
        }
        if profile_data.settings.enable_devtools {
            prefs.devtools_server_enabled = true;
            prefs.devtools_server_listen_address =
                format!("127.0.0.1:{}", profile_data.settings.devtools_port);
        }
        let page_ctx: SharedPageContext =
            Arc::new(parking_lot::RwLock::new(PageContext::default()));
        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(prefs)
            .event_loop_waker(Box::new(Waker(self.proxy.clone())))
            .protocol_registry(crate::internal_pages::protocol_registry(page_ctx.clone()))
            .build();
        let delegate = Rc::new(Delegate::default());
        servo.set_delegate(delegate.clone());
        let ucm = Rc::new(UserContentManager::new(&servo));

        let runtime = self.runtime.clone();
        let mcp = McpHost::start(&runtime, &mut profile_data, wake.clone());

        let keymap = Keymap::new(&profile_data.settings.keybindings);
        let mut running = Running {
            servo,
            delegate,
            ucm,
            profile: profile_data,
            windows: vec![first_window],
            focused: None,
            keymap,
            keymap_rev: 0,
            page_ctx,
            shared: SharedUi::default(),
            runtime,
            mcp,
            user_scripts: Vec::new(),
            ext_revision: u64::MAX,
            bookmarks_rev: u64::MAX,
            last_session_save: Instant::now(),
            last_maintenance: Instant::now(),
            screenshot: self.cli.screenshot.clone().map(|path| ScreenshotJob {
                path,
                delay: Duration::from_secs_f32(self.cli.screenshot_delay),
                loaded_at: None,
                started: Instant::now(),
            }),
            quitting: false,
            pending_app_cmds: Vec::new(),
            bridge_pending: Vec::new(),
            bridge_script: None,
            passkey_script: None,
            plugin_scripts: Vec::new(),
            plugin_scripts_key: Vec::new(),
            plugin_active: None,
        };
        running.shared.wake = Some(wake);
        running.shared.mcp_url = running.mcp.url();
        running.sync_extensions();
        running.sync_plugin_scripts();
        running.refresh_page_context();
        running
            .mcp
            .connect_clients(&running.runtime, &running.profile);

        // Initial tabs: command line URLs, restored session or a new tab.
        let mut urls: Vec<Url> = Vec::new();
        for raw in &self.cli.urls {
            let engines = running.profile.settings.search_engines();
            let engine = running.profile.settings.current_search_engine();
            if let Some(i) = ferrosurf_core::omnibox::interpret(raw, &engine, &engines) {
                urls.push(i.url().clone());
            }
        }
        let restore = !self.cli.no_restore
            && self.cli.screenshot.is_none()
            && !running.profile.profile.ephemeral
            && running.profile.settings.startup
                == ferrosurf_core::settings::StartupBehavior::RestoreSession;
        let session = if restore {
            SessionState::load(&running.profile.paths.session())
        } else {
            None
        };
        running.with_window(0, |w, env| {
            let mut restored = false;
            if let Some(session) = &session
                && let Some(ws) = session.windows.first()
            {
                restore_window_tabs(w, env, ws);
                restored = !w.tabs.is_empty();
            }
            if !restored && urls.is_empty() {
                let start = match env.profile.settings.startup {
                    ferrosurf_core::settings::StartupBehavior::Homepage => {
                        Url::parse(&env.profile.settings.homepage).ok()
                    }
                    _ => None,
                };
                w.open_tab(env, start, true, None);
            }
            for u in &urls {
                w.open_tab(env, Some(u.clone()), true, None);
            }
        });
        // Extra windows from the session.
        if let Some(session) = &session {
            for ws in session.windows.iter().skip(1) {
                if let Ok(w) = BrowserWindow::new(
                    event_loop,
                    &running.profile,
                    Some((ws.width, ws.height)),
                    false,
                ) {
                    running.windows.push(w);
                    let idx = running.windows.len() - 1;
                    running.with_window(idx, |w, env| restore_window_tabs(w, env, ws));
                }
            }
        }
        // Mark session as "not cleanly closed" until we exit.
        running.save_session(false);
        self.state = Some(running);
        Ok(())
    }
}

fn restore_window_tabs(w: &mut BrowserWindow, env: &mut Env, ws: &WindowState) {
    for (i, ts) in ws.tabs.iter().enumerate() {
        let Ok(url) = Url::parse(&ts.url) else {
            continue;
        };
        if i == ws.active.min(ws.tabs.len().saturating_sub(1)) {
            let id = w.open_tab(env, Some(url), true, None);
            if let Some(idx) = w.tab_index(id) {
                w.tabs[idx].pinned = ts.pinned;
                w.tabs[idx].zoom = ts.zoom;
            }
        } else {
            // Restore background tabs asleep: they load only when activated.
            let mut tab = crate::tab::Tab::new(Some(url));
            tab.title = ts.title.clone();
            tab.pinned = ts.pinned;
            tab.zoom = if ts.zoom > 0.0 { ts.zoom } else { 1.0 };
            let active_id = w.active_tab().map(|t| t.id);
            w.tabs.push(tab);
            if let Some(a) = active_id.and_then(|a| w.tab_index(a)) {
                w.active = a;
            }
        }
    }
    if ws.maximized {
        w.window.set_maximized(true);
    }
}

impl Running {
    /// Run `f` with a window and an [`Env`] borrowing the rest of the app.
    fn with_window<R>(
        &mut self,
        idx: usize,
        f: impl FnOnce(&mut BrowserWindow, &mut Env) -> R,
    ) -> Option<R> {
        let mut app_cmds = Vec::new();
        let r = {
            let window = self.windows.get_mut(idx)?;
            let mut env = Env {
                servo: &self.servo,
                delegate: &self.delegate,
                ucm: &self.ucm,
                profile: &mut self.profile,
                keymap: &self.keymap,
                shared: &mut self.shared,
                app_cmds: &mut app_cmds,
            };
            f(window, &mut env)
        };
        self.pending_app_cmds.extend(app_cmds);
        Some(r)
    }

    fn window_index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id() == id)
    }

    fn focused_index(&self) -> usize {
        self.focused
            .and_then(|id| self.window_index(id))
            .unwrap_or(0)
    }

    fn find_webview(&self, webview: &servo::WebView) -> Option<(usize, usize)> {
        self.windows
            .iter()
            .enumerate()
            .find_map(|(wi, w)| w.tab_by_webview(webview).map(|ti| (wi, ti)))
    }

    fn request_redraw_all(&mut self) {
        for w in &mut self.windows {
            w.needs_redraw = true;
            w.window.request_redraw();
        }
    }

    /// Publish data used by internal pages (new tab etc).
    fn refresh_page_context(&mut self) {
        let dark = self
            .windows
            .first()
            .map(|w| crate::ui::theme::is_dark(&self.profile, w.window.theme()))
            .unwrap_or(true);
        let engine = self.profile.settings.current_search_engine();
        let top_sites = self
            .profile
            .history
            .top_sites(8)
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                (
                    if e.title.is_empty() {
                        ferrosurf_core::util::pretty_url(&e.url)
                    } else {
                        e.title
                    },
                    e.url,
                )
            })
            .collect();
        let bookmarks = self
            .profile
            .bookmarks
            .toolbar()
            .children()
            .iter()
            .filter_map(|n| match n {
                ferrosurf_core::BookmarkNode::Bookmark(b) => Some((b.title.clone(), b.url.clone())),
                _ => None,
            })
            .take(12)
            .collect();
        let mut ctx = self.page_ctx.write();
        ctx.dark = dark;
        ctx.accent = self.profile.accent_hex();
        ctx.search_name = engine.name.clone();
        ctx.search_url = engine.url.clone();
        ctx.top_sites = top_sites;
        ctx.bookmarks = bookmarks;
        ctx.profile_name = if self.profile.profile.ephemeral {
            String::new()
        } else {
            self.profile.profile.name.clone()
        };
        ctx.data_dir = self.profile.paths.root.display().to_string();
        ctx.mcp_url = self.mcp.url();
        ctx.extensions = self
            .profile
            .extensions
            .list()
            .iter()
            .filter(|e| e.enabled)
            .map(|e| (e.id.clone(), e.root.clone()))
            .collect();
        ctx.newtab_override = self.profile.extensions.newtab_override();
        ctx.favicon_dir = Some(self.profile.favicons.dir().to_path_buf());
        ctx.assistant_enabled = self.profile.settings.assistant.enabled;
        for (_, url) in ctx.top_sites.iter().chain(ctx.bookmarks.iter()) {
            self.profile.favicons.ensure(url);
        }
    }

    /// Re-install extension content scripts when extensions change.
    fn sync_extensions(&mut self) {
        let rev = self.profile.extensions.revision();
        if rev == self.ext_revision {
            return;
        }
        self.ext_revision = rev;
        if self.bridge_script.is_none() {
            let script = Rc::new(UserScript::new(crate::bridge::CLIENT_JS.to_string(), None));
            self.ucm.add_script(script.clone());
            self.bridge_script = Some(script);
            let polyfill = Rc::new(UserScript::new(
                ferrosurf_passwords::WEBAUTHN_POLYFILL_JS.to_string(),
                None,
            ));
            self.ucm.add_script(polyfill.clone());
            self.passkey_script = Some(polyfill);
        }
        for s in self.user_scripts.drain(..) {
            self.ucm.remove_script(s);
        }
        for (id, source) in self.profile.extensions.user_scripts() {
            let script = Rc::new(UserScript::new(source, None));
            log::info!("Installing content scripts of extension {id}");
            self.ucm.add_script(script.clone());
            self.user_scripts.push(script);
        }
        self.profile.rebuild_blocker();
        self.refresh_page_context();
    }

    /// Re-install native plugin content scripts when the enabled set changes.
    fn sync_plugin_scripts(&mut self) {
        let scripts = self.profile.plugins.content_scripts();
        if scripts == self.plugin_scripts_key {
            return;
        }
        for s in self.plugin_scripts.drain(..) {
            self.ucm.remove_script(s);
        }
        for (id, source) in &scripts {
            log::info!("Installing content script of plugin {id}");
            let script = Rc::new(UserScript::new(source.clone(), None));
            self.ucm.add_script(script.clone());
            self.plugin_scripts.push(script);
        }
        self.plugin_scripts_key = scripts;
    }

    /// Tell plugins which tab is active (only when it changes).
    fn sync_plugin_active_tab(&mut self) {
        let active = self.find_tab(None).map(|(wi, ti)| {
            let t = &self.windows[wi].tabs[ti];
            (
                t.id,
                t.url.as_ref().map(|u| u.to_string()).unwrap_or_default(),
                t.title.clone(),
            )
        });
        if active != self.plugin_active {
            self.profile
                .plugins
                .set_active_tab(active.clone().map(|(tab_id, url, title)| {
                    ferrosurf_plugins::sdk::abi::ActiveTab { tab_id, url, title }
                }));
            self.plugin_active = active;
        }
    }

    /// Perform what plugins asked the browser to do.
    fn process_plugin_requests(&mut self) {
        use ferrosurf_plugins::HostRequest;
        let requests = self.profile.plugins.drain_host_requests();
        if requests.is_empty() {
            return;
        }
        for req in requests {
            match req {
                HostRequest::Toast { text, .. } => self.shared.toast(&text),
                HostRequest::Notify { title, body, .. } => {
                    self.shared.toast(&format!("{title}: {body}"))
                }
                HostRequest::OpenUrl {
                    url, background, ..
                } => {
                    let Ok(url) = Url::parse(&url) else { continue };
                    let wi = self.focused_index();
                    self.with_window(wi, |w, env| {
                        w.open_tab(env, Some(url), !background, None);
                    });
                }
                HostRequest::InjectJs { tab_id, code, .. } => {
                    if let Some((wi, ti)) = self.find_tab(Some(tab_id))
                        && let Some(wv) = &self.windows[wi].tabs[ti].webview
                    {
                        wv.evaluate_javascript(code, |_| {});
                    }
                }
                HostRequest::ClipboardWrite { text, .. } => {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text);
                    }
                }
                HostRequest::PluginDisabled { plugin, reason } => self
                    .shared
                    .toast(&format!("Plugin «{plugin}» desactivado: {reason}")),
                HostRequest::SettingChanged { .. } => {}
            }
        }
        self.sync_plugin_scripts();
        self.request_redraw_all();
    }

    fn session_state(&self, clean_exit: bool) -> SessionState {
        let windows = self
            .windows
            .iter()
            .filter(|w| !w.guest)
            .map(|w| {
                let size = w.window.inner_size();
                let scale = w.window.scale_factor();
                WindowState {
                    tabs: w
                        .tabs
                        .iter()
                        .filter_map(|t| {
                            let url = t.url.as_ref()?;
                            if url.scheme() == "about" {
                                return None;
                            }
                            Some(TabState {
                                url: url.to_string(),
                                title: t.title.clone(),
                                pinned: t.pinned,
                                zoom: t.zoom,
                                history: t
                                    .back_forward
                                    .iter()
                                    .rev()
                                    .take(25)
                                    .rev()
                                    .map(|u| u.to_string())
                                    .collect(),
                                history_index: t.history_index,
                                ..Default::default()
                            })
                        })
                        .collect(),
                    active: w.active,
                    width: (size.width as f64 / scale) as u32,
                    height: (size.height as f64 / scale) as u32,
                    x: None,
                    y: None,
                    maximized: w.window.is_maximized(),
                    vertical_tabs_width: None,
                }
            })
            .filter(|w| !w.tabs.is_empty())
            .collect();
        SessionState {
            version: 1,
            windows,
            saved_ms: ferrosurf_core::now_ms(),
            clean_exit,
        }
    }

    fn save_session(&mut self, clean_exit: bool) {
        if self.profile.profile.ephemeral {
            return;
        }
        let state = self.session_state(clean_exit);
        if state.windows.is_empty() && !clean_exit {
            return;
        }
        if let Err(e) = state.save(&self.profile.paths.session()) {
            log::error!("Failed to save session: {e}");
        }
        self.last_session_save = Instant::now();
    }

    // ------------------------------------------------------------- engine events

    fn process_engine_events(&mut self, event_loop: &ActiveEventLoop) {
        loop {
            let events = self.delegate.drain();
            if events.is_empty() {
                break;
            }
            for ev in events {
                self.handle_engine_event(event_loop, ev);
            }
        }
    }

    fn handle_engine_event(&mut self, _event_loop: &ActiveEventLoop, ev: EngineEvent) {
        match ev {
            EngineEvent::NewFrame(wv) => {
                if let Some(w) = self.windows.iter_mut().find(|w| w.is_sidebar_webview(&wv)) {
                    w.needs_redraw = true;
                    w.window.request_redraw();
                } else if let Some((wi, ti)) = self.find_webview(&wv) {
                    let w = &mut self.windows[wi];
                    if ti == w.active {
                        w.needs_redraw = true;
                        w.window.request_redraw();
                    }
                }
            }
            EngineEvent::UrlChanged(wv, url) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = &mut self.windows[wi].tabs[ti];
                    tab.url = Some(url.clone());
                    tab.omnibox_edit = None;
                    self.profile.plugins.notify_navigation(
                        tab.id,
                        url.as_str(),
                        &tab.title,
                        "commit",
                    );
                    self.profile.passwords.cancel_for_tab(tab.id);
                    let typed = std::mem::take(&mut tab.typed_navigation);
                    // The title arrives separately (TitleChanged / load complete).
                    let _ = self.profile.history.record_visit(url.as_str(), "", typed);
                    // Per-site zoom.
                    if let Some(host) = url.host_str() {
                        let z = self.profile.settings.zoom_for_host(host);
                        if (z - tab.zoom).abs() > 0.001 {
                            tab.zoom = z;
                            wv.set_page_zoom(z);
                        }
                    }
                    self.windows[wi].needs_redraw = true;
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::TitleChanged(wv, title) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = &mut self.windows[wi].tabs[ti];
                    tab.title = title.unwrap_or_default();
                    if let Some(u) = &tab.url {
                        if tab.title == "Error loading page" {
                            // Servo's network error page: don't keep failed loads in history.
                            tab.title = "No se pudo cargar la página".into();
                            let _ = self.profile.history.delete_url(u.as_str());
                        } else {
                            let _ = self.profile.history.update_title(u.as_str(), &tab.title);
                        }
                    }
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::StatusText(wv, text) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    self.windows[wi].tabs[ti].status_text = text;
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::LoadStatus(wv, status) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = &mut self.windows[wi].tabs[ti];
                    if status == LoadStatus::Started {
                        tab.blocked_requests = 0;
                        tab.crashed = None;
                        tab.load_progress_start = Some(Instant::now());
                    }
                    tab.load_status = status;
                    tab.can_go_back = wv.can_go_back();
                    tab.can_go_forward = wv.can_go_forward();
                    if status == LoadStatus::Complete {
                        tab.load_progress_start = None;
                        if let (Some(u), Some(t)) = (
                            wv.url(),
                            wv.page_title().filter(|t| t != "Error loading page"),
                        ) {
                            tab.title = t.clone();
                            let _ = self.profile.history.update_title(u.as_str(), &t);
                        }
                        let tab_id = tab.id;
                        self.profile.plugins.notify_navigation(
                            tab_id,
                            tab.url.as_ref().map(|u| u.as_str()).unwrap_or(""),
                            &tab.title,
                            "complete",
                        );
                        self.mcp.on_load_complete(tab_id);
                        if let Some(job) = &mut self.screenshot
                            && job.loaded_at.is_none()
                            && wi == 0
                            && ti == self.windows[0].active
                        {
                            job.loaded_at = Some(Instant::now());
                        }
                    }
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::Cursor(wv, cursor) => {
                if let Some(w) = self.windows.iter().find(|w| w.is_sidebar_webview(&wv)) {
                    if let Some(icon) = crate::input::cursor_icon(cursor) {
                        w.window.set_cursor_visible(true);
                        w.window.set_cursor(icon);
                    }
                } else if let Some((wi, ti)) = self.find_webview(&wv) {
                    let w = &self.windows[wi];
                    if ti == w.active {
                        match crate::input::cursor_icon(cursor) {
                            Some(icon) => {
                                w.window.set_cursor_visible(true);
                                w.window.set_cursor(icon);
                            }
                            None => w.window.set_cursor_visible(false),
                        }
                    }
                }
            }
            EngineEvent::Favicon(wv) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tex = wv.favicon().and_then(|img| favicon_image(&img)).map(|ci| {
                        self.windows[wi].egui_ctx.load_texture(
                            format!("favicon-{}", self.windows[wi].tabs[ti].id),
                            ci,
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    if let (Some(img), Some(u)) =
                        (wv.favicon().and_then(|i| favicon_image(&i)), wv.url())
                    {
                        let bytes: Vec<u8> = img
                            .pixels
                            .iter()
                            .flat_map(|c| c.to_srgba_unmultiplied())
                            .collect();
                        self.profile.favicons.store_rgba(
                            u.as_str(),
                            img.size[0] as u32,
                            img.size[1] as u32,
                            &bytes,
                        );
                    }
                    self.windows[wi].tabs[ti].favicon = tex;
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::History(wv, entries, current) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = &mut self.windows[wi].tabs[ti];
                    tab.can_go_back = current > 0;
                    tab.can_go_forward = current + 1 < entries.len();
                    tab.back_forward = entries;
                    tab.history_index = current;
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::Closed(wv) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    self.with_window(wi, |w, env| w.close_tab(env, ti));
                }
            }
            EngineEvent::Crashed(wv, reason) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    log::error!("WebView crashed: {reason}");
                    self.windows[wi].tabs[ti].crashed = Some(reason);
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::Fullscreen(wv, fs) => {
                if let Some((wi, _)) = self.find_webview(&wv) {
                    let w = &mut self.windows[wi];
                    w.fullscreen_by_page = fs;
                    w.window
                        .set_fullscreen(fs.then_some(winit::window::Fullscreen::Borderless(None)));
                }
            }
            EngineEvent::Navigation(wv, request) => self.handle_navigation(wv, request),
            EngineEvent::Unload(_, req) => req.allow(),
            EngineEvent::CreateNew(parent, request) => {
                let Some((wi, ti)) = self.find_webview(&parent) else {
                    return;
                };
                let opener = self.windows[wi].tabs[ti].id;
                let rc = self.windows[wi].offscreen.clone() as Rc<dyn RenderingContext>;
                let webview = request
                    .builder(rc)
                    .delegate(self.delegate.clone())
                    .user_content_manager(self.ucm.clone())
                    .hidpi_scale_factor(euclid::Scale::new(self.windows[wi].scale()))
                    .build();
                self.with_window(wi, |w, env| {
                    w.adopt_webview(env, webview, Some(opener), true)
                });
            }
            EngineEvent::Permission(wv, request) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let origin = self.windows[wi].tabs[ti]
                        .url
                        .as_ref()
                        .map(ferrosurf_core::permissions::PermissionStore::origin_of)
                        .unwrap_or_default();
                    let kind = crate::ui::dialogs::permission_kind(request.feature());
                    match self.profile.permissions.get(&origin, &kind) {
                        ferrosurf_core::permissions::Decision::Allow => request.allow(),
                        ferrosurf_core::permissions::Decision::Deny => request.deny(),
                        ferrosurf_core::permissions::Decision::Ask => {
                            let tab_id = self.windows[wi].tabs[ti].id;
                            self.windows[wi].chrome.permission_prompts.push(
                                crate::ui::PermissionPrompt {
                                    tab: tab_id,
                                    origin,
                                    kind,
                                    request: Some(request),
                                },
                            );
                            self.windows[wi].window.request_redraw();
                        }
                    }
                }
            }
            EngineEvent::Authentication(wv, request) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = self.windows[wi].tabs[ti].id;
                    self.windows[wi]
                        .chrome
                        .auth_prompts
                        .push(crate::ui::AuthPrompt::new(tab, request));
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::ShowControl(wv, control) => {
                if let Some((wi, ti)) = self.find_webview(&wv) {
                    let tab = self.windows[wi].tabs[ti].id;
                    match control {
                        EmbedderControl::FilePicker(mut picker) => {
                            let mut dialog = rfd::FileDialog::new();
                            for f in picker.filter_patterns() {
                                let ext = f.0.trim_start_matches('.').to_string();
                                if !ext.is_empty() {
                                    dialog = dialog.add_filter(ext.clone(), &[ext]);
                                }
                            }
                            let paths = if picker.allow_select_multiple() {
                                dialog.pick_files().unwrap_or_default()
                            } else {
                                dialog.pick_file().into_iter().collect()
                            };
                            if paths.is_empty() {
                                picker.dismiss();
                            } else {
                                picker.select(&paths);
                                picker.submit();
                            }
                        }
                        EmbedderControl::InputMethod(_) => {}
                        EmbedderControl::SimpleDialog(servo::SimpleDialog::Prompt(prompt))
                            if prompt.message().starts_with(crate::bridge::PREFIX) =>
                        {
                            let origin = self.windows[wi].tabs[ti]
                                .url
                                .as_ref()
                                .map(ferrosurf_core::permissions::PermissionStore::origin_of)
                                .unwrap_or_default();
                            self.handle_bridge(prompt, origin, tab);
                        }
                        other => {
                            self.windows[wi].controls.push((tab, other));
                            self.windows[wi].window.request_redraw();
                        }
                    }
                }
            }
            EngineEvent::HideControl(wv, id) => {
                if let Some((wi, _)) = self.find_webview(&wv) {
                    self.windows[wi].controls.retain(|(_, c)| c.id() != id);
                    self.windows[wi].window.request_redraw();
                }
            }
            EngineEvent::WebResource(wv, load) => self.handle_web_resource(wv, load),
            EngineEvent::Notification(_, n) => {
                self.shared.toast(&format!("🔔 {} — {}", n.title, n.body));
                self.request_redraw_all();
            }
            EngineEvent::Console(wv, level, msg) => {
                log::debug!(target: "console", "[{level:?}] {msg}");
                if let Some((wi, ti)) = wv.as_ref().and_then(|w| self.find_webview(w)) {
                    let level = format!("{level:?}").to_lowercase();
                    self.windows[wi].tabs[ti].push_console(&level, msg);
                    if self.windows[wi].chrome.devtools_open {
                        self.windows[wi].window.request_redraw();
                    }
                }
            }
            EngineEvent::ResizeTo(_) => {}
            EngineEvent::DevtoolsStarted(port, token) => {
                log::info!("DevTools server on port {port} (token {token})");
                self.shared.devtools = Some((port, token));
            }
            EngineEvent::DevtoolsConnection(req) => req.allow(),
            EngineEvent::Error(e) => log::error!("Servo error: {e}"),
        }
    }

    fn handle_navigation(&mut self, wv: servo::WebView, request: servo::NavigationRequest) {
        let url = request.url.clone();
        let loc = self.find_webview(&wv);
        // External protocols.
        if matches!(
            url.scheme(),
            "mailto" | "tel" | "sms" | "magnet" | "irc" | "steam" | "zoommtg" | "slack" | "spotify"
        ) {
            request.deny();
            open_external(url.as_str());
            return;
        }
        // Native internal pages.
        if url.scheme() == "ferro"
            && let Some(host) = url.host_str()
        {
            let action = match host {
                "settings" => Some(crate::actions::Action::ShowSettings),
                "history" => Some(crate::actions::Action::ShowHistory),
                "bookmarks" => Some(crate::actions::Action::ShowBookmarks),
                "downloads" => Some(crate::actions::Action::ShowDownloads),
                "extensions" => Some(crate::actions::Action::ShowExtensions),
                _ => None,
            };
            if let (Some(a), Some((wi, _))) = (action, loc) {
                request.deny();
                self.with_window(wi, |w, env| w.run_action(env, a));
                return;
            }
        }
        // Extension packages: install instead of downloading.
        if matches!(
            crate::store::detect(&url),
            Some(crate::store::StoreItem::Direct { .. })
        ) {
            request.deny();
            self.pending_app_cmds
                .push(AppCmd::InstallFromStore(crate::store::StoreItem::Direct {
                    url: url.clone(),
                }));
            return;
        }
        // Likely downloads (Servo can't render these).
        if is_download_url(&url) {
            request.deny();
            if let Some((wi, _)) = loc {
                let referrer = wv.url();
                self.with_window(wi, |_w, env| {
                    crate::window::start_download(env, &url, referrer)
                });
            }
            self.request_redraw_all();
            return;
        }
        // HTTPS-only mode.
        if self.profile.settings.privacy.https_only
            && let Some(https) = ferrosurf_core::omnibox::https_upgrade(&url)
        {
            request.deny();
            wv.load(https);
            return;
        }
        request.allow();
    }

    fn handle_web_resource(&mut self, wv: Option<servo::WebView>, load: servo::WebResourceLoad) {
        let req = load.request();
        let url: Url = req.url.clone().into_url();
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
            return; // dropping `load` lets it proceed
        }
        let resource_type = resource_type(&format!("{:?}", req.destination), req.is_for_main_frame);
        let initiator: Option<Url> = req
            .referrer_url
            .clone()
            .map(|u| u.into_url())
            .or_else(|| wv.as_ref().and_then(|w| w.url()));
        let info = RequestInfo {
            url: &url,
            initiator: initiator.as_ref(),
            resource_type,
        };
        let mut decision = self.profile.blocker.read().check(&info);
        if matches!(decision, BlockDecision::Allow) {
            decision = self.profile.plugins.check_request(&info).into();
        }
        if let Some((wi, ti)) = wv.as_ref().and_then(|w| self.find_webview(w)) {
            let tab = &mut self.windows[wi].tabs[ti];
            if req.is_for_main_frame && resource_type == ResourceType::MainFrame {
                tab.network.clear();
                tab.console.clear();
            }
            tab.push_network(crate::tab::NetEntry {
                url: url.to_string(),
                method: req.method.to_string(),
                kind: format!("{resource_type:?}").to_lowercase(),
                main_frame: req.is_for_main_frame,
                blocked: matches!(decision, BlockDecision::Block),
                time: std::time::SystemTime::now(),
            });
        }
        match decision {
            BlockDecision::Block => {
                log::debug!("Blocked {url}");
                if let Some((wi, ti)) = wv.as_ref().and_then(|w| self.find_webview(w)) {
                    self.windows[wi].tabs[ti].blocked_requests += 1;
                }
                self.shared.blocked_total += 1;
                load.intercept(
                    WebResourceResponse::new(url).status_code(http::StatusCode::FORBIDDEN),
                )
                .cancel();
            }
            BlockDecision::UpgradeScheme if url.scheme() == "http" => {
                let mut u = url.clone();
                let _ = u.set_scheme("https");
                let resp = WebResourceResponse::new(url)
                    .status_code(http::StatusCode::TEMPORARY_REDIRECT)
                    .headers({
                        let mut h = http::HeaderMap::new();
                        if let Ok(v) = http::HeaderValue::from_str(u.as_str()) {
                            h.insert(http::header::LOCATION, v);
                        }
                        h
                    });
                load.intercept(resp).finish();
            }
            BlockDecision::Redirect(target) => {
                let resp = WebResourceResponse::new(url)
                    .status_code(http::StatusCode::TEMPORARY_REDIRECT)
                    .headers({
                        let mut h = http::HeaderMap::new();
                        if let Ok(v) = http::HeaderValue::from_str(target.as_str()) {
                            h.insert(http::header::LOCATION, v);
                        }
                        h
                    });
                load.intercept(resp).finish();
            }
            _ => {}
        }
    }

    fn process_app_cmds(&mut self, event_loop: &ActiveEventLoop) {
        while !self.pending_app_cmds.is_empty() {
            let cmds: Vec<AppCmd> = std::mem::take(&mut self.pending_app_cmds);
            for cmd in cmds {
                self.handle_app_cmd(event_loop, cmd);
            }
        }
    }

    fn handle_app_cmd(&mut self, event_loop: &ActiveEventLoop, cmd: AppCmd) {
        match cmd {
            AppCmd::NewWindow { urls } => match BrowserWindow::new(
                event_loop,
                &self.profile,
                None,
                self.profile.profile.ephemeral,
            ) {
                Ok(w) => {
                    self.windows.push(w);
                    let idx = self.windows.len() - 1;
                    self.with_window(idx, |w, env| {
                        if urls.is_empty() {
                            w.open_tab(env, None, true, None);
                        }
                        for u in urls {
                            w.open_tab(env, Some(u), true, None);
                        }
                    });
                }
                Err(e) => log::error!("Could not open window: {e}"),
            },
            AppCmd::NewGuestWindow => spawn_self(&["--guest"]),
            AppCmd::OpenProfile(id) => spawn_self(&["--profile", &id]),
            AppCmd::CloseWindow(id) => {
                if let Some(i) = self.window_index(id) {
                    if self.windows.len() == 1 {
                        self.quit(event_loop);
                    } else {
                        let mut w = self.windows.remove(i);
                        w.destroy();
                    }
                }
            }
            AppCmd::Quit => self.quit(event_loop),
            AppCmd::ExtensionsChanged => {
                self.sync_extensions();
                self.sync_plugin_scripts();
            }
            AppCmd::SettingsChanged => {
                self.profile.flush_settings();
                self.keymap = Keymap::new(&self.profile.settings.keybindings);
                self.profile.rebuild_blocker();
                self.refresh_page_context();
                self.mcp.update_policy(&self.profile);
                for w in &mut self.windows {
                    crate::ui::theme::apply(&w.egui_ctx, &self.profile, w.window.theme());
                }
                self.request_redraw_all();
            }
            AppCmd::ClearBrowsingData {
                history,
                cookies,
                cache,
            } => {
                if history {
                    let _ = self.profile.history.clear();
                    self.profile.closed_tabs = Default::default();
                }
                if cookies {
                    self.servo.site_data_manager().clear_cookies(None);
                    self.servo
                        .site_data_manager()
                        .clear_site_data(&[], servo::StorageType::all());
                }
                if cache {
                    self.servo.network_manager().clear_cache();
                }
                self.refresh_page_context();
                self.shared.toast("Datos de navegación borrados");
            }
            AppCmd::StartAssistant { text } => {
                let idx = self.focused_index();
                let page = self.windows.get(idx).and_then(|w| w.active_tab()).map(|t| {
                    format!(
                        "Pestaña activa: «{}» — {}",
                        t.display_title(),
                        t.url_string()
                    )
                });
                self.mcp.start_assistant(
                    &self.runtime,
                    &self.profile,
                    &mut self.shared,
                    text,
                    page,
                );
            }
            AppCmd::StopAssistant => self.mcp.stop_assistant(&mut self.shared),
            AppCmd::ResolveMcpApproval { allow, always } => {
                if !self.shared.mcp_approvals.is_empty() {
                    let p = self.shared.mcp_approvals.remove(0);
                    if always {
                        self.shared.mcp_trusted_session = true;
                    }
                    if allow {
                        if let Some((reply, res)) = self.exec_mcp_command(p.cmd, p.reply) {
                            let _ = reply.send(res);
                        }
                    } else {
                        let _ = p
                            .reply
                            .send(Err("El usuario ha denegado esta acción".into()));
                    }
                }
                self.request_redraw_all();
            }
            AppCmd::InstallFromStore(item) => {
                let label = match &item {
                    crate::store::StoreItem::Chrome { id } => id.clone(),
                    crate::store::StoreItem::Firefox { slug } => slug.clone(),
                    crate::store::StoreItem::Direct { url } => url.to_string(),
                };
                if !self.profile.store_busy.contains(&label) {
                    self.profile.store_busy.push(label.clone());
                    self.shared.toast(&format!(
                        "Descargando la extensión desde {}…",
                        item.store_name()
                    ));
                    let results = self.profile.store_results.clone();
                    let wake = self.shared.wake.clone();
                    std::thread::spawn(move || {
                        let res = crate::store::download(&item).map_err(|e| format!("{e:#}"));
                        results.lock().push((label, res));
                        if let Some(wake) = wake {
                            wake();
                        }
                    });
                }
                self.request_redraw_all();
            }
            AppCmd::InstallExtensionFile(path) => self.install_extension_file(&path, None),
            AppCmd::ReconnectMcpClients => self.mcp.connect_clients(&self.runtime, &self.profile),
            AppCmd::RestartMcpServer => {
                self.mcp.restart_server(&self.runtime, &mut self.profile);
                self.shared.mcp_url = self.mcp.url();
                self.refresh_page_context();
            }
        }
    }

    /// Install a downloaded/picked extension package and report the result.
    fn install_extension_file(&mut self, path: &std::path::Path, label: Option<&str>) {
        // A malformed archive must never take the browser down.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.profile.extensions.install_from_archive(path)
        }));
        match res {
            Ok(Ok(id)) => {
                let (name, notes) = match self.profile.extensions.get(&id) {
                    Some(ext) => {
                        let raw: serde_json::Value = std::fs::read(ext.root.join("manifest.json"))
                            .ok()
                            .and_then(|b| serde_json::from_slice(&b).ok())
                            .unwrap_or_default();
                        (
                            ext.manifest.name.clone(),
                            crate::store::unsupported_features(&ext.manifest, &raw),
                        )
                    }
                    None => (id.clone(), Vec::new()),
                };
                if notes.is_empty() {
                    self.shared.toast(&format!(
                        "🧩 «{name}» instalada. Recarga las páginas para activarla."
                    ));
                } else {
                    self.shared.toast(&format!(
                        "🧩 «{name}» instalada. Funcionan sus scripts de página y reglas de red; aún no: {}.",
                        notes.join(", ")
                    ));
                }
                self.sync_extensions();
            }
            Ok(Err(e)) => self
                .shared
                .toast(&format!("No se pudo instalar la extensión: {e:#}")),
            Err(_) => self
                .shared
                .toast("No se pudo instalar la extensión: el archivo está dañado"),
        }
        if let Some(label) = label {
            self.profile.store_busy.retain(|l| l != label);
        }
        self.request_redraw_all();
    }

    /// Answer a bridge call from a page/injected script (see `bridge.rs`).
    fn handle_bridge(&mut self, mut prompt: servo::PromptDialog, origin: String, tab: TabId) {
        let reply = match crate::bridge::parse(prompt.message()) {
            Some(Ok(call)) => self.route_bridge(call, &origin, tab),
            Some(Err(e)) => crate::bridge::Reply::Now(serde_json::json!({ "__error": e })),
            None => return,
        };
        match reply {
            crate::bridge::Reply::Now(v) => {
                prompt.set_current_value(&v.to_string());
                prompt.confirm();
            }
            crate::bridge::Reply::Later(rx) => self.bridge_pending.push((prompt, rx)),
        }
    }

    fn route_bridge(
        &mut self,
        call: crate::bridge::BridgeCall,
        origin: &str,
        tab: TabId,
    ) -> crate::bridge::Reply {
        use crate::bridge::Reply;
        if let Some(plugin) = call.channel.strip_prefix("plugin:") {
            return Reply::Later(self.profile.plugins.page_message(
                plugin,
                origin,
                tab,
                call.payload,
            ));
        }
        match call.channel.as_str() {
            "webauthn" => {
                let reply = self
                    .profile
                    .passwords
                    .handle_bridge(call.payload, origin, tab);
                self.request_redraw_all();
                reply
            }
            "ping" => Reply::Now(
                serde_json::json!({ "pong": true, "origin": origin, "version": env!("CARGO_PKG_VERSION") }),
            ),
            other => {
                Reply::Now(serde_json::json!({ "__error": format!("canal desconocido: {other}") }))
            }
        }
    }

    fn poll_bridge(&mut self) {
        let mut i = 0;
        while i < self.bridge_pending.len() {
            match self.bridge_pending[i].1.try_recv() {
                Ok(res) => {
                    let (mut prompt, _) = self.bridge_pending.remove(i);
                    let text = match res {
                        Ok(v) => v.to_string(),
                        Err(e) => crate::bridge::error(e),
                    };
                    prompt.set_current_value(&text);
                    prompt.confirm();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let (mut prompt, _) = self.bridge_pending.remove(i);
                    prompt.set_current_value(&crate::bridge::error("cancelado"));
                    prompt.confirm();
                }
            }
        }
    }

    fn poll_store_installs(&mut self) {
        let done: Vec<_> = std::mem::take(&mut *self.profile.store_results.lock());
        for (label, res) in done {
            match res {
                Ok(path) => {
                    self.install_extension_file(&path, Some(&label));
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => {
                    self.profile.store_busy.retain(|l| *l != label);
                    self.shared
                        .toast(&format!("No se pudo descargar la extensión: {e}"));
                    self.request_redraw_all();
                }
            }
        }
    }

    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.save_session(true);
        self.profile.on_exit();
        if self.profile.settings.privacy.clear_cookies_on_exit {
            self.servo.site_data_manager().clear_cookies(None);
        }
        self.mcp.shutdown(&self.profile);
        for w in &mut self.windows {
            w.destroy();
        }
        self.windows.clear();
        event_loop.exit();
    }

    /// Periodic housekeeping.
    fn maintenance(&mut self) {
        self.mcp.expire_pending();
        if self.last_session_save.elapsed() > Duration::from_secs(20) {
            self.save_session(false);
        }
        if self.last_maintenance.elapsed() > Duration::from_secs(30) {
            self.last_maintenance = Instant::now();
            let minutes = self.profile.settings.tab_sleep_minutes;
            for w in &mut self.windows {
                w.sleep_idle_tabs(minutes);
            }
        }
        if self.keymap_rev != self.shared.keymap_revision {
            self.keymap_rev = self.shared.keymap_revision;
            self.keymap = Keymap::new(&self.profile.settings.keybindings);
        }
        let brev = self.profile.bookmarks.revision();
        if brev != self.bookmarks_rev {
            self.bookmarks_rev = brev;
            self.refresh_page_context();
        }
        self.profile.flush_settings();
    }

    fn find_tab(&self, id: Option<TabId>) -> Option<(usize, usize)> {
        match id {
            Some(id) => self
                .windows
                .iter()
                .enumerate()
                .find_map(|(wi, w)| w.tab_index(id).map(|ti| (wi, ti))),
            None => {
                let wi = self.focused_index();
                let w = self.windows.get(wi)?;
                (!w.tabs.is_empty()).then_some((wi, w.active))
            }
        }
    }

    /// Execute commands coming from MCP agents (external or the built-in assistant).
    fn process_mcp_commands(&mut self) {
        self.mcp.expire_pending();
        for ((cmd, reply), external) in self.mcp.take_commands() {
            if external
                && self.profile.settings.mcp_server.confirm_actions
                && !self.shared.mcp_trusted_session
                && let Some(description) = crate::mcp_host::describe_mutation(&cmd)
            {
                self.shared
                    .mcp_approvals
                    .push(crate::mcp_host::PendingApproval {
                        description,
                        cmd,
                        reply,
                    });
                self.request_redraw_all();
                continue;
            }
            let result = self.exec_mcp_command(cmd, reply);
            if let Some((reply, res)) = result {
                let _ = reply.send(res);
            }
        }
    }

    /// Returns `Some((reply, result))` when the reply is immediate.
    fn exec_mcp_command(
        &mut self,
        cmd: BrowserCommand,
        reply: tokio::sync::oneshot::Sender<CommandReply>,
    ) -> Option<(tokio::sync::oneshot::Sender<CommandReply>, CommandReply)> {
        use serde_json::json;
        let no_tab =
            || Err::<serde_json::Value, String>("No hay ninguna pestaña con ese id".to_string());
        let res: CommandReply = match cmd {
            BrowserCommand::ListTabs => {
                let focused = self.focused_index();
                let tabs: Vec<_> = self
                    .windows
                    .iter()
                    .enumerate()
                    .flat_map(|(wi, w)| {
                        w.tabs.iter().enumerate().map(move |(ti, t)| {
                            json!({
                                "id": t.id,
                                "title": t.display_title(),
                                "url": t.url_string(),
                                "active": ti == w.active && wi == focused,
                                "loading": t.is_loading(),
                                "pinned": t.pinned,
                                "window": wi,
                            })
                        })
                    })
                    .collect();
                Ok(json!({ "tabs": tabs }))
            }
            BrowserCommand::OpenTab { url, background } => {
                let wi = self.focused_index();
                let r = self.with_window(wi, |w, env| {
                    let target = if url.trim().is_empty() {
                        None
                    } else {
                        crate::window::interpret_input(env, &url)
                    };
                    let id = w.open_tab(env, target, !background, None);
                    w.window.focus_window();
                    id
                });
                match r {
                    Some(id) => Ok(json!({ "tab_id": id })),
                    None => Err("No hay ventanas abiertas".into()),
                }
            }
            BrowserCommand::CloseTab { tab_id } => match self.find_tab(Some(tab_id)) {
                Some((wi, ti)) => {
                    self.with_window(wi, |w, env| w.close_tab(env, ti));
                    Ok(json!({ "closed": true }))
                }
                None => no_tab(),
            },
            BrowserCommand::ActivateTab { tab_id } => match self.find_tab(Some(tab_id)) {
                Some((wi, ti)) => {
                    self.with_window(wi, |w, env| {
                        w.activate(env, ti);
                        w.window.focus_window();
                    });
                    Ok(json!({ "activated": true }))
                }
                None => no_tab(),
            },
            BrowserCommand::Navigate { tab_id, url } => match self.find_tab(tab_id) {
                Some((wi, ti)) => {
                    let r = self.with_window(wi, |w, env| {
                        let target = crate::window::interpret_input(env, &url)?;
                        w.ensure_awake(env, ti);
                        let tab = &mut w.tabs[ti];
                        if let Some(wv) = &tab.webview {
                            wv.load(target.clone());
                        }
                        tab.url = Some(target.clone());
                        tab.load_status = LoadStatus::Started;
                        Some(target)
                    });
                    match r.flatten() {
                        Some(u) => {
                            Ok(json!({ "url": u.as_str(), "tab_id": self.windows[wi].tabs[ti].id }))
                        }
                        None => Err("Dirección no válida".into()),
                    }
                }
                None => no_tab(),
            },
            BrowserCommand::GoBack { tab_id }
            | BrowserCommand::GoForward { tab_id }
            | BrowserCommand::Reload { tab_id }
                if self.find_tab(tab_id).is_none() =>
            {
                no_tab()
            }
            BrowserCommand::GoBack { tab_id } => {
                let (wi, ti) = self.find_tab(tab_id)?;
                let tab = &mut self.windows[wi].tabs[ti];
                match &tab.webview {
                    Some(wv) if wv.can_go_back() => {
                        wv.go_back(1);
                        tab.load_status = LoadStatus::Started;
                        Ok(json!({ "ok": true }))
                    }
                    _ => Err("No se puede ir atrás".into()),
                }
            }
            BrowserCommand::GoForward { tab_id } => {
                let (wi, ti) = self.find_tab(tab_id)?;
                let tab = &mut self.windows[wi].tabs[ti];
                match &tab.webview {
                    Some(wv) if wv.can_go_forward() => {
                        wv.go_forward(1);
                        tab.load_status = LoadStatus::Started;
                        Ok(json!({ "ok": true }))
                    }
                    _ => Err("No se puede ir adelante".into()),
                }
            }
            BrowserCommand::Reload { tab_id } => {
                let (wi, ti) = self.find_tab(tab_id)?;
                let tab = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &tab.webview {
                    wv.reload();
                    tab.load_status = LoadStatus::Started;
                }
                Ok(json!({ "ok": true }))
            }
            BrowserCommand::WaitForLoad { tab_id, timeout_ms } => match self.find_tab(tab_id) {
                Some((wi, ti)) => {
                    let tab = &self.windows[wi].tabs[ti];
                    if tab.is_loading() {
                        self.mcp.pending_loads.push(crate::mcp_host::PendingLoad {
                            tab: tab.id,
                            deadline: Instant::now() + Duration::from_millis(timeout_ms.max(100)),
                            reply,
                        });
                        return None;
                    }
                    Ok(
                        json!({ "loaded": true, "title": tab.display_title(), "url": tab.url_string() }),
                    )
                }
                None => no_tab(),
            },
            BrowserCommand::EvaluateJs { tab_id, script } => match self.find_tab(tab_id) {
                Some((wi, ti)) => {
                    self.with_window(wi, |w, env| w.ensure_awake(env, ti));
                    match &self.windows[wi].tabs[ti].webview {
                        Some(wv) => {
                            wv.evaluate_javascript(script, move |res| {
                                let r = match res {
                                    Ok(v) => Ok(crate::mcp_host::js_to_json(v)),
                                    Err(e) => Err(format!("Error de JavaScript: {e:?}")),
                                };
                                let _ = reply.send(r);
                            });
                            return None;
                        }
                        None => Err("La pestaña no está cargada".into()),
                    }
                }
                None => no_tab(),
            },
            BrowserCommand::Screenshot { tab_id } => match self.find_tab(tab_id) {
                Some((wi, ti)) => {
                    // Screenshots need the tab to be painted: bring it to front.
                    self.with_window(wi, |w, env| w.activate(env, ti));
                    match &self.windows[wi].tabs[ti].webview {
                        Some(wv) => {
                            wv.take_screenshot(None, move |res| {
                                let r = match res {
                                    Ok(img) => {
                                        let mut png = Vec::new();
                                        match img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png) {
                                            Ok(()) => {
                                                use base64::Engine;
                                                Ok(json!({ "png_base64": base64::engine::general_purpose::STANDARD.encode(&png) }))
                                            },
                                            Err(e) => Err(format!("PNG: {e}")),
                                        }
                                    },
                                    Err(e) => Err(format!("No se pudo capturar: {e:?}")),
                                };
                                let _ = reply.send(r);
                            });
                            return None;
                        }
                        None => Err("La pestaña no está cargada".into()),
                    }
                }
                None => no_tab(),
            },
            BrowserCommand::SearchHistory { query, limit } => {
                let items: Vec<_> = if query.trim().is_empty() {
                    self.profile
                        .history
                        .visits("", None, limit.max(1))
                        .unwrap_or_default()
                        .into_iter()
                        .map(
                            |v| json!({ "url": v.url, "title": v.title, "visited_ms": v.visit_ms }),
                        )
                        .collect()
                } else {
                    self.profile
                        .history
                        .suggest(&query, limit.max(1))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(_, e)| json!({ "url": e.url, "title": e.title, "visits": e.visit_count, "last_visit_ms": e.last_visit_ms }))
                        .collect()
                };
                Ok(json!({ "results": items }))
            }
            BrowserCommand::SearchBookmarks { query, limit } => {
                let items: Vec<_> = if query.trim().is_empty() {
                    self.profile
                        .bookmarks
                        .all()
                        .into_iter()
                        .take(limit.max(1))
                        .map(|b| json!({ "url": b.url, "title": b.title, "tags": b.tags }))
                        .collect()
                } else {
                    self.profile
                        .bookmarks
                        .search(&query, limit.max(1))
                        .into_iter()
                        .map(|(_, b)| json!({ "url": b.url, "title": b.title, "tags": b.tags }))
                        .collect()
                };
                Ok(json!({ "results": items }))
            }
            BrowserCommand::AddBookmark { url, title } => {
                let active = self.find_tab(None).map(|(wi, ti)| {
                    let t = &self.windows[wi].tabs[ti];
                    (t.url_string(), t.display_title())
                });
                let url = url
                    .or_else(|| active.as_ref().map(|a| a.0.clone()))
                    .unwrap_or_default();
                let title = title
                    .or_else(|| active.map(|a| a.1))
                    .unwrap_or_else(|| url.clone());
                if url.is_empty() {
                    Err("No hay URL que guardar".into())
                } else {
                    match self.profile.bookmarks.add(
                        ferrosurf_core::bookmarks::TOOLBAR_ID,
                        &title,
                        &url,
                    ) {
                        Ok(id) => Ok(json!({ "bookmark_id": id })),
                        Err(e) => Err(e.to_string()),
                    }
                }
            }
        };
        self.request_redraw_all();
        Some((reply, res))
    }

    fn check_screenshot(&mut self, event_loop: &ActiveEventLoop) {
        let Some(job) = &self.screenshot else { return };
        let ready = match job.loaded_at {
            Some(t) => t.elapsed() >= job.delay,
            None => job.started.elapsed() > Duration::from_secs(30),
        };
        if !ready {
            return;
        }
        let path = job.path.clone();
        self.screenshot = None;
        if let Some(w) = self.windows.first_mut() {
            // Draw one more frame, then read back the window's framebuffer.
            let mut app_cmds = Vec::new();
            let mut env = Env {
                servo: &self.servo,
                delegate: &self.delegate,
                ucm: &self.ucm,
                profile: &mut self.profile,
                keymap: &self.keymap,
                shared: &mut self.shared,
                app_cmds: &mut app_cmds,
            };
            w.redraw(&mut env);
            let size = w.window.inner_size();
            let rect = servo::DeviceIntRect::from_size(servo::DeviceIntSize::new(
                size.width as i32,
                size.height as i32,
            ));
            match w.rendering_context.read_to_image(rect) {
                Some(img) => match img.save(&path) {
                    Ok(()) => println!("Screenshot saved to {}", path.display()),
                    Err(e) => eprintln!("Could not save screenshot: {e}"),
                },
                None => eprintln!("Could not read framebuffer"),
            }
        }
        self.quit(event_loop);
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none()
            && self.failed.is_none()
            && let Err(e) = self.init(event_loop)
        {
            log::error!("Startup failed: {e:#}");
            self.failed = Some(e);
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: AppEvent) {
        let Some(state) = &mut self.state else { return };
        state.servo.spin_event_loop();
        state.process_engine_events(event_loop);
        state.process_mcp_commands();
        state.poll_store_installs();
        state.poll_bridge();
        if state.profile.passwords.poll() {
            state.request_redraw_all();
        }
        state.process_plugin_requests();
        state.sync_plugin_active_tab();
        if state.mcp.poll_assistant(&mut state.shared) || state.profile.downloads.active_count() > 0
        {
            state.request_redraw_all();
        }
        state.process_app_cmds(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &mut self.state else { return };
        state.servo.spin_event_loop();
        state.process_engine_events(event_loop);
        let Some(idx) = state.window_index(window_id) else {
            return;
        };
        match &event {
            WindowEvent::CloseRequested => {
                if state.windows.len() == 1 {
                    state.quit(event_loop);
                } else {
                    let mut w = state.windows.remove(idx);
                    w.destroy();
                }
                return;
            }
            WindowEvent::Focused(true) => state.focused = Some(window_id),
            WindowEvent::RedrawRequested => {
                state.with_window(idx, |w, env| w.redraw(env));
                state.process_app_cmds(event_loop);
                state.check_screenshot(event_loop);
                return;
            }
            _ => {}
        }
        state.with_window(idx, |w, env| w.handle_window_event(env, &event));
        if let Some(w) = state.windows.get(idx)
            && w.needs_redraw
        {
            w.window.request_redraw();
        }
        state.process_app_cmds(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = &mut self.state else { return };
        state.maintenance();
        state.sync_plugin_active_tab();
        state.process_app_cmds(event_loop);
        let mut next = Instant::now() + Duration::from_secs(5);
        for w in &mut state.windows {
            if let Some(t) = w.wants_repaint_at() {
                if t <= Instant::now() {
                    w.window.request_redraw();
                } else {
                    next = next.min(t);
                }
            }
            if w.tabs.iter().any(|t| t.is_loading()) {
                next = next.min(Instant::now() + Duration::from_millis(100));
            }
        }
        if state.screenshot.is_some() {
            next = next.min(Instant::now() + Duration::from_millis(100));
            state.check_screenshot(event_loop);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            state.quit(event_loop);
        }
        // Shut Servo down while the display connection is still alive (windows and their
        // clipboards were destroyed in `quit`). Dropping it later, or not at all, crashes
        // in SpiderMonkey / Wayland destructors at process exit.
        drop(self.state.take());
    }
}

fn favicon_image(img: &servo::Image) -> Option<egui::ColorImage> {
    let (w, h) = (img.width as usize, img.height as usize);
    let data = img.data();
    let rgba: Vec<u8> = match img.format {
        servo::PixelFormat::RGBA8 => data.to_vec(),
        servo::PixelFormat::BGRA8 => data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect(),
        servo::PixelFormat::RGB8 => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        servo::PixelFormat::K8 => data.iter().flat_map(|k| [*k, *k, *k, 255]).collect(),
        servo::PixelFormat::KA8 => data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
    };
    if rgba.len() < w * h * 4 || w == 0 || h == 0 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w, h],
        &rgba[..w * h * 4],
    ))
}

fn resource_type(destination: &str, main_frame: bool) -> ResourceType {
    match destination {
        "Document" if main_frame => ResourceType::MainFrame,
        "Document" | "Iframe" | "Frame" | "Embed" | "Object" => ResourceType::SubFrame,
        "Script" | "Worker" | "SharedWorker" | "ServiceWorker" | "AudioWorklet"
        | "PaintWorklet" | "Json" => ResourceType::Script,
        "Style" => ResourceType::Stylesheet,
        "Image" => ResourceType::Image,
        "Font" => ResourceType::Font,
        "Audio" | "Video" | "Track" => ResourceType::Media,
        "Empty" => ResourceType::Xhr,
        "Report" => ResourceType::Ping,
        _ => ResourceType::Other,
    }
}

/// URLs whose extension strongly suggests a file download.
fn is_download_url(url: &Url) -> bool {
    const EXT: &[&str] = &[
        "zip",
        "7z",
        "rar",
        "gz",
        "tgz",
        "bz2",
        "xz",
        "zst",
        "exe",
        "msi",
        "msix",
        "dmg",
        "pkg",
        "deb",
        "rpm",
        "appimage",
        "flatpakref",
        "iso",
        "img",
        "apk",
        "jar",
        "torrent",
        "pdf",
        "docx",
        "xlsx",
        "pptx",
        "odt",
        "ods",
        "odp",
        "epub",
        "mobi",
        "bin",
        "dll",
        "so",
        "crx",
        "xpi",
        "whl",
        "nupkg",
    ];
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let path = url.path().to_ascii_lowercase();
    let file = path.rsplit('/').next().unwrap_or("");
    match file.rsplit_once('.') {
        Some((_, ext)) => EXT.contains(&ext),
        None => false,
    }
}

pub fn open_external(target: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", "", target])
        .spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(target).spawn();
    if let Err(e) = r {
        log::error!("Could not open {target}: {e}");
    }
}

fn spawn_self(args: &[&str]) {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).args(args).spawn() {
                log::error!("Could not start FerroSurf: {e}");
            }
        }
        Err(e) => log::error!("current_exe: {e}"),
    }
}

/// Open a URL in the focused window (used by the MCP host for `browser_open_tab`).
pub fn _unused(_d: Disposition) {}
