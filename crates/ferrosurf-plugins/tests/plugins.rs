//! End-to-end tests of the plugin host with tiny modules written in WAT, so
//! they need no wasm32 target. Each module answers fixed MessagePack
//! outputs per hook, spins, or makes one host call.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ferrosurf_extensions::{RequestInfo, ResourceType};
use ferrosurf_mcp::ToolHandler;
use ferrosurf_plugins::sdk::abi::{
    self, ActiveTab, KvSetReq, McpToolOutput, OmniboxOutput, PageMessageOutput, RequestOutput,
    SettingValue, Suggestion, TextReq, hook, op,
};
use ferrosurf_plugins::{HostRequest, Permission, PluginHost, RequestDecision};
use url::Url;

/// What a hook does in a test module.
#[derive(Clone)]
enum Behavior {
    /// Return this MessagePack.
    Output(Vec<u8>),
    /// Loop forever (fuel exhaustion).
    Spin,
    /// Make one host call; return `on_err` if it fails, else `on_ok`.
    Call {
        op: i32,
        input: Vec<u8>,
        on_err: Vec<u8>,
        on_ok: Vec<u8>,
    },
    /// Make one host call and return nothing.
    CallQuiet { op: i32, input: Vec<u8> },
}

fn esc(b: &[u8]) -> String {
    b.iter().map(|x| format!("\\{x:02x}")).collect()
}

/// A complete plugin module answering `hooks`.
fn wat_plugin(hooks: &[(i32, Behavior)]) -> Vec<u8> {
    let mut data = String::new();
    let mut offset = 1024u32;
    let mut place = |b: &[u8]| -> (u32, usize) {
        let at = offset;
        data.push_str(&format!("(data (i32.const {at}) \"{}\")\n", esc(b)));
        offset += b.len() as u32 + 8;
        (at, b.len())
    };
    let packed = |p: (u32, usize)| ((u64::from(p.0) << 32) | p.1 as u64) as i64;
    let mut body = String::new();
    for (h, b) in hooks {
        body.push_str(&format!(
            "(if (i32.eq (local.get 0) (i32.const {h})) (then\n"
        ));
        match b {
            Behavior::Output(o) => {
                let p = place(o);
                body.push_str(&format!("(return (i64.const {}))", packed(p)));
            }
            Behavior::Spin => body.push_str("(loop $l (br $l))"),
            Behavior::Call {
                op,
                input,
                on_err,
                on_ok,
            } => {
                let i = place(input);
                let e = place(on_err);
                let k = place(on_ok);
                body.push_str(&format!(
                    "(if (i32.lt_s (call $host_call (i32.const {op}) (i32.const {}) (i32.const {})) (i32.const 0))
                       (then (return (i64.const {})))
                       (else (return (i64.const {}))))",
                    i.0,
                    i.1,
                    packed(e),
                    packed(k)
                ));
            }
            Behavior::CallQuiet { op, input } => {
                let i = place(input);
                body.push_str(&format!(
                    "(drop (call $host_call (i32.const {op}) (i32.const {}) (i32.const {})))
                     (return (i64.const 0))",
                    i.0, i.1
                ));
            }
        }
        body.push_str("))\n");
    }
    let wat = format!(
        r#"(module
  (import "ferrosurf" "host_call" (func $host_call (param i32 i32 i32) (result i32)))
  (import "ferrosurf" "host_take" (func $host_take (param i32 i32) (result i32)))
  (memory (export "memory") 2)
  {data}
  (func (export "fs_abi_version") (result i32) (i32.const 1))
  (func (export "fs_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "fs_call") (param i32 i32 i32) (result i64)
    {body}
    (i64.const 0)))"#
    );
    wat::parse_str(&wat).unwrap()
}

fn out<T: serde::Serialize>(v: &T) -> Behavior {
    Behavior::Output(abi::encode(v).unwrap())
}

const HEAD: &str = "name = \"T\"\nversion = \"1.0.0\"\napi_version = 1\n";

fn install(dir: &Path, id: &str, extra: &str, wasm: &[u8]) {
    let d = dir.join(id);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("plugin.toml"),
        format!("id = \"{id}\"\n{HEAD}{extra}"),
    )
    .unwrap();
    std::fs::write(d.join("plugin.wasm"), wasm).unwrap();
}

struct Rig {
    _tmp: tempfile::TempDir,
    plugins: std::path::PathBuf,
    data: std::path::PathBuf,
    wakes: Arc<AtomicUsize>,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&plugins).unwrap();
        Self {
            _tmp: tmp,
            plugins,
            data,
            wakes: Arc::default(),
        }
    }

    fn open(&self) -> PluginHost {
        let w = self.wakes.clone();
        PluginHost::open(
            self.plugins.clone(),
            self.data.clone(),
            Arc::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }
}

fn req<'a>(url: &'a Url, t: ResourceType) -> RequestInfo<'a> {
    RequestInfo {
        url,
        initiator: None,
        resource_type: t,
    }
}

fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

const REQ_TOML: &str = "permissions = [\"requests\"]\nrequest_filters = [\"host:twitter.com\"]\n";

#[test]
fn zero_cost_when_nothing_is_enabled() {
    let rig = Rig::new();
    let wasm = wat_plugin(&[(hook::REQUEST, out(&RequestOutput::block()))]);
    install(&rig.plugins, "blocker", REQ_TOML, &wasm);

    let host = rig.open();
    let s = host.engine_stats();
    assert!(!s.running);
    assert_eq!(
        (s.engines_created, s.threads_started, s.threads_alive),
        (0, 0, 0)
    );

    // Installed but disabled: still no engine, requests pass untouched.
    assert_eq!(host.list().len(), 1);
    let u = Url::parse("https://twitter.com/a").unwrap();
    assert_eq!(
        host.check_request(&req(&u, ResourceType::MainFrame)),
        RequestDecision::Allow
    );
    assert!(host.commands().is_empty() && host.mcp_tools().is_empty());
    assert!(host.content_scripts().is_empty() && host.drain_host_requests().is_empty());
    assert_eq!(host.engine_stats().threads_started, 0);

    // Enabling starts one engine and one thread; disabling stops it.
    host.enable("blocker", &[Permission::Requests]).unwrap();
    let s = host.engine_stats();
    assert!(s.running && s.engines_created <= 1);
    assert!(wait_for(|| host.engine_stats().threads_alive == 1));
    host.disable("blocker");
    assert!(!host.engine_stats().running);
    assert!(wait_for(|| host.engine_stats().threads_alive == 0));
    assert_eq!(host.engine_stats().threads_started, 1);
    assert_eq!(
        host.check_request(&req(&u, ResourceType::MainFrame)),
        RequestDecision::Allow
    );
}

#[test]
fn enabled_plugins_start_on_open_and_nothing_else_does() {
    let rig = Rig::new();
    let wasm = wat_plugin(&[(hook::REQUEST, out(&RequestOutput::block()))]);
    install(&rig.plugins, "blocker", REQ_TOML, &wasm);
    rig.open()
        .enable("blocker", &[Permission::Requests])
        .unwrap();
    // A fresh process: state.json says enabled -> engine starts at open.
    let host = rig.open();
    assert!(host.engine_stats().running);
    let u = Url::parse("https://twitter.com/a").unwrap();
    assert_eq!(
        host.check_request(&req(&u, ResourceType::MainFrame)),
        RequestDecision::Block
    );
}

#[test]
fn request_hook_decisions_and_filters() {
    let rig = Rig::new();
    let redirect = RequestOutput::redirect("https://nitter.example/a");
    install(
        &rig.plugins,
        "redir",
        REQ_TOML,
        &wat_plugin(&[(hook::REQUEST, out(&redirect))]),
    );
    install(
        &rig.plugins,
        "same",
        "permissions = [\"requests\"]\nrequest_filters = [\"host:loop.example\"]\n",
        &wat_plugin(&[(
            hook::REQUEST,
            out(&RequestOutput::redirect("https://loop.example/x")),
        )]),
    );
    install(
        &rig.plugins,
        "bad",
        "permissions = [\"requests\"]\nrequest_filters = [\"host:bad.example\"]\n",
        &wat_plugin(&[(
            hook::REQUEST,
            out(&RequestOutput::redirect("javascript:alert(1)")),
        )]),
    );
    install(
        &rig.plugins,
        "ads",
        "permissions = [\"requests\"]\nrequest_filters = [\"/ads/\"]\n",
        &wat_plugin(&[(hook::REQUEST, out(&RequestOutput::block()))]),
    );
    let host = rig.open();
    for id in ["redir", "same", "bad", "ads"] {
        host.enable(id, &[Permission::Requests]).unwrap();
    }
    let check = |u: &str, t| host.check_request(&req(&Url::parse(u).unwrap(), t));
    assert_eq!(
        check("https://twitter.com/a", ResourceType::MainFrame),
        RequestDecision::Redirect(Url::parse("https://nitter.example/a").unwrap())
    );
    assert_eq!(
        check("https://mobile.twitter.com/a", ResourceType::Image),
        RequestDecision::Redirect(Url::parse("https://nitter.example/a").unwrap())
    );
    assert_eq!(
        check("https://cdn.test/ads/x.js", ResourceType::Script),
        RequestDecision::Block
    );
    // Redirect to itself and to a non-web scheme are ignored.
    assert_eq!(
        check("https://loop.example/x", ResourceType::MainFrame),
        RequestDecision::Allow
    );
    assert_eq!(
        check("https://bad.example/", ResourceType::MainFrame),
        RequestDecision::Allow
    );
    // No filter matches: allowed without reaching any plugin.
    assert_eq!(
        check("https://example.org/", ResourceType::MainFrame),
        RequestDecision::Allow
    );
    // Without the `requests` grant the hook is never called.
    host.disable("ads");
    host.enable("ads", &[]).unwrap();
    assert_eq!(
        check("https://cdn.test/ads/x.js", ResourceType::Script),
        RequestDecision::Allow
    );
}

#[test]
fn fuel_exhaustion_allows_then_disables_the_plugin() {
    let rig = Rig::new();
    install(
        &rig.plugins,
        "spin",
        REQ_TOML,
        &wat_plugin(&[(hook::REQUEST, Behavior::Spin)]),
    );
    let host = rig.open();
    host.enable("spin", &[Permission::Requests]).unwrap();
    let u = Url::parse("https://twitter.com/a").unwrap();
    let mut disabled = None;
    let end = Instant::now() + Duration::from_secs(10);
    while disabled.is_none() && Instant::now() < end {
        // Every call is allowed (fuel ran out or timed out), never blocked.
        assert_eq!(
            host.check_request(&req(&u, ResourceType::Xhr)),
            RequestDecision::Allow
        );
        disabled = host
            .drain_host_requests()
            .into_iter()
            .find(|r| matches!(r, HostRequest::PluginDisabled { .. }));
    }
    let Some(HostRequest::PluginDisabled { plugin, reason }) = disabled else {
        panic!("the plugin was never disabled");
    };
    assert_eq!(plugin, "spin");
    assert!(reason.contains("presupuesto"), "{reason}");
    let info = &host.list()[0];
    assert!(!info.enabled && !info.running);
    assert!(
        info.error
            .as_deref()
            .is_some_and(|e| e.contains("presupuesto"))
    );
    assert!(
        !host.engine_stats().running,
        "last plugin gone: engine stops"
    );
    // The decision survives a restart.
    assert!(!rig.open().engine_stats().running);
}

#[tokio::test]
async fn permissions_gate_host_ops_in_tools() {
    let rig = Rig::new();
    let set = abi::encode(&KvSetReq {
        key: "k".into(),
        value: b"v".to_vec(),
    })
    .unwrap();
    let wasm = wat_plugin(&[(
        hook::MCP_TOOL,
        Behavior::Call {
            op: op::KV_SET,
            input: set,
            on_err: abi::encode(&McpToolOutput::error("denied")).unwrap(),
            on_ok: abi::encode(&McpToolOutput::text("stored")).unwrap(),
        },
    )]);
    install(
        &rig.plugins,
        "notes",
        "permissions = [\"mcp_tools\", \"storage\"]\n[[tools]]\nname = \"save\"\ndescription = \"Saves\"\ninput_schema = '{\"type\":\"object\"}'\n",
        &wasm,
    );
    let host = rig.open();
    let handler = host.tool_handler();
    assert!(handler.tools().is_empty(), "off by default");
    let name = "plugin_notes__save";

    // MCP granted, storage not: the host call is DENIED inside the plugin.
    host.enable("notes", &[Permission::McpTools]).unwrap();
    let tools = handler.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, name);
    let r = handler.call(name, serde_json::json!({})).await;
    assert!(r.is_error, "{r:?}");

    // Both granted: the same call succeeds.
    host.disable("notes");
    host.enable("notes", &[Permission::McpTools, Permission::Storage])
        .unwrap();
    let r = handler.call(name, serde_json::json!({})).await;
    assert!(!r.is_error, "{r:?}");

    // Without `mcp_tools` the tool is neither listed nor callable.
    host.disable("notes");
    host.enable("notes", &[Permission::Storage]).unwrap();
    assert!(handler.tools().is_empty());
    assert!(handler.call(name, serde_json::json!({})).await.is_error);
}

#[test]
fn commands_run_and_their_requests_are_drained() {
    let rig = Rig::new();
    let toast = abi::encode(&TextReq {
        text: "Nota guardada".into(),
    })
    .unwrap();
    install(
        &rig.plugins,
        "cmd",
        "[[commands]]\nid = \"save\"\ntitle = \"Guardar nota de esta página\"\nshortcut = \"Ctrl+Shift+S\"\n",
        &wat_plugin(&[(
            hook::COMMAND,
            Behavior::CallQuiet {
                op: op::TOAST,
                input: toast,
            },
        )]),
    );
    let host = rig.open();
    assert!(host.commands().is_empty());
    host.enable("cmd", &[]).unwrap();
    let cmds = host.commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].title, "Guardar nota de esta página");
    assert!(host.run_command("cmd", "nope", None).is_err());
    let tab = ActiveTab {
        tab_id: 3,
        url: "https://example.com/".into(),
        title: "Ex".into(),
    };
    host.run_command("cmd", "save", Some(&tab)).unwrap();
    let mut got = Vec::new();
    assert!(wait_for(|| {
        got.extend(host.drain_host_requests());
        !got.is_empty()
    }));
    assert_eq!(
        got[0],
        HostRequest::Toast {
            plugin: "cmd".into(),
            text: "Nota guardada".into()
        }
    );
    assert!(
        rig.wakes.load(Ordering::SeqCst) > 0,
        "the UI thread was woken"
    );
}

#[test]
fn omnibox_and_page_messages() {
    let rig = Rig::new();
    let omni = OmniboxOutput {
        suggestions: vec![
            Suggestion {
                title: "Repo".into(),
                url: "https://github.com/rust-lang/rust".into(),
            },
            Suggestion {
                title: "evil".into(),
                url: "javascript:alert(1)".into(),
            },
        ],
        navigate: Some("https://github.com/rust-lang".into()),
    };
    install(
        &rig.plugins,
        "gh",
        "omnibox_keyword = \"gh\"\n",
        &wat_plugin(&[(hook::OMNIBOX, out(&omni))]),
    );
    let reply = PageMessageOutput {
        reply_json: "{\"ok\":true}".into(),
    };
    install(
        &rig.plugins,
        "pm",
        "permissions = [\"page_messages\"]\npage_message_origins = [\"https://*.example.com/*\"]\n",
        &wat_plugin(&[(hook::PAGE_MESSAGE, out(&reply))]),
    );
    let host = rig.open();
    assert!(host.omnibox("gh", "rust").is_none(), "off by default");
    host.enable("gh", &[]).unwrap();
    host.enable("pm", &[Permission::PageMessages]).unwrap();
    assert!(host.omnibox("zz", "x").is_none());
    let r = host
        .omnibox("gh", "rust")
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(r.plugin_id, "gh");
    assert_eq!(r.suggestions.len(), 1, "non-web URLs are dropped");
    assert_eq!(r.navigate.as_deref(), Some("https://github.com/rust-lang"));

    let call = |origin: &str| {
        host.page_message("pm", origin, 1, serde_json::json!({"a": 1}))
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    };
    assert_eq!(
        call("https://app.example.com").unwrap(),
        serde_json::json!({"ok": true})
    );
    assert!(
        call("https://evil.com").is_err(),
        "origin not in page_message_origins"
    );
    host.disable("pm");
    assert!(call("https://app.example.com").is_err());
    host.enable("pm", &[]).unwrap();
    assert!(
        call("https://app.example.com").is_err(),
        "needs the page_messages grant"
    );
}

#[test]
fn content_scripts_follow_the_page_scripting_grant() {
    let rig = Rig::new();
    install(
        &rig.plugins,
        "inj",
        "permissions = [\"page_scripting\"]\n[[content]]\nmatches = [\"https://*.example.com/*\"]\njs = \"inject.js\"\ncss = \"inject.css\"\n",
        &wat_plugin(&[]),
    );
    std::fs::write(
        rig.plugins.join("inj/inject.js"),
        "console.log('hi from plugin');",
    )
    .unwrap();
    std::fs::write(rig.plugins.join("inj/inject.css"), "body{color:red}").unwrap();
    let host = rig.open();
    host.enable("inj", &[]).unwrap();
    assert!(host.content_scripts().is_empty());
    host.disable("inj");
    host.enable("inj", &[Permission::PageScripting]).unwrap();
    let scripts = host.content_scripts();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].0, "inj");
    assert!(scripts[0].1.contains("hi from plugin") && scripts[0].1.contains("body{color:red}"));
    host.disable("inj");
    assert!(host.content_scripts().is_empty());
}

#[test]
fn install_update_reapproval_settings_and_uninstall() {
    let rig = Rig::new();
    let src = tempfile::tempdir().unwrap();
    let wasm = wat_plugin(&[]);
    let mk = |extra: &str, version: &str| {
        let d = src.path().join("p");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("plugin.toml"),
            format!("id = \"p\"\nname = \"P\"\nversion = \"{version}\"\napi_version = 1\n{extra}"),
        )
        .unwrap();
        std::fs::write(d.join("plugin.wasm"), &wasm).unwrap();
        d
    };
    let settings = "[[settings]]\nkey = \"host\"\ntype = \"string\"\nlabel = \"Host\"\ndefault = \"a.example\"\n";
    let perms = "permissions = [\"storage\"]\n";
    let host = rig.open();
    assert_eq!(host.install_from(&mk(settings, "1")).unwrap(), "p");
    host.enable("p", &[]).unwrap();
    assert!(host.list()[0].running);

    // Settings: schema-checked, persisted, effective values reported.
    assert!(
        host.set_setting("p", "host", SettingValue::Bool(true))
            .is_err()
    );
    assert!(host.set_setting("p", "nope", SettingValue::Int(1)).is_err());
    host.set_setting("p", "host", SettingValue::Str("b.example".into()))
        .unwrap();
    assert_eq!(
        host.list()[0].settings[0].value,
        SettingValue::Str("b.example".into())
    );
    assert_eq!(
        rig.open().list()[0].settings[0].value,
        SettingValue::Str("b.example".into())
    );

    // An update that asks for more permissions stops until re-approved.
    let more = format!("{perms}{settings}");
    host.install_from(&mk(&more, "2")).unwrap();
    let info = host.list().remove(0);
    assert_eq!(info.manifest.version, "2");
    assert!(info.enabled && info.needs_reapproval && !info.running);
    assert!(!host.engine_stats().running);
    host.enable("p", &[Permission::Storage]).unwrap();
    assert!(host.list()[0].running && !host.list()[0].needs_reapproval);

    // Hot reload keeps it running; uninstall removes everything.
    host.reload_all();
    assert!(host.list()[0].running);
    host.uninstall("p").unwrap();
    assert!(host.list().is_empty());
    assert!(!rig.plugins.join("p").exists());
    assert!(!host.engine_stats().running);
    assert!(host.enable("p", &[]).is_err());
}

#[test]
fn broken_modules_are_reported_not_fatal() {
    let rig = Rig::new();
    install(&rig.plugins, "junk", "", b"\0asm\x01\0\0\0garbage");
    std::fs::create_dir_all(rig.plugins.join("empty")).unwrap();
    let host = rig.open();
    assert_eq!(host.broken().len(), 1);
    host.enable("junk", &[]).unwrap_or(());
    assert!(wait_for(|| {
        host.drain_host_requests()
            .iter()
            .any(|r| matches!(r, HostRequest::PluginDisabled { .. }))
            || !host.list()[0].enabled
    }));
    assert!(wait_for(|| !host.engine_stats().running));
}

/// The real SDK-built examples (`examples/plugins/build.sh`) run in the
/// host. Skipped when they were not built (no wasm32 target).
#[test]
fn built_example_plugins_work() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/dist");
    if !dist.join("privacy-redirects/plugin.wasm").exists() {
        eprintln!("examples not built; skipping");
        return;
    }
    let rig = Rig::new();
    let host = rig.open();
    for id in ["privacy-redirects", "quick-notes", "gh-omnibox"] {
        assert_eq!(host.install_from(&dist.join(id)).unwrap(), id);
    }
    let all = |id: &str| {
        host.list()
            .into_iter()
            .find(|p| p.manifest.id == id)
            .unwrap()
            .manifest
            .permissions
    };
    for id in ["privacy-redirects", "quick-notes", "gh-omnibox"] {
        host.enable(id, &all(id)).unwrap();
    }

    // privacy-redirects: off until the setting is on.
    let u = Url::parse("https://twitter.com/rustlang").unwrap();
    assert_eq!(
        host.check_request(&req(&u, ResourceType::MainFrame)),
        RequestDecision::Allow
    );
    host.set_setting(
        "privacy-redirects",
        "twitter_enabled",
        SettingValue::Bool(true),
    )
    .unwrap();
    let want = RequestDecision::Redirect(Url::parse("https://nitter.net/rustlang").unwrap());
    assert!(wait_for(|| host
        .check_request(&req(&u, ResourceType::MainFrame))
        == want));
    assert_eq!(
        host.check_request(&req(&u, ResourceType::Image)),
        RequestDecision::Allow
    );

    // gh-omnibox
    let r = host
        .omnibox("gh", "rust-lang/rust")
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(r.suggestions[0].url, "https://github.com/rust-lang/rust");

    // quick-notes: save through the palette command, read through MCP.
    let tab = ActiveTab {
        tab_id: 1,
        url: "https://example.com/".into(),
        title: "Example".into(),
    };
    host.set_active_tab(Some(tab.clone()));
    assert_eq!(host.commands()[0].id, "save");
    host.run_command("quick-notes", "save", Some(&tab)).unwrap();
    let mut toasts = Vec::new();
    assert!(wait_for(|| {
        toasts.extend(host.drain_host_requests());
        toasts
            .iter()
            .any(|t| matches!(t, HostRequest::Toast { .. }))
    }));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let out = rt.block_on(
        host.tool_handler()
            .call("plugin_quick_notes__list_notes", serde_json::json!({})),
    );
    assert!(!out.is_error, "{out:?}");
    let text = format!("{:?}", out.content);
    assert!(text.contains("https://example.com/"), "{text}");
}
