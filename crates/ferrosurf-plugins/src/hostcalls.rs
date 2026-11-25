//! The host side of `ferrosurf.host_call`: permission and context checks,
//! limits, and the requests handed back to the UI thread.
//!
//! Nothing here touches the network, the profile, the cookie jar or
//! arbitrary files. Calls that act on the browser only produce an [`Out`]
//! request; the app performs it through its own code paths.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ferrosurf_plugin_sdk::abi::{
    self, ActiveTab, HttpRequest, InjectJsReq, KeyReq, KvSetReq, LogReq, NotifyReq, OpenUrlReq,
    SetSettingReq, SettingValue, TextReq, err, op,
};
use url::Url;
use wasmi::StoreLimits;

use crate::kv::Kv;
use crate::manifest::{Manifest, Permission};

/// Host calls one hook invocation may make.
pub const MAX_CALLS_PER_HOOK: u32 = 256;
/// Largest host-call input.
pub const MAX_CALL_INPUT: usize = 256 * 1024;
pub const MAX_TOAST: usize = 200;
const MAX_LOG: usize = 1000;
const MAX_TITLE: usize = 100;
const MAX_BODY: usize = 400;
const MAX_URL: usize = 2048;
pub const MAX_CODE: usize = 64 * 1024;
pub const MAX_HTTP_BODY: usize = 64 * 1024;
const MAX_HTTP_HEADERS: usize = 16;
/// HTTP requests one plugin may have in flight.
pub const MAX_HTTP_INFLIGHT: usize = 4;
const MAX_INJECTS_PER_HOOK: u32 = 4;
/// Header names a plugin may not set (the client owns these).
const FORBIDDEN_HEADERS: [&str; 9] = [
    "host",
    "cookie",
    "connection",
    "content-length",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "te",
    "trailer",
];

/// Something only the UI thread can do, queued by a plugin.
/// Fetch with [`crate::PluginHost::drain_host_requests`]. `Debug` never
/// prints page code or clipboard content.
#[derive(Clone, PartialEq)]
pub enum HostRequest {
    /// Show a short in-browser toast.
    Toast { plugin: String, text: String },
    /// Open a tab (`http`/`https` only; the user ran a palette command).
    OpenUrl {
        plugin: String,
        url: String,
        background: bool,
    },
    /// Run JavaScript in a tab (`page_scripting`).
    InjectJs {
        plugin: String,
        tab_id: u64,
        code: String,
    },
    /// Show a system notification.
    Notify {
        plugin: String,
        title: String,
        body: String,
    },
    /// Copy text to the clipboard (the user ran a palette command).
    ClipboardWrite { plugin: String, text: String },
    /// The host turned a plugin off (repeated faults, broken module); the
    /// state file is already updated. Refresh the Plugins page.
    PluginDisabled { plugin: String, reason: String },
    /// A plugin changed one of its own settings (already persisted).
    SettingChanged {
        plugin: String,
        key: String,
        value: SettingValue,
    },
}

impl std::fmt::Debug for HostRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Toast { .. } => "Toast",
            Self::OpenUrl { .. } => "OpenUrl",
            Self::InjectJs { .. } => "InjectJs",
            Self::Notify { .. } => "Notify",
            Self::ClipboardWrite { .. } => "ClipboardWrite",
            Self::PluginDisabled { .. } => "PluginDisabled",
            Self::SettingChanged { .. } => "SettingChanged",
        };
        write!(f, "HostRequest::{name}")
    }
}

/// Worker-internal messages to the host.
pub enum Out {
    Host(HostRequest),
    /// The plugin set one of its settings (needs persisting).
    SettingSaved {
        plugin: String,
        key: String,
        value: SettingValue,
    },
    /// The plugin trapped, ran out of fuel, panicked or failed to load.
    Faulted {
        plugin: String,
        reason: String,
        disabled: bool,
    },
    /// An `http_fetch` to perform (the worker spawns a thread for it).
    Http {
        plugin: String,
        request_id: u64,
        req: HttpRequest,
    },
}

/// Which hook is running: decides which host calls are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// Load, unload, setting changes, HTTP answers.
    Lifecycle,
    /// A network request: synchronous, hot path. Only log, settings
    /// (read) and clock.
    Request,
    Navigation {
        tab_id: u64,
    },
    /// A palette command: a user action.
    Command {
        tab_id: u64,
    },
    Omnibox,
    McpTool,
    /// A page is waiting: log, settings, clock and storage only.
    PageMessage,
}

/// Per-instance host state (the wasmi store data).
pub struct Env {
    pub manifest: Arc<Manifest>,
    pub granted: Arc<Vec<Permission>>,
    pub hosts: Arc<Vec<String>>,
    pub settings: Rc<RefCell<BTreeMap<String, SettingValue>>>,
    pub ctx: Ctx,
    /// Requests produced by the current call; forwarded when it returns
    /// cleanly, dropped if it faults.
    pub out: Vec<Out>,
    pub calls: u32,
    /// Result of the last `host_call`, read by `host_take`.
    pub pending: Vec<u8>,
    pub kv: Rc<RefCell<Kv>>,
    pub limits: StoreLimits,
    /// The instance's linear memory, set once instantiated.
    pub memory: Option<wasmi::Memory>,
    pub now_ms: fn() -> u64,
    pub active_tab: Arc<Mutex<Option<ActiveTab>>>,
    pub http_inflight: Arc<AtomicUsize>,
    next_request: u64,
    toasts: u32,
    notifies: u32,
    opens: u32,
    injects: u32,
}

pub fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Env {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manifest: Arc<Manifest>,
        granted: Arc<Vec<Permission>>,
        hosts: Arc<Vec<String>>,
        settings: Rc<RefCell<BTreeMap<String, SettingValue>>>,
        kv: Rc<RefCell<Kv>>,
        limits: StoreLimits,
        active_tab: Arc<Mutex<Option<ActiveTab>>>,
        http_inflight: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            manifest,
            granted,
            hosts,
            settings,
            ctx: Ctx::Lifecycle,
            out: Vec::new(),
            calls: 0,
            pending: Vec::new(),
            kv,
            limits,
            memory: None,
            now_ms: system_now_ms,
            active_tab,
            http_inflight,
            next_request: 1,
            toasts: 0,
            notifies: 0,
            opens: 0,
            injects: 0,
        }
    }

    /// Resets the per-call state for a hook of kind `ctx`.
    pub fn begin(&mut self, ctx: Ctx) {
        self.ctx = ctx;
        self.out.clear();
        self.pending.clear();
        self.calls = 0;
        self.toasts = 0;
        self.notifies = 0;
        self.opens = 0;
        self.injects = 0;
    }

    pub fn has(&self, p: Permission) -> bool {
        self.granted.contains(&p)
    }

    fn need(&self, p: Permission) -> Result<(), i32> {
        if self.has(p) {
            Ok(())
        } else {
            Err(err::DENIED)
        }
    }

    fn id(&self) -> String {
        self.manifest.id.clone()
    }
}

fn dec<'a, T: serde::Deserialize<'a>>(input: &'a [u8]) -> Result<T, i32> {
    abi::decode(input).map_err(|_| err::BAD_INPUT)
}

fn enc<T: serde::Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, i32> {
    abi::encode(v).map_err(|_| err::BAD_INPUT)
}

/// Whether `op_id` may run at all in `ctx` (before permission checks).
fn allowed_in(ctx: Ctx, op_id: i32) -> bool {
    match ctx {
        Ctx::Request => matches!(op_id, op::LOG | op::GET_SETTING | op::NOW_MS),
        Ctx::PageMessage => matches!(
            op_id,
            op::LOG | op::GET_SETTING | op::NOW_MS | op::KV_GET | op::KV_SET | op::KV_DELETE
        ),
        _ => true,
    }
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// `http`/`https` URL of a sane length (open_url, redirects).
pub fn web_url(s: &str) -> Option<Url> {
    if s.len() > MAX_URL {
        return None;
    }
    Url::parse(s)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// Runs host operation `op_id` for the plugin in `env`.
pub fn handle(env: &mut Env, op_id: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
    if input.len() > MAX_CALL_INPUT {
        return Err(err::LIMIT);
    }
    env.calls += 1;
    if env.calls > MAX_CALLS_PER_HOOK {
        return Err(err::LIMIT);
    }
    if !allowed_in(env.ctx, op_id) {
        return Err(err::DENIED);
    }
    let ctx = env.ctx;
    let user_action = matches!(ctx, Ctx::Command { .. });
    match op_id {
        op::LOG => {
            let r: LogReq = dec(input)?;
            let target = format!("plugin:{}", env.manifest.id);
            let msg = clip(&r.message, MAX_LOG);
            match r.level {
                0 => log::error!(target: &target, "{msg}"),
                1 => log::warn!(target: &target, "{msg}"),
                2 => log::info!(target: &target, "{msg}"),
                _ => log::debug!(target: &target, "{msg}"),
            }
            Ok(Vec::new())
        }
        op::TOAST => {
            let r: TextReq = dec(input)?;
            if env.toasts >= 1 {
                return Err(err::LIMIT);
            }
            env.toasts += 1;
            let text = clip(r.text.trim(), MAX_TOAST);
            let plugin = env.id();
            env.out.push(Out::Host(HostRequest::Toast { plugin, text }));
            Ok(Vec::new())
        }
        op::GET_SETTING => {
            let r: KeyReq = dec(input)?;
            let v = env.settings.borrow().get(&r.key).cloned();
            enc(&v)
        }
        op::SET_SETTING => {
            let r: SetSettingReq = dec(input)?;
            let spec = env.manifest.setting(&r.key).ok_or(err::BAD_INPUT)?;
            spec.check(&r.value).map_err(|_| err::BAD_INPUT)?;
            env.settings
                .borrow_mut()
                .insert(r.key.clone(), r.value.clone());
            let plugin = env.id();
            env.out.push(Out::SettingSaved {
                plugin,
                key: r.key,
                value: r.value,
            });
            Ok(Vec::new())
        }
        op::KV_GET => {
            env.need(Permission::Storage)?;
            let r: KeyReq = dec(input)?;
            let v = env.kv.borrow_mut().get(&env.manifest.id, &r.key)?;
            enc(&v.map(ferrosurf_plugin_sdk::serde_bytes::ByteBuf::from))
        }
        op::KV_SET => {
            env.need(Permission::Storage)?;
            let r: KvSetReq = dec(input)?;
            env.kv
                .borrow_mut()
                .set(&env.manifest.id, &r.key, &r.value)?;
            Ok(Vec::new())
        }
        op::KV_DELETE => {
            env.need(Permission::Storage)?;
            let r: KeyReq = dec(input)?;
            env.kv.borrow_mut().delete(&env.manifest.id, &r.key)?;
            Ok(Vec::new())
        }
        op::HTTP_FETCH => {
            env.need(Permission::Network)?;
            let r: HttpRequest = dec(input)?;
            check_http(env, &r)?;
            if env.http_inflight.load(Ordering::Relaxed) >= MAX_HTTP_INFLIGHT {
                return Err(err::LIMIT);
            }
            env.http_inflight.fetch_add(1, Ordering::Relaxed);
            let request_id = env.next_request;
            env.next_request += 1;
            let plugin = env.id();
            env.out.push(Out::Http {
                plugin,
                request_id,
                req: r,
            });
            enc(&request_id)
        }
        op::OPEN_URL => {
            env.need(Permission::Tabs)?;
            if !user_action {
                return Err(err::NOT_USER_ACTION);
            }
            let r: OpenUrlReq = dec(input)?;
            let url = web_url(&r.url).ok_or(err::BAD_INPUT)?;
            if env.opens >= 1 {
                return Err(err::LIMIT);
            }
            env.opens += 1;
            let plugin = env.id();
            env.out.push(Out::Host(HostRequest::OpenUrl {
                plugin,
                url: url.into(),
                background: r.background,
            }));
            Ok(Vec::new())
        }
        op::INJECT_JS => {
            env.need(Permission::PageScripting)?;
            let tab = match ctx {
                Ctx::Command { tab_id } | Ctx::Navigation { tab_id } => tab_id,
                _ => return Err(err::DENIED),
            };
            let r: InjectJsReq = dec(input)?;
            if r.tab_id != tab {
                return Err(err::DENIED);
            }
            if r.code.len() > MAX_CODE || r.code.is_empty() {
                return Err(err::LIMIT);
            }
            if env.injects >= MAX_INJECTS_PER_HOOK {
                return Err(err::LIMIT);
            }
            env.injects += 1;
            let plugin = env.id();
            env.out.push(Out::Host(HostRequest::InjectJs {
                plugin,
                tab_id: r.tab_id,
                code: r.code,
            }));
            Ok(Vec::new())
        }
        op::CLIPBOARD_WRITE => {
            env.need(Permission::Clipboard)?;
            if !user_action {
                return Err(err::NOT_USER_ACTION);
            }
            let r: TextReq = dec(input)?;
            if r.text.len() > MAX_HTTP_BODY {
                return Err(err::LIMIT);
            }
            let plugin = env.id();
            env.out.push(Out::Host(HostRequest::ClipboardWrite {
                plugin,
                text: r.text,
            }));
            Ok(Vec::new())
        }
        op::NOW_MS => enc(&(env.now_ms)()),
        op::NOTIFY => {
            env.need(Permission::Notifications)?;
            let r: NotifyReq = dec(input)?;
            if env.notifies >= 1 {
                return Err(err::LIMIT);
            }
            env.notifies += 1;
            let plugin = env.id();
            env.out.push(Out::Host(HostRequest::Notify {
                plugin,
                title: clip(r.title.trim(), MAX_TITLE),
                body: clip(r.body.trim(), MAX_BODY),
            }));
            Ok(Vec::new())
        }
        op::GET_ACTIVE_TAB => {
            env.need(Permission::Tabs)?;
            let tab = env.active_tab.lock().ok().and_then(|t| t.clone());
            enc(&tab)
        }
        _ => Err(err::UNKNOWN_OP),
    }
}

/// HTTPS to a listed host, default port, no credentials in the URL.
pub fn check_http(env: &Env, r: &HttpRequest) -> Result<(), i32> {
    let u = Url::parse(&r.url).map_err(|_| err::BAD_INPUT)?;
    let host = u.host_str().map(str::to_ascii_lowercase);
    let ok = u.scheme() == "https"
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && host.as_ref().is_some_and(|h| env.hosts.contains(h));
    if !ok {
        return Err(err::HOST_NOT_ALLOWED);
    }
    if !matches!(r.method.as_str(), "GET" | "POST") {
        return Err(err::BAD_INPUT);
    }
    if r.body.len() > MAX_HTTP_BODY || r.headers.len() > MAX_HTTP_HEADERS {
        return Err(err::LIMIT);
    }
    for (k, v) in &r.headers {
        let lk = k.to_ascii_lowercase();
        if k.is_empty()
            || FORBIDDEN_HEADERS.contains(&lk.as_str())
            || k.chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == ':')
            || v.chars().any(char::is_control)
        {
            return Err(err::BAD_INPUT);
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// An `Env` over `manifest_extra` (extra TOML) with `grant`ed permissions.
    pub fn env(extra: &str, grant: &[Permission]) -> Env {
        let m = Manifest::parse(&format!(
            "id = \"p\"\nname = \"P\"\nversion = \"1\"\napi_version = 1\n{extra}"
        ))
        .unwrap();
        let hosts = m.network_hosts.clone();
        Env::new(
            Arc::new(m),
            Arc::new(grant.to_vec()),
            Arc::new(hosts),
            Rc::default(),
            Rc::new(RefCell::new(Kv::new(None))),
            wasmi::StoreLimitsBuilder::new().build(),
            Arc::default(),
            Arc::default(),
        )
    }

    fn call<T: serde::Serialize>(env: &mut Env, op_id: i32, v: &T) -> Result<Vec<u8>, i32> {
        handle(env, op_id, &abi::encode(v).unwrap())
    }

    #[test]
    fn missing_permissions_are_denied() {
        let mut e = env(
            "permissions = [\"storage\", \"clipboard\", \"tabs\", \"page_scripting\", \"notifications\", \"network\"]\nnetwork_hosts = [\"api.example.com\"]",
            &[],
        );
        e.begin(Ctx::Command { tab_id: 1 });
        let t = TextReq { text: "x".into() };
        let k = KeyReq { key: "k".into() };
        assert_eq!(call(&mut e, op::KV_GET, &k), Err(err::DENIED));
        assert_eq!(
            call(
                &mut e,
                op::KV_SET,
                &KvSetReq {
                    key: "k".into(),
                    value: vec![1]
                }
            ),
            Err(err::DENIED)
        );
        assert_eq!(call(&mut e, op::KV_DELETE, &k), Err(err::DENIED));
        assert_eq!(call(&mut e, op::CLIPBOARD_WRITE, &t), Err(err::DENIED));
        assert_eq!(call(&mut e, op::GET_ACTIVE_TAB, &()), Err(err::DENIED));
        assert_eq!(
            call(
                &mut e,
                op::OPEN_URL,
                &OpenUrlReq {
                    url: "https://a.com/".into(),
                    background: false
                }
            ),
            Err(err::DENIED)
        );
        assert_eq!(
            call(
                &mut e,
                op::INJECT_JS,
                &InjectJsReq {
                    tab_id: 1,
                    code: "1".into()
                }
            ),
            Err(err::DENIED)
        );
        assert_eq!(
            call(&mut e, op::NOTIFY, &NotifyReq::default()),
            Err(err::DENIED)
        );
        assert_eq!(
            call(
                &mut e,
                op::HTTP_FETCH,
                &HttpRequest {
                    method: "GET".into(),
                    url: "https://api.example.com/".into(),
                    ..Default::default()
                }
            ),
            Err(err::DENIED)
        );
        // Ungated ops still work.
        assert!(call(&mut e, op::TOAST, &t).is_ok());
        assert!(call(&mut e, op::NOW_MS, &()).is_ok());
        assert_eq!(
            e.out.len(),
            1,
            "denied calls must not queue anything: {}",
            e.out.len()
        );
    }

    #[test]
    fn kv_round_trip_with_permission() {
        let mut e = env("permissions = [\"storage\"]", &[Permission::Storage]);
        e.begin(Ctx::McpTool);
        let set = KvSetReq {
            key: "note:1".into(),
            value: b"hello".to_vec(),
        };
        call(&mut e, op::KV_SET, &set).unwrap();
        let got = call(
            &mut e,
            op::KV_GET,
            &KeyReq {
                key: "note:1".into(),
            },
        )
        .unwrap();
        let v: Option<ferrosurf_plugin_sdk::serde_bytes::ByteBuf> = abi::decode(&got).unwrap();
        assert_eq!(v.unwrap().as_ref(), b"hello");
        call(
            &mut e,
            op::KV_DELETE,
            &KeyReq {
                key: "note:1".into(),
            },
        )
        .unwrap();
        let got = call(
            &mut e,
            op::KV_GET,
            &KeyReq {
                key: "note:1".into(),
            },
        )
        .unwrap();
        assert_eq!(
            abi::decode::<Option<ferrosurf_plugin_sdk::serde_bytes::ByteBuf>>(&got).unwrap(),
            None
        );
        assert_eq!(
            call(&mut e, op::KV_GET, &KeyReq::default()),
            Err(err::BAD_INPUT)
        );
    }

    #[test]
    fn request_hook_is_restricted() {
        let mut e = env(
            "permissions = [\"requests\", \"storage\"]\nrequest_filters = [\"*\"]",
            &[Permission::Requests, Permission::Storage],
        );
        e.begin(Ctx::Request);
        assert_eq!(
            call(&mut e, op::KV_GET, &KeyReq { key: "k".into() }),
            Err(err::DENIED)
        );
        assert_eq!(
            call(&mut e, op::TOAST, &TextReq { text: "x".into() }),
            Err(err::DENIED)
        );
        assert!(call(&mut e, op::GET_SETTING, &KeyReq { key: "k".into() }).is_ok());
    }

    #[test]
    fn user_action_and_tab_scoping() {
        let all = [
            Permission::Clipboard,
            Permission::Tabs,
            Permission::PageScripting,
        ];
        let mut e = env(
            "permissions = [\"clipboard\", \"tabs\", \"page_scripting\"]",
            &all,
        );
        let t = TextReq { text: "x".into() };
        let open = OpenUrlReq {
            url: "https://a.com/".into(),
            background: true,
        };
        e.begin(Ctx::Navigation { tab_id: 7 });
        assert_eq!(
            call(&mut e, op::CLIPBOARD_WRITE, &t),
            Err(err::NOT_USER_ACTION)
        );
        assert_eq!(call(&mut e, op::OPEN_URL, &open), Err(err::NOT_USER_ACTION));
        let inj = |tab_id| InjectJsReq {
            tab_id,
            code: "document.title".into(),
        };
        assert!(call(&mut e, op::INJECT_JS, &inj(7)).is_ok());
        assert_eq!(call(&mut e, op::INJECT_JS, &inj(8)), Err(err::DENIED));
        e.begin(Ctx::McpTool);
        assert_eq!(call(&mut e, op::INJECT_JS, &inj(7)), Err(err::DENIED));
        e.begin(Ctx::Command { tab_id: 7 });
        assert!(call(&mut e, op::CLIPBOARD_WRITE, &t).is_ok());
        assert!(call(&mut e, op::OPEN_URL, &open).is_ok());
        assert_eq!(
            call(&mut e, op::OPEN_URL, &open),
            Err(err::LIMIT),
            "one per hook"
        );
        let bad = OpenUrlReq {
            url: "javascript:alert(1)".into(),
            background: false,
        };
        e.begin(Ctx::Command { tab_id: 7 });
        assert_eq!(call(&mut e, op::OPEN_URL, &bad), Err(err::BAD_INPUT));
    }

    #[test]
    fn http_is_https_and_allowlisted() {
        let mut e = env(
            "permissions = [\"network\"]\nnetwork_hosts = [\"api.example.com\"]",
            &[Permission::Network],
        );
        e.begin(Ctx::Command { tab_id: 0 });
        let get = |url: &str| HttpRequest {
            method: "GET".into(),
            url: url.into(),
            ..Default::default()
        };
        assert!(call(&mut e, op::HTTP_FETCH, &get("https://api.example.com/x")).is_ok());
        for bad in [
            "http://api.example.com/",
            "https://other.example.com/",
            "https://api.example.com:8443/",
            "https://user@api.example.com/",
            "https://api.example.com.evil.net/",
        ] {
            assert_eq!(
                call(&mut e, op::HTTP_FETCH, &get(bad)),
                Err(err::HOST_NOT_ALLOWED),
                "{bad}"
            );
        }
        let mut h = get("https://api.example.com/");
        h.headers = vec![("Cookie".into(), "a=b".into())];
        assert_eq!(call(&mut e, op::HTTP_FETCH, &h), Err(err::BAD_INPUT));
    }

    #[test]
    fn settings_are_schema_checked() {
        let mut e = env(
            "[[settings]]\nkey=\"n\"\ntype=\"int\"\nlabel=\"N\"\ndefault=1\nmax=3",
            &[],
        );
        e.begin(Ctx::McpTool);
        let set = |v| SetSettingReq {
            key: "n".into(),
            value: SettingValue::Int(v),
        };
        assert!(call(&mut e, op::SET_SETTING, &set(2)).is_ok());
        assert_eq!(call(&mut e, op::SET_SETTING, &set(9)), Err(err::BAD_INPUT));
        let v = call(&mut e, op::GET_SETTING, &KeyReq { key: "n".into() }).unwrap();
        assert_eq!(
            abi::decode::<Option<SettingValue>>(&v).unwrap(),
            Some(SettingValue::Int(2))
        );
        assert_eq!(call(&mut e, 99, &()), Err(err::UNKNOWN_OP));
    }
}
