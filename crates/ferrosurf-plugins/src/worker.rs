//! The plugin thread. All plugin code runs here, never on the UI thread:
//! one thread for all plugins, a priority queue for hooks somebody waits
//! on, and a bounded queue for observer events (the oldest are dropped
//! when plugins are slow, so the app never waits and memory stays bounded).
//!
//! The thread (and the wasm engine) only exist while at least one plugin
//! is enabled.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use ferrosurf_plugin_sdk::abi::{
    self, ABI_VERSION, ActiveTab, HttpResponse, LoadInfo, SettingChanged, SettingValue, hook,
};
use wasmi::{Engine, Module};

use crate::hostcalls::{Ctx, Env, Out};
use crate::kv::Kv;
use crate::manifest::{Manifest, Permission};
use crate::runtime::{self, Fault, Instance};

/// Observer events waiting at most; older ones are dropped beyond this.
pub const EVENT_QUEUE: usize = 256;
/// Faults (traps, fuel exhaustion, panics) before a plugin is disabled.
pub const MAX_FAULTS: u32 = 3;

/// Counters of one host's engine and thread (zero-cost assertions).
#[derive(Default)]
pub struct Stats {
    /// Engines created since the host opened.
    pub engines_created: AtomicUsize,
    /// Plugin threads started since the host opened.
    pub threads_started: AtomicUsize,
    /// Plugin threads currently alive.
    pub threads_alive: AtomicUsize,
}

/// Completion callback of a hook: `Ok(None)` = the hook returned no output.
pub type Reply = Box<dyn FnOnce(Result<Option<Vec<u8>>, String>) + Send>;

pub struct HookJob {
    pub plugin: String,
    pub hook: i32,
    pub ctx: Ctx,
    pub payload: Vec<u8>,
    pub fuel: u64,
    /// Jobs older than this are answered with a timeout error unrun.
    pub deadline: Option<Instant>,
    pub reply: Option<Reply>,
}

pub struct LoadSpec {
    pub manifest: Manifest,
    pub wasm: Vec<u8>,
    pub settings: BTreeMap<String, SettingValue>,
    pub granted: Vec<Permission>,
}

pub enum Job {
    Load(Box<LoadSpec>),
    Unload(String),
    Hook(HookJob),
    Setting {
        plugin: String,
        key: String,
        value: SettingValue,
    },
    HttpDone {
        plugin: String,
        response: HttpResponse,
    },
}

#[derive(Default)]
struct Queues {
    /// Load / unload / setting jobs: they must run before any hook that
    /// was queued after them.
    ctl: VecDeque<Job>,
    /// Hooks someone waits on (requests, tools, omnibox, page messages).
    sync: VecDeque<Job>,
    /// Control jobs and user actions; never dropped.
    prio: VecDeque<Job>,
    /// Observer events; bounded.
    events: VecDeque<HookJob>,
    closed: bool,
}

type Shared = Arc<(Mutex<Queues>, Condvar)>;

fn lock(s: &Shared) -> MutexGuard<'_, Queues> {
    s.0.lock().unwrap_or_else(|e| e.into_inner())
}

fn push(shared: &Shared, job: Job) {
    {
        let mut q = lock(shared);
        match &job {
            Job::Load(_) | Job::Unload(_) | Job::Setting { .. } => q.ctl.push_back(job),
            Job::Hook(h) if h.deadline.is_some() => q.sync.push_back(job),
            _ => q.prio.push_back(job),
        }
    }
    shared.1.notify_one();
}

/// The host's handle to the plugin thread. Dropping the last clone stops
/// the thread.
#[derive(Clone)]
pub struct Handle {
    shared: Shared,
    _stop: Arc<StopOnDrop>,
}

struct StopOnDrop(Shared);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        lock(&self.0).closed = true;
        self.0.1.notify_all();
    }
}

struct AliveGuard(Arc<Stats>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.threads_alive.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Handle {
    /// Queues a job (control jobs and user actions are never dropped).
    pub fn send(&self, job: Job) {
        push(&self.shared, job);
    }

    /// Queues an observer event, dropping the oldest when full.
    pub fn event(&self, job: HookJob) {
        {
            let mut q = lock(&self.shared);
            if q.events.len() >= EVENT_QUEUE {
                q.events.pop_front();
            }
            q.events.push_back(job);
        }
        self.shared.1.notify_one();
    }
}

/// Everything the worker needs from the host.
pub struct Config {
    pub kv_path: Option<PathBuf>,
    pub out: Sender<Out>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub active_tab: Arc<Mutex<Option<ActiveTab>>>,
    pub stats: Arc<Stats>,
}

/// Starts the plugin thread.
pub fn spawn(cfg: Config) -> std::io::Result<Handle> {
    let shared: Shared = Arc::default();
    cfg.stats.threads_started.fetch_add(1, Ordering::SeqCst);
    cfg.stats.threads_alive.fetch_add(1, Ordering::SeqCst);
    let thread_shared = shared.clone();
    let stats = cfg.stats.clone();
    let spawned = std::thread::Builder::new()
        .name("ferrosurf-plugins".into())
        .spawn(move || {
            let _alive = AliveGuard(cfg.stats.clone());
            run(thread_shared, cfg);
        });
    if let Err(e) = spawned {
        stats.threads_alive.fetch_sub(1, Ordering::SeqCst);
        return Err(e);
    }
    Ok(Handle {
        _stop: Arc::new(StopOnDrop(shared.clone())),
        shared,
    })
}

struct Slot {
    module: Module,
    manifest: Arc<Manifest>,
    granted: Arc<Vec<Permission>>,
    hosts: Arc<Vec<String>>,
    settings: Rc<RefCell<BTreeMap<String, SettingValue>>>,
    inflight: Arc<AtomicUsize>,
    instance: Option<Instance>,
    faults: u32,
}

struct Worker {
    engine: Engine,
    kv: Rc<RefCell<Kv>>,
    plugins: BTreeMap<String, Slot>,
    cfg: Config,
    shared: Shared,
}

fn run(shared: Shared, cfg: Config) {
    let engine = runtime::engine(&cfg.stats.engines_created);
    let kv = Rc::new(RefCell::new(Kv::new(cfg.kv_path.clone())));
    let mut w = Worker {
        engine,
        kv,
        plugins: BTreeMap::new(),
        cfg,
        shared: shared.clone(),
    };
    loop {
        let job = {
            let mut q = lock(&shared);
            loop {
                if q.closed {
                    break None;
                }
                if let Some(j) = q
                    .ctl
                    .pop_front()
                    .or_else(|| q.sync.pop_front())
                    .or_else(|| q.prio.pop_front())
                {
                    break Some(j);
                }
                if let Some(e) = q.events.pop_front() {
                    break Some(Job::Hook(e));
                }
                q = shared.1.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(job) = job else { break };
        w.process(job);
    }
    // Stopping: unload what is loaded and answer whoever still waits.
    let ids: Vec<String> = w.plugins.keys().cloned().collect();
    for id in ids {
        w.unload(&id);
    }
    let rest: Vec<Job> = {
        let mut q = lock(&shared);
        let ctl = std::mem::take(&mut q.ctl);
        let sync = std::mem::take(&mut q.sync);
        let prio = std::mem::take(&mut q.prio);
        let events = std::mem::take(&mut q.events);
        ctl.into_iter()
            .chain(sync)
            .chain(prio)
            .chain(events.into_iter().map(Job::Hook))
            .collect()
    };
    for j in rest {
        if let Job::Hook(HookJob { reply: Some(r), .. }) = j {
            r(Err("el motor de plugins se detuvo".into()));
        }
    }
}

impl Worker {
    fn process(&mut self, job: Job) {
        match job {
            Job::Load(spec) => self.load(*spec),
            Job::Unload(id) => self.unload(&id),
            Job::Hook(h) => {
                let HookJob {
                    plugin,
                    hook,
                    ctx,
                    payload,
                    fuel,
                    deadline,
                    reply,
                } = h;
                let r = if deadline.is_some_and(|d| Instant::now() > d) {
                    Err("tiempo agotado".to_owned())
                } else {
                    self.run_hook(&plugin, hook, ctx, &payload, fuel)
                };
                if let Some(reply) = reply {
                    reply(r);
                }
            }
            Job::Setting { plugin, key, value } => {
                if let Some(s) = self.plugins.get(&plugin) {
                    s.settings.borrow_mut().insert(key.clone(), value.clone());
                }
                if let Ok(p) = abi::encode(&SettingChanged { key, value }) {
                    let _ = self.run_hook(
                        &plugin,
                        hook::SETTING_CHANGED,
                        Ctx::Lifecycle,
                        &p,
                        runtime::SETTING_FUEL,
                    );
                }
            }
            Job::HttpDone { plugin, response } => {
                if let Some(s) = self.plugins.get(&plugin)
                    && s.inflight.load(Ordering::Relaxed) > 0
                {
                    s.inflight.fetch_sub(1, Ordering::Relaxed);
                }
                if let Ok(p) = abi::encode(&response) {
                    let _ = self.run_hook(
                        &plugin,
                        hook::HTTP_RESPONSE,
                        Ctx::Lifecycle,
                        &p,
                        runtime::HTTP_FUEL,
                    );
                }
            }
        }
    }

    fn send(&self, out: Out) {
        let _ = self.cfg.out.send(out);
        (self.cfg.wake)();
    }

    fn load(&mut self, spec: LoadSpec) {
        let id = spec.manifest.id.clone();
        if self.plugins.contains_key(&id) {
            self.unload(&id);
        }
        let module = match runtime::compile(&self.engine, &spec.wasm) {
            Ok(m) => m,
            Err(reason) => {
                self.send(Out::Faulted {
                    plugin: id,
                    reason,
                    disabled: true,
                });
                return;
            }
        };
        let hosts = if spec.granted.contains(&Permission::Network) {
            spec.manifest.network_hosts.clone()
        } else {
            Vec::new()
        };
        self.plugins.insert(
            id.clone(),
            Slot {
                module,
                manifest: Arc::new(spec.manifest),
                granted: Arc::new(spec.granted),
                hosts: Arc::new(hosts),
                settings: Rc::new(RefCell::new(spec.settings)),
                inflight: Arc::default(),
                instance: None,
                faults: 0,
            },
        );
        // Instantiate now so load errors surface immediately.
        let _ = self.ensure_instance(&id);
    }

    fn unload(&mut self, id: &str) {
        if let Some(mut slot) = self.plugins.remove(id)
            && let Some(inst) = slot.instance.as_mut()
        {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                inst.call(hook::UNLOAD, &[], Ctx::Lifecycle, runtime::UNLOAD_FUEL)
            }));
        }
    }

    /// Instantiates a slot that has no live instance (first load, or after
    /// a fault) and runs its `load` hook.
    fn ensure_instance(&mut self, id: &str) -> Result<(), String> {
        let Some(slot) = self.plugins.get_mut(id) else {
            return Err("el plugin no está activo".into());
        };
        if slot.instance.is_some() {
            return Ok(());
        }
        let env = Env::new(
            slot.manifest.clone(),
            slot.granted.clone(),
            slot.hosts.clone(),
            slot.settings.clone(),
            self.kv.clone(),
            runtime::limits(),
            self.cfg.active_tab.clone(),
            slot.inflight.clone(),
        );
        let info = LoadInfo {
            api_version: ABI_VERSION,
            plugin_id: id.to_owned(),
            settings: slot.settings.borrow().clone(),
        };
        let engine = self.engine.clone();
        let module = slot.module.clone();
        let started = catch_unwind(AssertUnwindSafe(|| -> Result<Instance, Fault> {
            let mut inst = Instance::new(&engine, &module, env)?;
            let payload = abi::encode(&info).map_err(|e| Fault::Abi(e.to_string()))?;
            inst.call(hook::LOAD, &payload, Ctx::Lifecycle, runtime::LOAD_FUEL)?;
            Ok(inst)
        }));
        match started {
            Ok(Ok(mut inst)) => {
                let outs = inst.take_out();
                if let Some(slot) = self.plugins.get_mut(id) {
                    slot.instance = Some(inst);
                }
                self.forward(id, outs);
                Ok(())
            }
            Ok(Err(f)) => Err(self.fault(id, f)),
            Err(_) => Err(self.fault(id, Fault::Panic)),
        }
    }

    fn run_hook(
        &mut self,
        id: &str,
        hook: i32,
        ctx: Ctx,
        payload: &[u8],
        fuel: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        self.ensure_instance(id)?;
        let Some(inst) = self.plugins.get_mut(id).and_then(|s| s.instance.as_mut()) else {
            return Err("el plugin no está activo".into());
        };
        let r = catch_unwind(AssertUnwindSafe(|| inst.call(hook, payload, ctx, fuel)));
        match r {
            Ok(Ok(out)) => {
                let outs = inst.take_out();
                self.forward(id, outs);
                Ok(out)
            }
            Ok(Err(f)) => Err(self.fault(id, f)),
            Err(_) => Err(self.fault(id, Fault::Panic)),
        }
    }

    /// Records a fault: the instance is dropped (restarted on the next
    /// call), and after [`MAX_FAULTS`] the plugin is removed and reported.
    fn fault(&mut self, id: &str, f: Fault) -> String {
        let mut disabled = false;
        if let Some(slot) = self.plugins.get_mut(id) {
            slot.instance = None;
            slot.faults += 1;
            disabled = slot.faults >= MAX_FAULTS;
        }
        if disabled {
            self.plugins.remove(id);
        }
        let reason = f.to_string();
        log::warn!("plugin {id} {reason}");
        self.send(Out::Faulted {
            plugin: id.to_owned(),
            reason: reason.clone(),
            disabled,
        });
        reason
    }

    /// Hands a clean call's requests to the host; HTTP fetches run on a
    /// short-lived thread that posts the answer back as a job.
    fn forward(&mut self, id: &str, outs: Vec<Out>) {
        let mut any = false;
        for out in outs {
            match out {
                Out::Http {
                    plugin,
                    request_id,
                    req,
                } => {
                    let hosts = self
                        .plugins
                        .get(id)
                        .map(|s| s.hosts.clone())
                        .unwrap_or_default();
                    let shared = self.shared.clone();
                    let wake = self.cfg.wake.clone();
                    let spawned = std::thread::Builder::new()
                        .name("ferrosurf-plugin-http".into())
                        .spawn({
                            let plugin = plugin.clone();
                            move || {
                                let response = crate::http::fetch(request_id, &req, &hosts);
                                push(&shared, Job::HttpDone { plugin, response });
                                wake();
                            }
                        });
                    if spawned.is_err() {
                        push(
                            &self.shared,
                            Job::HttpDone {
                                plugin,
                                response: HttpResponse {
                                    request_id,
                                    error: Some("no se pudo iniciar la petición".into()),
                                    ..Default::default()
                                },
                            },
                        );
                    }
                }
                other => {
                    let _ = self.cfg.out.send(other);
                    any = true;
                }
            }
        }
        if any {
            (self.cfg.wake)();
        }
    }
}
