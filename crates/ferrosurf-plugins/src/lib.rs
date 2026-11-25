//! FerroSurf native plugin host: WebAssembly plugins on `wasmi`.
//!
//! A plugin is a core-wasm module (`wasm32-unknown-unknown`, no WASI) plus a
//! `plugin.toml` manifest, built with `ferrosurf-plugin-sdk`. Plugins are
//! **off by default**, enabled explicitly with the permissions the user
//! accepts, sandboxed by fuel / memory / time limits, and cost nothing when
//! none is enabled: no engine, no thread (see [`PluginHost::engine_stats`]).
//! The ABI is documented in `docs/plugins.md`.
//!
//! # Public API
//!
//! Create one [`PluginHost`] at startup (cheap: one read of `state.json`) and
//! share clones of it:
//!
//! ```ignore
//! let host = PluginHost::open(plugins_dir, data_dir, Arc::new(move || proxy.wake()));
//! ```
//!
//! | Area | Calls |
//! |---|---|
//! | Plugins page | [`PluginHost::list`], [`broken`](PluginHost::broken), [`enable`](PluginHost::enable) (with the permissions the user accepted), [`disable`](PluginHost::disable), [`uninstall`](PluginHost::uninstall), [`install_from`](PluginHost::install_from) (folder or `.ferroplug`), [`reload_all`](PluginHost::reload_all), [`set_setting`](PluginHost::set_setting) |
//! | Network | [`PluginHost::check_request`] |
//! | Pages | [`PluginHost::content_scripts`], [`page_message`](PluginHost::page_message) |
//! | Tabs | [`PluginHost::set_active_tab`], [`notify_navigation`](PluginHost::notify_navigation) |
//! | Palette | [`PluginHost::commands`], [`run_command`](PluginHost::run_command) |
//! | Omnibox | [`PluginHost::omnibox`] |
//! | MCP / AI | [`PluginHost::mcp_tools`], [`tool_handler`](PluginHost::tool_handler) ([`PluginToolHandler`]) |
//! | UI thread work | [`PluginHost::drain_host_requests`] returning [`HostRequest`]s |
//!
//! # Integration checklist
//!
//! 1. **Startup**: `PluginHost::open(<config>/plugins, <data dir>, wake)`; the
//!    `wake` closure must wake the winit event loop (it is called from plugin
//!    threads). Keep it in the app state next to the extension manager.
//! 2. **Event loop**: on the wake event (and once per frame), call
//!    [`PluginHost::drain_host_requests`] and perform each [`HostRequest`]:
//!    `Toast` (egui toast), `OpenUrl` (new tab, `background` flag),
//!    `InjectJs` (evaluate in that tab), `Notify` (system notification),
//!    `ClipboardWrite`, `PluginDisabled` (toast + refresh the Plugins page),
//!    `SettingChanged` (refresh the page).
//! 3. **Network interception**: in the request callback that today asks the
//!    blocker, call [`PluginHost::check_request`] with the same
//!    `ferrosurf_extensions::RequestInfo` (convert with
//!    `Decision::from`); apply `Block` / `Redirect`. Cheap when no plugin
//!    listens.
//! 4. **Tabs**: call [`PluginHost::set_active_tab`] whenever the active tab,
//!    its URL or title changes; call [`PluginHost::notify_navigation`] with
//!    phase `"commit"` and `"complete"`.
//! 5. **Content scripts**: register [`PluginHost::content_scripts`] as user
//!    scripts next to the WebExtension ones, and refresh them after every
//!    enable / disable / reload / install.
//! 6. **Page bridge**: define `window.__ferrosurf.call(target, payload)` in
//!    pages; for `target == "plugin:<id>"` post `{origin, tab_id, payload}` to
//!    the UI thread, call [`PluginHost::page_message`] and resolve the
//!    page's promise when the receiver answers (poll it per frame; never
//!    block).
//! 7. **Palette**: list [`PluginHost::commands`] in the command palette (and
//!    bind `shortcut`s); run with [`PluginHost::run_command`] passing the
//!    active tab.
//! 8. **Omnibox**: when the input starts with `<keyword> `, call
//!    [`PluginHost::omnibox`] with the rest; poll the receiver and show its
//!    suggestions (`navigate` replaces the Enter target).
//! 9. **MCP**: merge [`PluginHost::tool_handler`] into the handler given to
//!    the MCP server and the assistant (its tool list is read live).
//! 10. **Settings UI**: a Plugins page backed by [`PluginHost::list`] with
//!     the permission prompt on enable ([`Permission::describe`] is Spanish).
//!
//! # Threading
//!
//! All plugin code runs on one thread (`ferrosurf-plugins`) that exists only
//! while a plugin is enabled. [`PluginHost::check_request`] is the only
//! call that waits on it (bounded by [`REQUEST_TIMEOUT`], and only when a
//! plugin's static `request_filters` matched); every other call returns
//! immediately or hands back a `Receiver`.

mod content;
mod host;
mod hostcalls;
mod http;
mod kv;
pub mod manifest;
pub mod package;
mod runtime;
pub mod state;
mod tools;
mod worker;

pub use ferrosurf_plugin_sdk as sdk;
pub use host::{
    BrokenPlugin, EngineStats, OMNIBOX_TIMEOUT, OmniboxResult, PAGE_MESSAGE_TIMEOUT, PluginCommand,
    PluginHost, PluginInfo, REQUEST_TIMEOUT, RequestDecision, SettingInfo, TOOL_TIMEOUT,
    resource_type_name,
};
pub use hostcalls::HostRequest;
pub use manifest::{Manifest, Permission};
pub use runtime::{MAX_MEMORY_BYTES, REQUEST_FUEL};
pub use tools::PluginToolHandler;
