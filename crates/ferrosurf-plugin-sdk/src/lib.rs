//! SDK for FerroSurf native plugins.
//!
//! A plugin is a WebAssembly module (`wasm32-unknown-unknown`) plus a
//! `plugin.toml` manifest. With this crate a plugin is a few lines:
//!
//! ```ignore
//! use ferrosurf_plugin_sdk::prelude::*;
//!
//! #[derive(Default)]
//! struct Blocker;
//!
//! impl Plugin for Blocker {
//!     fn request(&mut self, req: RequestInput) -> RequestOutput {
//!         if req.url.contains("tracker.example") {
//!             RequestOutput::block()
//!         } else {
//!             RequestOutput::allow()
//!         }
//!     }
//! }
//!
//! register_plugin!(Blocker);
//! ```
//!
//! The [`abi`] module is the language-neutral contract (documented in
//! `docs/plugins.md`); any language that compiles to core WebAssembly can
//! implement it directly.

pub mod abi;

/// Re-exported for `Vec<u8>` fields encoded as MessagePack `bin`.
pub use serde_bytes;

#[cfg(feature = "guest")]
pub mod guest;
#[cfg(feature = "guest")]
pub mod host;

#[cfg(feature = "guest")]
pub mod prelude {
    pub use crate::abi::{
        ActiveTab, CommandInput, HttpRequest, HttpResponse, LoadInfo, McpToolInput, McpToolOutput,
        NavigationEvent, OmniboxInput, OmniboxOutput, PageMessageInput, RequestAction,
        RequestInput, RequestOutput, SettingValue, Suggestion,
    };
    pub use crate::guest::Plugin;
    pub use crate::host::{self, HostError};
    pub use crate::register_plugin;
}
