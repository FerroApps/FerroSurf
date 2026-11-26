# CLAUDE.md

## Overview
FerroSurf is a web browser in Rust (edition 2024) built on the Servo engine (`servo` 0.8 from
crates.io). The chrome is winit + egui 0.36 (egui_glow); Servo renders each tab into an
`OffscreenRenderingContext` that egui composites through a GL paint callback. Native MCP:
the browser is an MCP server (tools to drive tabs/pages) and an MCP client for the built-in
AI assistant.

## Commands
- `cargo run -p ferrosurf -- [URL]` — run (first build compiles Servo + SpiderMonkey: ~1 h).
- `cargo run -p ferrosurf -- --screenshot out.png --window-size 1280x800 <url>` — render to PNG
  (headless: `xvfb-run -a -s "-screen 0 1400x900x24" ...`; needs `libxkbcommon-x11-0`, `libegl1`).
- `--data-dir <dir>` (or `FERROSURF_HOME`) for a throw-away profile root; `--load-extension <dir>`;
  `--mcp-stdio` bridges stdio to the running browser's MCP endpoint.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`.
  Library crates alone (no Servo build): `-p ferrosurf-core -p ferrosurf-extensions -p ferrosurf-mcp`.

## Architecture
- `crates/ferrosurf-core` — profiles, settings, bookmarks, history (SQLite), session, downloads,
  permissions, search engines, omnibox, importers. No engine dependency.
- `crates/ferrosurf-extensions` — WebExtension subset (content scripts as Servo user scripts,
  DNR rules) and the ad/tracker blocker.
- `crates/ferrosurf-plugins` + `ferrosurf-plugin-sdk` — native WASM plugins (wasmi, permissions,
  zero cost when disabled); examples in `examples/plugins` (own workspace, `build.sh`).
- `crates/ferrosurf-passwords` — Bitwarden CLI client, WebAuthn polyfill + passkey authenticator.
- `crates/ferrosurf-mcp` — JSON-RPC, MCP HTTP server + stdio bridge, MCP client, browser tools,
  assistant loop (Anthropic / OpenAI-compatible).
- `crates/ferrosurf` — the app: `app.rs` (Servo, windows, engine events, MCP commands),
  `window.rs` (tabs, input, GL composition), `delegate.rs` (Servo callbacks → queued events),
  `internal_pages.rs` (`ferro://`, `ferro-extension://`), `ui/` (egui chrome).
- Page ↔ browser calls go through `bridge.rs` (`prompt()` with a magic prefix): treat every call
  as untrusted (origin comes from the tab URL; sensitive actions need user consent).
- Servo is single-threaded (`Rc`); MCP/assistant/downloads run on tokio and talk to the UI
  thread through channels + `EventLoopProxy` wakes.

## Conventions
- English for code, comments and commits (Conventional Commits: `feat:`, `fix:`, `chore:`,
  `docs:`, `test:`). User-facing UI copy is Spanish.
- Settings structs use `#[serde(default)]` so old/new settings files always load.
- UI code never blocks; mutate settings then `mark_settings_dirty()` and push
  `Cmd::App(AppCmd::SettingsChanged)`.

## Workflow quirks
- Work on feature branches off `development`; PRs target `development`.
- `version.yml` (helper `.github/scripts/version.sh`) bumps the `[workspace.package]` version
  (do not bump by hand) and merges `main` back into `development` after each main bump.

## Deployment
Releases are automatic: `.github/workflows/release.yml` chains off "Version" via `workflow_run`,
builds on ubuntu-22.04 and windows-latest, and creates GitHub Release `v<version>` with
`ferrosurf-v<ver>-linux-x86_64.tar.gz` and `ferrosurf-v<ver>-windows-x86_64.zip` (+ `.sha256`).
Linux users run `install.sh` (`curl ... | sh`; `-s -- --uninstall`); test it with
`FERRO_RELEASE_API=file://...`.

## Gotchas
- Omnibox: while unfocused it shows `omnibox::display_url` (no `https://`), while focused the full
  URL. The click that focuses it is laid out on the *displayed* text, so egui's caret/char indices
  from that frame must go through `display_to_edit_index`; never compute a selection length from
  the frame's buffer (v0.3.0 selected only 28 of 36 chars). A plain focusing click selects all on
  release (`focus_click_selection`), a drag keeps its range, a click while focused places the caret.
- Servo 0.8 ships several web APIs behind prefs that default OFF (`servo-config` `Preferences`:
  IntersectionObserver, IndexedDB, Web Animations, container queries, …). Scripts that call a missing
  constructor throw and leave blank pages (DuckDuckGo Images' grid). `settings.experimental_web_features`
  (default on, applied in `app.rs` when Servo is built) turns on IntersectionObserver; check the
  prefs before blaming the blocker. `blocker::tests::builtin_list_allows_duckduckgo_images` guards
  the blocker side; never fake another browser's UA to work around engine gaps.
