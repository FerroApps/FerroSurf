//! [`PluginHost`]: the UI-thread facade over manifests, state, the engine
//! thread and the browser extension points.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferrosurf_extensions::{RequestInfo, ResourceType};
use ferrosurf_plugin_sdk::abi::{
    self, ActiveTab, CommandInput, McpToolInput, NavigationEvent, OmniboxInput, OmniboxOutput,
    PageMessageInput, PageMessageOutput, RequestAction, RequestInput, RequestOutput, SettingValue,
    Suggestion, hook,
};
use parking_lot::{Mutex as PlMutex, RwLock};
use url::Url;

use crate::content;
use crate::hostcalls::{Ctx, HostRequest, Out, web_url};
use crate::kv::Kv;
use crate::manifest::{CommandSpec, Manifest, Permission, RequestFilter, SettingSpec, ToolSpec};
use crate::package::{self, Scan};
use crate::runtime;
use crate::state::{Record, State};
use crate::worker::{self, HookJob, Job, LoadSpec, Reply, Stats};

pub use crate::worker::Handle as EngineHandle;

/// Total time `check_request` may wait for plugins; past it, allow.
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(30);
/// Time an omnibox query may take.
pub const OMNIBOX_TIMEOUT: Duration = Duration::from_millis(400);
/// Time a page message may take.
pub const PAGE_MESSAGE_TIMEOUT: Duration = Duration::from_millis(250);
/// Time an MCP tool call may take.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_SUGGESTIONS: usize = 8;

/// What the `request` hook decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestDecision {
    Allow,
    Block,
    Redirect(Url),
}

impl From<RequestDecision> for ferrosurf_extensions::Decision {
    fn from(d: RequestDecision) -> Self {
        match d {
            RequestDecision::Allow => Self::Allow,
            RequestDecision::Block => Self::Block,
            RequestDecision::Redirect(u) => Self::Redirect(u),
        }
    }
}

/// A palette command contributed by an enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    pub plugin_id: String,
    pub plugin_name: String,
    /// The manifest command id (pass it to [`PluginHost::run_command`]).
    pub id: String,
    pub title: String,
    pub shortcut: Option<String>,
}

/// Answer to an omnibox query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OmniboxResult {
    pub plugin_id: String,
    pub suggestions: Vec<Suggestion>,
    pub navigate: Option<String>,
}

/// One setting with its current value, for the Plugins page.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingInfo {
    pub spec: SettingSpec,
    pub value: SettingValue,
}

/// Everything the Plugins page shows about one installed plugin.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub manifest: Manifest,
    pub path: PathBuf,
    /// The user turned it on.
    pub enabled: bool,
    /// Loaded in the engine right now.
    pub running: bool,
    /// Enabled, but an update asks for more than was approved.
    pub needs_reapproval: bool,
    pub granted: Vec<Permission>,
    pub settings: Vec<SettingInfo>,
    /// Why it is off or failed (faults, broken module).
    pub error: Option<String>,
}

/// A folder or package that looked like a plugin but could not be read.
#[derive(Debug, Clone)]
pub struct BrokenPlugin {
    pub path: PathBuf,
    pub error: String,
}

/// Engine and thread counters (zero-cost checks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStats {
    pub running: bool,
    pub engines_created: usize,
    pub threads_started: usize,
    pub threads_alive: usize,
}

struct ActivePlugin {
    manifest: Arc<Manifest>,
    granted: Vec<Permission>,
    content_js: Option<String>,
}

impl ActivePlugin {
    fn has(&self, p: Permission) -> bool {
        self.granted.contains(&p)
    }
}

#[derive(Default)]
struct Active {
    handle: Option<EngineHandle>,
    plugins: BTreeMap<String, ActivePlugin>,
}

#[derive(Default)]
struct Core {
    scan: Option<Scan>,
    state: State,
    running: BTreeSet<String>,
    errors: BTreeMap<String, String>,
    handle: Option<EngineHandle>,
    active: BTreeMap<String, Arc<ActivePlugin>>,
}

enum Reload<'a> {
    No,
    One(&'a str),
    All,
}

struct Inner {
    dir: PathBuf,
    data_dir: PathBuf,
    wake: Arc<dyn Fn() + Send + Sync>,
    core: PlMutex<Core>,
    /// Read-mostly view used on hot paths; rebuilt after every change.
    active: RwLock<Active>,
    request_any: AtomicBool,
    out_tx: Sender<Out>,
    out_rx: Mutex<Receiver<Out>>,
    active_tab: Arc<Mutex<Option<ActiveTab>>>,
    stats: Arc<Stats>,
}

/// The browser's plugin host. Cheap to clone (shared state); every method
/// is safe to call from any thread, and none blocks the UI thread except
/// [`check_request`](Self::check_request), which is bounded by
/// [`REQUEST_TIMEOUT`] and only waits when a plugin's filter matched.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

fn effective_settings(m: &Manifest, rec: &Record) -> BTreeMap<String, SettingValue> {
    m.settings
        .iter()
        .map(|s| {
            let v = rec
                .settings
                .get(&s.key)
                .filter(|v| s.check(v).is_ok())
                .cloned()
                .unwrap_or_else(|| s.default.clone());
            (s.key.clone(), v)
        })
        .collect()
}

/// Wire name of a resource type.
pub fn resource_type_name(t: ResourceType) -> &'static str {
    match t {
        ResourceType::MainFrame => "main_frame",
        ResourceType::SubFrame => "sub_frame",
        ResourceType::Script => "script",
        ResourceType::Stylesheet => "stylesheet",
        ResourceType::Image => "image",
        ResourceType::Font => "font",
        ResourceType::Xhr => "xhr",
        ResourceType::Media => "media",
        ResourceType::WebSocket => "websocket",
        ResourceType::Ping => "ping",
        ResourceType::Other => "other",
    }
}

fn to_reply_channel<T: Send + 'static>(
    decode: impl FnOnce(Result<Option<Vec<u8>>, String>) -> T + Send + 'static,
) -> (Reply, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel(1);
    let reply: Reply = Box::new(move |r| {
        let _ = tx.send(decode(r));
    });
    (reply, rx)
}

impl PluginHost {
    /// Opens the host over the plugins folder `dir` (`state.json` and the
    /// plugins live there) and `data_dir` (`plugins.sqlite3`). `wake` is
    /// called from plugin threads whenever [`drain_host_requests`]
    /// (Self::drain_host_requests) has something to return (call
    /// `EventLoopProxy::send_event` or similar in it; it must not block).
    ///
    /// Costs one read of `state.json`; the folder is only scanned and the
    /// engine thread only started when a plugin is enabled.
    pub fn open(dir: PathBuf, data_dir: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (out_tx, out_rx) = mpsc::channel();
        let state = State::load(&dir);
        let host = Self {
            inner: Arc::new(Inner {
                dir,
                data_dir,
                wake,
                core: PlMutex::new(Core {
                    state,
                    ..Core::default()
                }),
                active: RwLock::default(),
                request_any: AtomicBool::new(false),
                out_tx,
                out_rx: Mutex::new(out_rx),
                active_tab: Arc::default(),
                stats: Arc::default(),
            }),
        };
        {
            let mut core = host.inner.core.lock();
            host.reconcile(&mut core, Reload::No);
        }
        host
    }

    /// The plugins folder.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Engine and thread counters; with nothing enabled `running` is false
    /// and `threads_started` stays 0.
    pub fn engine_stats(&self) -> EngineStats {
        let s = &self.inner.stats;
        EngineStats {
            running: self.inner.active.read().handle.is_some(),
            engines_created: s.engines_created.load(Ordering::SeqCst),
            threads_started: s.threads_started.load(Ordering::SeqCst),
            threads_alive: s.threads_alive.load(Ordering::SeqCst),
        }
    }

    fn scan_dir(&self, core: &mut Core) -> Scan {
        core.scan
            .get_or_insert_with(|| package::scan(&self.inner.dir))
            .clone()
    }

    fn save(&self, core: &Core) {
        if let Err(e) = core.state.save(&self.inner.dir) {
            log::warn!("cannot save plugins state: {e}");
        }
    }

    /// Brings the engine in line with the state: loads what should run and
    /// is not running (or everything with `reload`), unloads the rest, and
    /// starts / stops the engine thread.
    fn reconcile(&self, core: &mut Core, reload: Reload<'_>) {
        if core.scan.is_none() && core.state.any_enabled() {
            core.scan = Some(package::scan(&self.inner.dir));
        }
        let scan = core.scan.clone().unwrap_or_default();
        let mut desired = Vec::new();
        for (id, rec) in &core.state.plugins {
            if !rec.enabled {
                continue;
            }
            if let Some(found) = scan.get(id)
                && rec.covers(&found.manifest)
            {
                desired.push((found.clone(), rec.clone()));
            }
        }
        let desired_ids: BTreeSet<String> =
            desired.iter().map(|(f, _)| f.manifest.id.clone()).collect();

        if desired.is_empty() {
            // Dropping the last handle stops the thread, which unloads.
            core.handle = None;
            core.running.clear();
            core.active.clear();
            self.publish(core);
            return;
        }
        if core.handle.is_none() {
            let cfg = worker::Config {
                kv_path: Some(self.inner.data_dir.join("plugins.sqlite3")),
                out: self.inner.out_tx.clone(),
                wake: self.inner.wake.clone(),
                active_tab: self.inner.active_tab.clone(),
                stats: self.inner.stats.clone(),
            };
            match worker::spawn(cfg) {
                Ok(h) => {
                    core.handle = Some(h);
                    core.running.clear();
                }
                Err(e) => {
                    log::error!("cannot start the plugin thread: {e}");
                    return;
                }
            }
        }
        let Some(handle) = core.handle.clone() else {
            return;
        };
        let stale: Vec<String> = core.running.difference(&desired_ids).cloned().collect();
        for id in stale {
            handle.send(Job::Unload(id.clone()));
            core.running.remove(&id);
            core.active.remove(&id);
        }
        for (found, rec) in desired {
            let id = found.manifest.id.clone();
            let again = match reload {
                Reload::All => true,
                Reload::One(r) => r == id,
                Reload::No => false,
            };
            if core.running.contains(&id) && !again {
                continue;
            }
            let loaded = package::read_wasm(&found.path).and_then(|wasm| {
                let assets = package::read_content(&found.path, &found.manifest)?;
                Ok((wasm, assets))
            });
            match loaded {
                Ok((wasm, assets)) => {
                    let m = found.manifest;
                    let content_js = if rec.granted.contains(&Permission::PageScripting) {
                        content::wrapper(&m, &assets)
                    } else {
                        None
                    };
                    handle.send(Job::Load(Box::new(LoadSpec {
                        settings: effective_settings(&m, &rec),
                        granted: rec.granted.clone(),
                        manifest: m.clone(),
                        wasm,
                    })));
                    core.errors.remove(&id);
                    core.running.insert(id.clone());
                    core.active.insert(
                        id,
                        Arc::new(ActivePlugin {
                            granted: rec.granted,
                            manifest: Arc::new(m),
                            content_js,
                        }),
                    );
                }
                Err(e) => {
                    core.errors.insert(id.clone(), e.clone());
                    core.state.disable(&id, Some(e.clone()));
                    core.running.remove(&id);
                    core.active.remove(&id);
                    self.save(core);
                    let _ = self
                        .inner
                        .out_tx
                        .send(Out::Host(HostRequest::PluginDisabled {
                            plugin: id,
                            reason: e,
                        }));
                    (self.inner.wake)();
                }
            }
        }
        if core.running.is_empty() {
            core.handle = None;
        }
        self.publish(core);
    }

    fn publish(&self, core: &Core) {
        let plugins: BTreeMap<String, ActivePlugin> = core
            .active
            .iter()
            .map(|(k, a)| {
                (
                    k.clone(),
                    ActivePlugin {
                        manifest: a.manifest.clone(),
                        granted: a.granted.clone(),
                        content_js: a.content_js.clone(),
                    },
                )
            })
            .collect();
        let any_request = plugins
            .values()
            .any(|p| p.has(Permission::Requests) && !p.manifest.request_filters.is_empty());
        let mut active = self.inner.active.write();
        active.handle = core.handle.clone();
        active.plugins = plugins;
        self.inner.request_any.store(any_request, Ordering::Release);
    }

    // ---- Plugins page -------------------------------------------------

    /// Every plugin found in the folder (sorted by name).
    pub fn list(&self) -> Vec<PluginInfo> {
        let mut core = self.inner.core.lock();
        let scan = self.scan_dir(&mut core);
        scan.found
            .iter()
            .map(|f| {
                let id = &f.manifest.id;
                let rec = core.state.plugins.get(id).cloned().unwrap_or_default();
                let eff = effective_settings(&f.manifest, &rec);
                PluginInfo {
                    path: f.path.clone(),
                    enabled: rec.enabled,
                    running: core.running.contains(id),
                    needs_reapproval: rec.enabled && !rec.covers(&f.manifest),
                    granted: rec.granted.clone(),
                    settings: f
                        .manifest
                        .settings
                        .iter()
                        .map(|s| SettingInfo {
                            spec: s.clone(),
                            value: eff[&s.key].clone(),
                        })
                        .collect(),
                    error: core
                        .errors
                        .get(id)
                        .cloned()
                        .or_else(|| rec.disabled_reason.clone()),
                    manifest: f.manifest.clone(),
                }
            })
            .collect()
    }

    /// Entries of the folder that are not valid plugins.
    pub fn broken(&self) -> Vec<BrokenPlugin> {
        let mut core = self.inner.core.lock();
        self.scan_dir(&mut core);
        core.scan
            .as_ref()
            .map(|s| {
                s.broken
                    .iter()
                    .map(|b| BrokenPlugin {
                        path: b.path.clone(),
                        error: b.error.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Turns a plugin on, granting `grant_permissions` (the subset of its
    /// manifest permissions the user accepted; the rest are denied at run
    /// time). Network hosts are granted with `Permission::Network`.
    pub fn enable(&self, id: &str, grant_permissions: &[Permission]) -> Result<(), String> {
        let mut core = self.inner.core.lock();
        let scan = self.scan_dir(&mut core);
        let found = scan
            .get(id)
            .ok_or_else(|| format!("plugin {id:?} no encontrado"))?;
        core.state.enable(&found.manifest, grant_permissions);
        core.errors.remove(id);
        self.save(&core);
        self.reconcile(&mut core, Reload::One(id));
        core.errors.get(id).map_or(Ok(()), |e| Err(e.clone()))
    }

    /// Turns a plugin off (its grant is dropped; its settings and store stay).
    pub fn disable(&self, id: &str) {
        let mut core = self.inner.core.lock();
        core.state.disable(id, None);
        core.errors.remove(id);
        self.save(&core);
        self.reconcile(&mut core, Reload::No);
    }

    /// Removes a plugin: files, state, settings and private store.
    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        let mut core = self.inner.core.lock();
        let scan = self.scan_dir(&mut core);
        if let Some(f) = scan.get(id) {
            let r = if f.path.is_dir() {
                std::fs::remove_dir_all(&f.path)
            } else {
                std::fs::remove_file(&f.path)
            };
            r.map_err(|e| e.to_string())?;
        }
        core.state.plugins.remove(id);
        core.errors.remove(id);
        self.save(&core);
        let _ = Kv::new(Some(self.inner.data_dir.join("plugins.sqlite3"))).delete_all(id);
        core.scan = Some(package::scan(&self.inner.dir));
        self.reconcile(&mut core, Reload::No);
        Ok(())
    }

    /// Installs a plugin folder or `.ferroplug` file into the plugins
    /// folder (replacing an older version) and returns its id. A new
    /// version that asks for more permissions is turned off until the
    /// user enables it again.
    pub fn install_from(&self, path: &Path) -> Result<String, String> {
        let m = package::install(path, &self.inner.dir)?;
        let mut core = self.inner.core.lock();
        core.scan = Some(package::scan(&self.inner.dir));
        core.errors.remove(&m.id);
        self.reconcile(&mut core, Reload::One(&m.id));
        Ok(m.id)
    }

    /// Rescans the folder and restarts every enabled plugin from disk
    /// (hot reload while developing one).
    pub fn reload_all(&self) {
        let mut core = self.inner.core.lock();
        core.scan = Some(package::scan(&self.inner.dir));
        core.errors.clear();
        self.reconcile(&mut core, Reload::All);
    }

    /// Changes a setting (validated against the manifest schema), saves it
    /// and tells the plugin through `setting_changed`.
    pub fn set_setting(&self, id: &str, key: &str, value: SettingValue) -> Result<(), String> {
        let mut core = self.inner.core.lock();
        let scan = self.scan_dir(&mut core);
        let found = scan
            .get(id)
            .ok_or_else(|| format!("plugin {id:?} no encontrado"))?;
        let spec = found
            .manifest
            .setting(key)
            .ok_or_else(|| format!("ajuste {key:?} desconocido"))?;
        spec.check(&value)?;
        core.state
            .plugins
            .entry(id.to_owned())
            .or_default()
            .settings
            .insert(key.to_owned(), value.clone());
        self.save(&core);
        if core.running.contains(id)
            && let Some(h) = &core.handle
        {
            h.send(Job::Setting {
                plugin: id.to_owned(),
                key: key.to_owned(),
                value,
            });
        }
        Ok(())
    }

    // ---- Browser integration ------------------------------------------

    /// Tells plugins which tab is active (answers `get_active_tab`). Call
    /// when the active tab, its URL or its title changes.
    pub fn set_active_tab(&self, tab: Option<ActiveTab>) {
        if let Ok(mut t) = self.inner.active_tab.lock() {
            *t = tab;
        }
    }

    /// Decides one network request. Synchronous and fast: with no plugin
    /// listening this is one atomic load; otherwise only plugins whose
    /// manifest `request_filters` match the URL are called (never the
    /// engine for the rest), under a tight fuel budget and an overall
    /// [`REQUEST_TIMEOUT`] after which the request is allowed.
    pub fn check_request(&self, req: &RequestInfo<'_>) -> RequestDecision {
        if !self.inner.request_any.load(Ordering::Acquire) {
            return RequestDecision::Allow;
        }
        let url_lower = req.url.as_str().to_ascii_lowercase();
        let host = req.url.host_str().unwrap_or("").to_ascii_lowercase();
        let (handle, matching): (EngineHandle, Vec<String>) = {
            let active = self.inner.active.read();
            let Some(h) = active.handle.clone() else {
                return RequestDecision::Allow;
            };
            let m = active
                .plugins
                .iter()
                .filter(|(_, p)| {
                    p.has(Permission::Requests)
                        && p.manifest
                            .request_filters
                            .iter()
                            .any(|f: &RequestFilter| f.matches(&host, &url_lower))
                })
                .map(|(id, _)| id.clone())
                .collect();
            (h, m)
        };
        if matching.is_empty() {
            return RequestDecision::Allow;
        }
        let input = RequestInput {
            url: req.url.as_str().to_owned(),
            resource_type: resource_type_name(req.resource_type).to_owned(),
            initiator: req.initiator.map(|u| u.as_str().to_owned()),
            main_frame: req.resource_type == ResourceType::MainFrame,
        };
        let Ok(payload) = abi::encode(&input) else {
            return RequestDecision::Allow;
        };
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        for id in matching {
            let (reply, rx) = to_reply_channel(|r| r);
            handle.send(Job::Hook(HookJob {
                plugin: id,
                hook: hook::REQUEST,
                ctx: Ctx::Request,
                payload: payload.clone(),
                fuel: runtime::REQUEST_FUEL,
                deadline: Some(deadline),
                reply: Some(reply),
            }));
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(Ok(Some(bytes))) = rx.recv_timeout(left) else {
                continue;
            };
            let Ok(out) = abi::decode::<RequestOutput>(&bytes) else {
                continue;
            };
            match out.action {
                RequestAction::Allow => {}
                RequestAction::Block => return RequestDecision::Block,
                RequestAction::Redirect => {
                    let target = out.redirect_url.as_deref().and_then(web_url);
                    if let Some(t) = target
                        && &t != req.url
                    {
                        return RequestDecision::Redirect(t);
                    }
                }
            }
        }
        RequestDecision::Allow
    }

    /// `(plugin_id, js_source)` of every enabled plugin with static content
    /// scripts (`page_scripting` granted), ready to inject as user scripts.
    /// The programs carry their own match-pattern guards. Re-read after
    /// every enable / disable / reload.
    pub fn content_scripts(&self) -> Vec<(String, String)> {
        self.inner
            .active
            .read()
            .plugins
            .iter()
            .filter_map(|(id, p)| p.content_js.clone().map(|js| (id.clone(), js)))
            .collect()
    }

    /// Palette commands of the enabled plugins.
    pub fn commands(&self) -> Vec<PluginCommand> {
        self.inner
            .active
            .read()
            .plugins
            .values()
            .flat_map(|p| {
                p.manifest
                    .commands
                    .iter()
                    .map(|c: &CommandSpec| PluginCommand {
                        plugin_id: p.manifest.id.clone(),
                        plugin_name: p.manifest.name.clone(),
                        id: c.id.clone(),
                        title: c.title.clone(),
                        shortcut: c.shortcut.clone(),
                    })
            })
            .collect()
    }

    /// Runs a palette command (a user action) in the background; its
    /// effects arrive through [`drain_host_requests`](Self::drain_host_requests).
    pub fn run_command(
        &self,
        plugin_id: &str,
        command_id: &str,
        tab: Option<&ActiveTab>,
    ) -> Result<(), String> {
        let handle = {
            let active = self.inner.active.read();
            let p = active
                .plugins
                .get(plugin_id)
                .ok_or("el plugin no está activo")?;
            if !p.manifest.commands.iter().any(|c| c.id == command_id) {
                return Err(format!("comando {command_id:?} desconocido"));
            }
            active
                .handle
                .clone()
                .ok_or("el motor de plugins no está activo")?
        };
        let input = CommandInput {
            command: command_id.to_owned(),
            tab_id: tab.map_or(0, |t| t.tab_id),
            url: tab.map(|t| t.url.clone()).unwrap_or_default(),
        };
        let payload = abi::encode(&input).map_err(|e| e.to_string())?;
        handle.send(Job::Hook(HookJob {
            plugin: plugin_id.to_owned(),
            hook: hook::COMMAND,
            ctx: Ctx::Command {
                tab_id: input.tab_id,
            },
            payload,
            fuel: runtime::COMMAND_FUEL,
            deadline: None,
            reply: None,
        }));
        Ok(())
    }

    /// Tells plugins that subscribe to `navigation` (and hold `tabs`) that
    /// a tab committed (`phase = "commit"`) or finished loading
    /// (`"complete"`). Asynchronous, bounded queue.
    pub fn notify_navigation(&self, tab_id: u64, url: &str, title: &str, phase: &str) {
        let (handle, ids) = {
            let active = self.inner.active.read();
            let Some(h) = active.handle.clone() else {
                return;
            };
            let ids: Vec<String> = active
                .plugins
                .iter()
                .filter(|(_, p)| p.has(Permission::Tabs) && p.manifest.subscribes_navigation())
                .map(|(id, _)| id.clone())
                .collect();
            (h, ids)
        };
        if ids.is_empty() {
            return;
        }
        let ev = NavigationEvent {
            tab_id,
            url: url.chars().take(2048).collect(),
            title: title.chars().take(200).collect(),
            phase: phase.to_owned(),
        };
        let Ok(payload) = abi::encode(&ev) else {
            return;
        };
        for id in ids {
            handle.event(HookJob {
                plugin: id,
                hook: hook::NAVIGATION,
                ctx: Ctx::Navigation { tab_id },
                payload: payload.clone(),
                fuel: runtime::NAVIGATION_FUEL,
                deadline: None,
                reply: None,
            });
        }
    }

    /// Sends `text` to the plugin that owns omnibox `keyword`. `None` when
    /// no enabled plugin has that keyword. The receiver gets exactly one
    /// result (empty on timeout or failure).
    pub fn omnibox(&self, keyword: &str, text: &str) -> Option<Receiver<OmniboxResult>> {
        let (handle, id) = {
            let active = self.inner.active.read();
            let p = active
                .plugins
                .values()
                .find(|p| p.manifest.omnibox_keyword.as_deref() == Some(keyword))?;
            (active.handle.clone()?, p.manifest.id.clone())
        };
        let payload = abi::encode(&OmniboxInput {
            text: text.chars().take(512).collect(),
        })
        .ok()?;
        let pid = id.clone();
        let (reply, rx) = to_reply_channel(move |r| {
            let out = match r {
                Ok(Some(b)) => abi::decode::<OmniboxOutput>(&b).unwrap_or_default(),
                _ => OmniboxOutput::default(),
            };
            sanitize_omnibox(pid, out)
        });
        handle.send(Job::Hook(HookJob {
            plugin: id,
            hook: hook::OMNIBOX,
            ctx: Ctx::Omnibox,
            payload,
            fuel: runtime::OMNIBOX_FUEL,
            deadline: Some(Instant::now() + OMNIBOX_TIMEOUT),
            reply: Some(reply),
        }));
        Some(rx)
    }

    /// Tools of the enabled plugins (`mcp_tools` granted), named
    /// `plugin_<id>__<tool>`. Also served by [`tool_handler`](Self::tool_handler).
    pub fn mcp_tools(&self) -> Vec<ferrosurf_mcp::server::ToolDef> {
        self.inner
            .active
            .read()
            .plugins
            .values()
            .filter(|p| p.has(Permission::McpTools))
            .flat_map(|p| {
                p.manifest
                    .tools
                    .iter()
                    .map(|t: &ToolSpec| ferrosurf_mcp::server::ToolDef {
                        name: p.manifest.mcp_tool_name(&t.name),
                        title: Some(format!("{}: {}", p.manifest.name, t.name)),
                        description: t.description.clone(),
                        input_schema: t.input_schema.clone(),
                        read_only: t.read_only,
                        destructive: false,
                    })
            })
            .collect()
    }

    /// Queues a call of the exposed tool `name`; `reply` gets the raw hook
    /// output. Used by [`crate::PluginToolHandler`].
    pub(crate) fn submit_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        reply: Reply,
    ) -> Result<(), String> {
        let (handle, plugin, tool) = {
            let active = self.inner.active.read();
            let (p, t) = active
                .plugins
                .values()
                .filter(|p| p.has(Permission::McpTools))
                .find_map(|p| {
                    p.manifest
                        .tools
                        .iter()
                        .find(|t| p.manifest.mcp_tool_name(&t.name) == name)
                        .map(|t| (p, t))
                })
                .ok_or_else(|| format!("herramienta {name:?} desconocida"))?;
            (
                active
                    .handle
                    .clone()
                    .ok_or("el motor de plugins no está activo")?,
                p.manifest.id.clone(),
                t.name.clone(),
            )
        };
        let args_json = if args.is_null() {
            "{}".to_owned()
        } else {
            args.to_string()
        };
        if args_json.len() > MAX_PAYLOAD {
            return Err("argumentos demasiado grandes".into());
        }
        let payload = abi::encode(&McpToolInput {
            name: tool,
            args_json,
        })
        .map_err(|e| e.to_string())?;
        handle.send(Job::Hook(HookJob {
            plugin,
            hook: hook::MCP_TOOL,
            ctx: Ctx::McpTool,
            payload,
            fuel: runtime::MCP_FUEL,
            deadline: Some(Instant::now() + TOOL_TIMEOUT),
            reply: Some(reply),
        }));
        Ok(())
    }

    /// An adapter that serves [`mcp_tools`](Self::mcp_tools) as an
    /// `ferrosurf_mcp::ToolHandler` (merge it with the browser tools).
    pub fn tool_handler(&self) -> crate::PluginToolHandler {
        crate::PluginToolHandler::new(self.clone())
    }

    /// A page called `window.__ferrosurf.call("plugin:<plugin_id>", payload)`.
    /// The receiver gets the plugin's reply, or why there is none (no such
    /// plugin, permission or origin not allowed, timeout, fault). The page
    /// should be answered when the receiver does; never block the UI thread
    /// on it.
    pub fn page_message(
        &self,
        plugin_id: &str,
        origin: &str,
        tab_id: u64,
        payload: serde_json::Value,
    ) -> Receiver<Result<serde_json::Value, String>> {
        let (tx, rx) = mpsc::sync_channel(1);
        let fail = |tx: &mpsc::SyncSender<_>, e: &str| {
            let _ = tx.send(Err(e.to_owned()));
        };
        let (handle, manifest) = {
            let active = self.inner.active.read();
            match active.plugins.get(plugin_id) {
                Some(p) if p.has(Permission::PageMessages) => {
                    let Some(h) = active.handle.clone() else {
                        fail(&tx, "el motor de plugins no está activo");
                        return rx;
                    };
                    (h, p.manifest.clone())
                }
                _ => {
                    fail(&tx, "plugin no disponible");
                    return rx;
                }
            }
        };
        let origin_url = Url::parse(origin).ok().and_then(|u| {
            // An origin is scheme://host[:port]; match it as a URL with path "/".
            Url::parse(&format!("{}/", u.origin().ascii_serialization())).ok()
        });
        let allowed = origin_url.as_ref().is_some_and(|u| {
            manifest
                .page_message_origins
                .iter()
                .any(|p| ferrosurf_extensions::MatchPattern::parse(p).is_ok_and(|mp| mp.matches(u)))
        });
        if !allowed {
            fail(&tx, "origen no permitido");
            return rx;
        }
        let payload_json = payload.to_string();
        if payload_json.len() > MAX_PAYLOAD {
            fail(&tx, "mensaje demasiado grande");
            return rx;
        }
        let Ok(bytes) = abi::encode(&PageMessageInput {
            origin: origin.to_owned(),
            tab_id,
            payload_json,
        }) else {
            fail(&tx, "mensaje inválido");
            return rx;
        };
        let reply: Reply = Box::new(move |r| {
            let res = match r {
                Ok(Some(b)) => match abi::decode::<PageMessageOutput>(&b) {
                    Ok(o) if o.reply_json.len() > MAX_PAYLOAD => {
                        Err("respuesta demasiado grande".to_owned())
                    }
                    Ok(o) if o.reply_json.is_empty() => Ok(serde_json::Value::Null),
                    Ok(o) => serde_json::from_str(&o.reply_json)
                        .map_err(|_| "respuesta no es JSON".to_owned()),
                    Err(_) => Err("respuesta inválida".to_owned()),
                },
                Ok(None) => Ok(serde_json::Value::Null),
                Err(e) => Err(e),
            };
            let _ = tx.send(res);
        });
        handle.send(Job::Hook(HookJob {
            plugin: plugin_id.to_owned(),
            hook: hook::PAGE_MESSAGE,
            ctx: Ctx::PageMessage,
            payload: bytes,
            fuel: runtime::PAGE_MESSAGE_FUEL,
            deadline: Some(Instant::now() + PAGE_MESSAGE_TIMEOUT),
            reply: Some(reply),
        }));
        rx
    }

    /// Things plugins asked the UI thread to do since the last call (call
    /// it when `wake` fired, and once per frame is fine too). Also applies
    /// host-side bookkeeping: plugin-set settings are saved, and a plugin
    /// the engine gave up on (repeated faults) is turned off in the state
    /// file (reported as [`HostRequest::PluginDisabled`]).
    pub fn drain_host_requests(&self) -> Vec<HostRequest> {
        let outs: Vec<Out> = {
            let Ok(rx) = self.inner.out_rx.lock() else {
                return Vec::new();
            };
            rx.try_iter().collect()
        };
        let mut reqs = Vec::new();
        for out in outs {
            match out {
                Out::Host(r) => reqs.push(r),
                Out::SettingSaved { plugin, key, value } => {
                    let mut core = self.inner.core.lock();
                    core.state
                        .plugins
                        .entry(plugin.clone())
                        .or_default()
                        .settings
                        .insert(key.clone(), value.clone());
                    self.save(&core);
                    reqs.push(HostRequest::SettingChanged { plugin, key, value });
                }
                Out::Faulted {
                    plugin,
                    reason,
                    disabled,
                } => {
                    if !disabled {
                        continue;
                    }
                    let mut core = self.inner.core.lock();
                    core.state.disable(&plugin, Some(reason.clone()));
                    core.errors.insert(plugin.clone(), reason.clone());
                    core.running.remove(&plugin);
                    core.active.remove(&plugin);
                    self.save(&core);
                    self.reconcile(&mut core, Reload::No);
                    reqs.push(HostRequest::PluginDisabled { plugin, reason });
                }
                Out::Http { .. } => {}
            }
        }
        reqs
    }
}

fn sanitize_omnibox(plugin_id: String, out: OmniboxOutput) -> OmniboxResult {
    let suggestions = out
        .suggestions
        .into_iter()
        .filter_map(|s| {
            let url = web_url(&s.url)?;
            let title: String = s.title.chars().take(120).collect();
            Some(Suggestion {
                title: if title.trim().is_empty() {
                    url.as_str().to_owned()
                } else {
                    title
                },
                url: url.into(),
            })
        })
        .take(MAX_SUGGESTIONS)
        .collect();
    OmniboxResult {
        plugin_id,
        suggestions,
        navigate: out.navigate.as_deref().and_then(web_url).map(Into::into),
    }
}
