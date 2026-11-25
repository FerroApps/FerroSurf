# Plugins

FerroSurf tiene dos sistemas de extensión. Los **plugins nativos** son el
recomendado; las **extensiones WebExtension** siguen soportadas como
compatibilidad (legacy).

Contents: [Plugins nativos (recomendado)](#plugins-nativos-recomendado) ·
[Extensiones WebExtension (legacy)](#extensiones-webextension-legacy) ·
[1. Install](#1-install-and-manage) · [2. Manifest](#2-the-manifest-plugintoml) ·
[3. Permissions](#3-permission-model) · [4. Limits](#4-limits) ·
[5. Extension points](#5-extension-points) · [6. ABI](#6-the-abi-version-1) ·
[7. SDK guide](#7-rust-sdk-guide) · [8. Other languages](#8-other-languages) ·
[9. Cost](#9-cost-and-runtime-choice) · [10. Host internals](#10-host-internals) ·
[11. App integration](#11-app-integration)

## Plugins nativos (recomendado)

Un plugin nativo es un pequeño módulo **WebAssembly** (escrito en Rust u otro
lenguaje) más un archivo `plugin.toml`. No hay JavaScript en el camino
caliente: el plugin se ejecuta en un intérprete aislado con límites de CPU,
memoria y tiempo, y solo puede hacer lo que declara y tú le concedes.

Ventajas frente a las extensiones de Chrome y Firefox:

- **Más eficientes.** Sin plugins activados el coste es cero: no hay motor ni
  hilo. Un plugin cuesta unos pocos MiB y ~3 ms de carga; no hay un proceso de
  fondo por extensión ni una página de fondo con su propio motor de JS.
- **Bloqueo y redirección de peticiones sin trabajo inútil.** El plugin declara
  filtros estáticos (`request_filters`) y solo se le llama para las URL que
  coinciden, con un presupuesto minúsculo y un tiempo máximo (si se pasa, la
  petición se permite).
- **Permisos explícitos y desactivado por defecto.** Al activar un plugin ves
  todo lo que puede hacer, incluidos los servidores a los que puede conectarse.
  Si una actualización pide más permisos, se desactiva hasta que lo apruebes.
- **Nativos de IA.** Un plugin puede ofrecer herramientas (`[[tools]]`) que el
  asistente de IA integrado y el servidor MCP de FerroSurf ven como cualquier
  otra herramienta del navegador.
- **Integrados.** Comandos de la paleta, palabras clave de la barra de
  direcciones, scripts de contenido, ajustes y almacén privado, todo declarativo.

Cómo usarlos: copia la carpeta del plugin (o un archivo `.ferroplug`) a la
carpeta de plugins, o usa **Ajustes → Plugins → Instalar**. Pulsa **Activar**,
revisa los permisos y acepta. **Recargar plugins** reinicia los activos desde
disco (útil al desarrollar). Hay ejemplos en `examples/plugins/`
(`privacy-redirects`, `quick-notes`, `gh-omnibox`).

> Los plugins son código de terceros. FerroSurf los aísla y los limita a los
> permisos concedidos, pero instala solo plugins en los que confíes.

## Extensiones WebExtension (legacy)

FerroSurf conserva un subconjunto de WebExtensions (scripts de contenido
inyectados como user scripts, reglas `declarativeNetRequest` y un bloqueador
de anuncios y rastreadores) para poder usar extensiones existentes. Es una
capa de compatibilidad: no hay página de fondo ni la API completa, y no se
amplía con funciones nuevas. Para funciones nuevas usa un plugin nativo.
Ambos sistemas conviven: un plugin puede declarar `[[content]]` (scripts de
contenido) y `request_filters` igual que una extensión, pero con el modelo de
permisos y los límites de abajo.

| | Plugin nativo | WebExtension (legacy) |
|---|---|---|
| Código | WebAssembly (Rust, C, Zig, ...) | JavaScript |
| Coste sin usar | cero (sin motor ni hilo) | cero |
| Peticiones | hook `request` con filtros estáticos y presupuesto de combustible | reglas DNR declarativas |
| Herramientas de IA / MCP | sí (`[[tools]]`) | no |
| Permisos | explícitos, parciales, por plugin | los del manifest |
| Instalación | carpeta o `.ferroplug` | carpeta o `.zip` / `.xpi` |

---

*The rest of this document is the technical reference (English).*

Native plugins mirror Ferrocord's: a core-wasm module plus a manifest, MessagePack
payloads, a two-function host ABI, permissions, fuel/memory/time limits and a
private key-value store. FerroSurf-specific parts are the browser extension
points (§5).

## 1. Install and manage

A plugin is either a **folder** or a **`.ferroplug` file** (a zip) holding
`plugin.toml`, `plugin.wasm` and, for `[[content]]`, the referenced `.js` /
`.css` files. Plugins live in `<config dir>/plugins/`; the app passes the
folder to `PluginHost::open`. `install_from(path)` validates a folder or
`.ferroplug` (sizes, paths, manifest, module header; no `..` or absolute
entries) and copies it to `plugins/<id>/`, replacing an older version.

Every plugin is **off by default**. `enable(id, grant)` records the
permissions the user accepted. The choice, the granted permissions and the
plugin's settings are saved in `plugins/state.json`. If a new version asks for
more permissions or hosts than were approved, it stays off
(`needs_reapproval`) until enabled again. `reload_all()` rescans the folder
and restarts every enabled plugin from disk. `uninstall(id)` removes files,
state and the private store.

The private store (`storage` permission) lives in `<data dir>/plugins.sqlite3`.

## 2. The manifest (`plugin.toml`)

```toml
id = "my-plugin"                 # a-z 0-9 . _ -, at most 64; unique
name = "My Plugin"
version = "1.0.0"
author = "Me"
description = "What it does."
api_version = 1                  # the ABI this plugin was built for
permissions = ["requests", "tabs", "mcp_tools", "page_messages",
               "page_scripting", "network", "storage", "clipboard",
               "notifications"]
network_hosts = ["api.example.com"]   # with "network" only, bare lowercase domains
events = ["navigation"]               # needs "tabs"
request_filters = ["host:twitter.com", "/watch?v="]   # with "requests" only
omnibox_keyword = "gh"                # a-z 0-9 -, at most 16
page_message_origins = ["https://*.example.com/*"]    # with "page_messages" only

[[settings]]                     # rendered by the host on the Plugins page
key = "host"                     # a-z 0-9 _
type = "string"                  # bool | int | string | choice
label = "Host"
description = "..."
default = "nitter.net"
max_len = 100                    # string only (default 200, max 4000)
# min = 1, max = 10              # int only
# choices = ["a", "b"]           # choice only (1-16)

[[commands]]                     # palette commands (at most 16)
id = "save"
title = "Guardar nota de esta página"
shortcut = "Ctrl+Alt+N"          # optional

[[tools]]                        # exposed to MCP / the AI assistant (at most 16)
name = "list_notes"              # a-z 0-9 _
description = "Lists notes"
input_schema = '{"type":"object","properties":{}}'   # JSON object string
read_only = true                 # optional MCP hint

[[content]]                      # static content scripts (at most 8); needs "page_scripting"
matches = ["https://*.example.com/*"]    # WebExtension match patterns
js = "inject.js"                 # relative to the plugin folder
css = "inject.css"               # js and/or css
run_at = "document_end"          # document_start | document_end | document_idle
all_frames = false
```

Unknown keys are errors. Declarations and permissions go together:
`network` <-> `network_hosts`, `requests` <-> `request_filters`,
`mcp_tools` <-> `[[tools]]`, `page_messages` <-> `page_message_origins`;
`[[content]]` needs `page_scripting`; `events` needs `tabs`.

**Request filters** decide, without running plugin code, whether the
`request` hook is called: `*` (everything; use sparingly), `host:<domain>`
(the host or any subdomain) or any other string = case-insensitive substring
of the full URL (3-128 characters).

MCP tool names are exposed as `plugin_<id>__<tool>` (`.` and `-` in the id
become `_`), e.g. `plugin_quick_notes__list_notes`.

## 3. Permission model

The sandbox is the WebAssembly module: a plugin can only call the two
functions the host imports (`host_call`, `host_take`). No file system, no
clock, no sockets, no threads, no access to the browser's memory, cookies,
passwords or history. Each host operation then checks a permission and the
**hook context** it runs in:

| Permission | Allows | Restriction |
|---|---|---|
| — | `log`, `toast`, `get_setting` / `set_setting` (schema-checked), `now_ms` | one toast per hook call; none of them in `request` except log / get_setting / now_ms |
| `requests` | the `request` hook | only for URLs matching `request_filters`; redirects must be `http(s)` and different from the request |
| `tabs` | `navigation` events, `get_active_tab`, `open_url` | `open_url`: palette `command` only, `http(s)`, one per call |
| `mcp_tools` | `[[tools]]` served through MCP and the assistant | results are untrusted text to the model |
| `page_messages` | `page_message` hook | only pages whose origin matches `page_message_origins` |
| `page_scripting` | `[[content]]` scripts, `inject_js` | `inject_js`: during `command` / `navigation` only, for that hook's tab, ≤ 64 KiB, ≤ 4 per call |
| `network` | `http_fetch` | HTTPS `GET`/`POST` to `network_hosts` only, default port, no redirects, no cookies; checked in the plugin thread and again before the request leaves |
| `storage` | `kv_get` / `kv_set` / `kv_delete` | own namespace, quotas (§4); not in `request` |
| `clipboard` | `clipboard_write` | palette `command` only |
| `notifications` | `notify` | one per hook call |

Ops that need a user action fail with `NOT_USER_ACTION` outside a `command`
hook; a missing permission (or an op not allowed in the current hook) returns
`DENIED`. Permissions can be granted partially: the plugin keeps running and
the denied calls return `DENIED`.

**Faults.** Every call runs under `catch_unwind` with a fuel budget and a
memory cap. A trap (a Rust panic is an `unreachable` trap), running out of
fuel or broken ABI use is a *fault*: the call's requests are dropped, the
instance restarts from scratch (settings kept), and after three faults the
plugin is turned off (`HostRequest::PluginDisabled`, reason on the Plugins
page, persisted). A request hook that faults or times out always allows the
request. Nothing a plugin does can crash or freeze the browser.

## 4. Limits

| What | Limit |
|---|---|
| `plugin.wasm` | 8 MiB; `.ferroplug` 16 MiB; `plugin.toml` 64 KiB; each `.js` / `.css` 512 KiB |
| Linear memory | 16 MiB (one memory) |
| Fuel (≈ wasm instructions) | load / setting 50 M · **request 2 M** · navigation 20 M · omnibox 20 M · page message 20 M · command / mcp_tool / http answer 100 M |
| `request` | overall 30 ms across plugins, then allow |
| `omnibox` / `page_message` / `mcp_tool` | 400 ms / 250 ms / 10 s |
| Host calls per hook | 256; input ≤ 256 KiB, result ≤ 256 KiB |
| Navigation queue | 256 events; the oldest is dropped when full |
| Key-value store | key ≤ 128 B, value ≤ 64 KiB, 1 MiB and 1024 keys per plugin |
| HTTP | 4 in flight per plugin; request body 64 KiB, 16 headers; response 1 MiB; 10 s timeout |
| Omnibox | ≤ 8 suggestions, non-`http(s)` URLs dropped |
| Page message | payload and reply ≤ 64 KiB (JSON) |
| Faults | 3, then the plugin is turned off |

## 5. Extension points

1. **Lifecycle** — `load` (with current settings), `unload`, `setting_changed`.
2. **`request`** (`requests`) — input `{url, resource_type, initiator, main_frame}`,
   output `{action: "allow"|"block"|"redirect", redirect_url?}`. Called
   synchronously per network request, but only for requests matching the
   manifest `request_filters`. With nothing listening, the browser pays one
   atomic load. `resource_type`: `main_frame`, `sub_frame`, `script`,
   `stylesheet`, `image`, `font`, `xhr`, `media`, `websocket`, `ping`, `other`.
   Plugins run in id order; the first non-allow decision wins.
3. **`navigation`** (`tabs` + `events = ["navigation"]`) — `{tab_id, url, title, phase}`,
   `phase` is `commit` or `complete`. Asynchronous.
4. **`command`** — palette commands from `[[commands]]`: `{command, tab_id, url}`.
   A user action: it may `open_url`, `clipboard_write` and `inject_js`.
5. **`omnibox`** — `omnibox_keyword = "gh"`: `{text}` (what follows the
   keyword) -> `{suggestions: [{title, url}], navigate?: url}`.
6. **`mcp_tool`** (`mcp_tools`) — `{name, args_json}` -> `{content_text, is_error}`.
   The tools appear in the MCP server's `tools/list` and in the AI
   assistant's tools, next to the browser tools.
7. **`page_message`** (`page_messages`) — a page script calls
   `window.__ferrosurf.call("plugin:<id>", payload)`; the plugin gets
   `{origin, tab_id, payload_json}` and answers `{reply_json}`. Bounded time.
8. **Page injection** — static `[[content]]` assets (injected by the browser as
   user scripts, with match-pattern guards and `run_at`) and the host op
   `inject_js {tab_id, code}`. Content scripts get a `ferrosurf` binding with
   `ferrosurf.call(payload)` (a promise for the plugin's `page_message` reply).
9. **`http_response`** — answer to an earlier `http_fetch` (asynchronous).

## 6. The ABI (version 1)

Core WebAssembly (`wasm32`), no WASI, no component model. All payloads are
**MessagePack**, structs encoded as **maps keyed by field name**, so fields can
be added later without breaking older plugins (unknown keys are ignored,
missing ones default). Enums are lowercase strings; setting values are an
untagged bool / integer / string; byte fields are `bin`. The Rust definitions
in `crates/ferrosurf-plugin-sdk/src/abi.rs` are the reference.

### Guest exports

| Export | Signature | Meaning |
|---|---|---|
| `memory` | memory | the only linear memory |
| `fs_abi_version` | `() -> i32` | must return `1` |
| `fs_alloc` | `(len: i32) -> i32` | returns a buffer of `len` bytes; the host writes a hook input into it and passes it to `fs_call`, which owns it from then on |
| `fs_call` | `(hook: i32, ptr: i32, len: i32) -> i64` | runs a hook. Returns `0` (no output), `-1` (input not understood; not a fault) or `(out_ptr << 32) \| out_len` pointing at the MessagePack output, valid until the next call |

### Hooks (`fs_call` first argument)

| Id | Hook | Input | Output |
|---|---|---|---|
| 1 | load | `LoadInfo {api_version, plugin_id, settings}` | — |
| 2 | unload | empty | — |
| 3 | setting_changed | `SettingChanged {key, value}` | — |
| 4 | http_response | `HttpResponse {request_id, status, body, error}` | — |
| 5 | request | `RequestInput {url, resource_type, initiator, main_frame}` | `RequestOutput {action, redirect_url}` |
| 6 | navigation | `NavigationEvent {tab_id, url, title, phase}` | — |
| 7 | command | `CommandInput {command, tab_id, url}` | — |
| 8 | omnibox | `OmniboxInput {text}` | `OmniboxOutput {suggestions: [{title, url}], navigate}` |
| 9 | mcp_tool | `McpToolInput {name, args_json}` | `McpToolOutput {content_text, is_error}` |
| 10 | page_message | `PageMessageInput {origin, tab_id, payload_json}` | `PageMessageOutput {reply_json}` |

### Host imports (module `ferrosurf`)

| Import | Signature | Meaning |
|---|---|---|
| `host_call` | `(op: i32, ptr: i32, len: i32) -> i32` | runs operation `op` on the MessagePack input at `ptr`/`len`. `>= 0`: length of the result, which the host keeps; `< 0`: an error code |
| `host_take` | `(ptr: i32, len: i32) -> i32` | copies the pending result (at most `len` bytes) to `ptr`, returns the bytes copied |

| Op | Name | Input | Result |
|---|---|---|---|
| 1 | log | `{level: 0-3, message}` | — |
| 2 | toast | `{text}` | — |
| 3 | get_setting | `{key}` | value or nil |
| 4 | set_setting | `{key, value}` | — |
| 5 | kv_get | `{key}` | bin or nil |
| 6 | kv_set | `{key, value: bin}` | — |
| 7 | kv_delete | `{key}` | — |
| 8 | http_fetch | `{method, url, headers: [[k, v]], body: bin}` | request id (u64); the answer arrives in the `http_response` hook |
| 9 | open_url | `{url, background}` | — |
| 10 | inject_js | `{tab_id, code}` | — |
| 11 | clipboard_write | `{text}` | — |
| 12 | now_ms | nil | u64 |
| 13 | notify | `{title, body}` | — |
| 14 | get_active_tab | nil | `{tab_id, url, title}` or nil |

| Code | Name | Meaning |
|---|---|---|
| -1 | DENIED | the permission is missing (or the op is not allowed in this hook) |
| -2 | BAD_INPUT | did not decode or failed validation |
| -3 | NOT_USER_ACTION | needs a palette command |
| -4 | LIMIT | size, quota or budget exceeded |
| -5 | UNKNOWN_OP | |
| -6 | HOST_NOT_ALLOWED | URL not HTTPS to a listed host |
| -7 | UNAVAILABLE | storage failure |

**Versioning.** `api_version` and `fs_abi_version` must both match the host's.
Additive changes (new hooks, ops, fields) keep version 1: plugins ignore what
they do not know and the host never calls a hook a plugin did not declare,
except the lifecycle ones.

## 7. Rust SDK guide

`crates/ferrosurf-plugin-sdk` wraps the ABI: implement the hooks you need on
a `Default` type and register it.

```toml
# Cargo.toml
[package]
name = "block-ads"
version = "1.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
ferrosurf-plugin-sdk = { git = "https://github.com/FerroApps/FerroSurf" }

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

```rust
use ferrosurf_plugin_sdk::prelude::*;

#[derive(Default)]
struct BlockAds;

impl Plugin for BlockAds {
    fn request(&mut self, req: RequestInput) -> RequestOutput {
        if req.url.contains("/ads/") {
            RequestOutput::block()
        } else {
            RequestOutput::allow()
        }
    }
}

register_plugin!(BlockAds);
```

```toml
# plugin.toml
id = "block-ads"
name = "Block Ads"
version = "1.0.0"
api_version = 1
permissions = ["requests"]
request_filters = ["/ads/"]
```

Build and install:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
mkdir -p block-ads && cp plugin.toml block-ads/
cp target/wasm32-unknown-unknown/release/block_ads.wasm block-ads/plugin.wasm
# copy the folder into the plugins folder, or zip both files as block-ads.ferroplug
```

Host calls are in `ferrosurf_plugin_sdk::host` (`toast`, `setting`,
`setting_str`, `setting_bool`, `set_setting`, `kv_get` / `kv_set` /
`kv_delete`, `http_fetch`, `open_url`, `inject_js`, `clipboard_write`,
`now_ms`, `notify`, `get_active_tab`, `log`). Returns map the negative codes to
`HostError`. A panic is logged and then traps; the host restarts the plugin.

**Testing.** On a native target the host calls go to a recorder, so plugin
logic runs under plain `cargo test`: `host::testing::reply(..)` queues an
answer, `host::testing::take_calls()` returns what was called.

**Examples** (`examples/plugins/`, a separate workspace; `build.sh` builds
them into `dist/<id>/`):

| Plugin | Shows |
|---|---|
| `privacy-redirects` | `request` hook with `request_filters`; bool and string settings; Twitter/X -> Nitter-like host, YouTube -> Piped/Invidious host, all off by default |
| `quick-notes` | palette command + `get_active_tab` + KV store + an `mcp_tool` (`list_notes`) the AI assistant can call |
| `gh-omnibox` | `omnibox_keyword = "gh"` with no permissions |

## 8. Other languages

Anything that compiles to core wasm and can export `fs_abi_version`,
`fs_alloc`, `fs_call` and `memory` works (C/C++ with clang, Zig, TinyGo,
AssemblyScript, ...). You need a MessagePack library, a bump or real allocator
behind `fs_alloc`, and the two imports. `crates/ferrosurf-plugins/tests/plugins.rs`
builds minimal modules straight from WebAssembly text, a compact reference
for the calling convention.

## 9. Cost and runtime choice

**Zero plugins enabled costs nothing.** At startup the app reads
`plugins/state.json`; when no plugin is enabled there is no wasm engine, no
plugin thread, no folder scan and no subscription
(`PluginHost::engine_stats()`, asserted in tests). Enabling the first plugin
starts one thread and the engine; disabling the last stops both.
`check_request` with no request plugin is one atomic load, and with request
plugins the engine is only consulted for URLs matching a plugin's static
filters.

Same runtime choice as Ferrocord: **wasmi 2.0** (interpreter, lazy translation,
deterministic fuel) rather than a JIT. Measured there: ~3 ms and ~2 MiB per
plugin vs ~420 ms and ~17 MiB for wasmtime, with hook latencies of tens of
microseconds, invisible next to a network request or a keystroke.

## 10. Host internals

| Module (`crates/ferrosurf-plugins/src`) | Role |
|---|---|
| `manifest.rs` | manifest parsing and validation, permissions, settings schema, request filters |
| `package.rs` | folder / `.ferroplug` discovery, install, size caps |
| `state.rs` | `state.json`: enabled, asked / granted permissions, setting values |
| `runtime.rs` | wasmi engine, store limits, fuel budgets, the two imports |
| `hostcalls.rs` | every host operation: permission, hook-context and limit checks |
| `worker.rs` | the plugin thread: control / sync / action / event queues, faults |
| `kv.rs` | per-plugin SQLite store with quotas |
| `http.rs` | `http_fetch` execution (ureq, no redirects, no cookies) |
| `content.rs` | injected JS for `[[content]]` |
| `host.rs` | `PluginHost`, the UI-thread facade |
| `tools.rs` | `PluginToolHandler`, the MCP adapter |

## 11. App integration

`PluginHost` is `Clone + Send + Sync`; see the crate docs of
`ferrosurf-plugins` for the full checklist. In short: `open` at startup with a
`wake` that wakes the event loop; `drain_host_requests` on wake (toasts, open
URL, inject JS, notifications, clipboard, disabled plugins); `check_request` in
the network interception path next to the blocker; `set_active_tab` /
`notify_navigation` from tab events; `content_scripts` registered as user
scripts; `window.__ferrosurf.call("plugin:<id>", ..)` routed to `page_message`;
`commands` in the palette; `omnibox` for keywords; `tool_handler` merged into
the MCP server and the assistant.
