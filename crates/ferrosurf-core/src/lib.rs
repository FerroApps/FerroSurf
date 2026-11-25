//! FerroSurf core: everything that makes a browser a browser, minus the engine.
//!
//! This crate is deliberately free of any rendering or engine dependency so it can be
//! unit-tested quickly and reused by tooling (e.g. the MCP bridge).

pub mod bookmarks;
pub mod downloads;
pub mod history;
pub mod import;
pub mod omnibox;
pub mod paths;
pub mod permissions;
pub mod profile;
pub mod search;
pub mod session;
pub mod settings;
pub mod storage;
pub mod util;

pub use bookmarks::{Bookmark, BookmarkId, BookmarkNode, BookmarkStore};
pub use history::{History, HistoryEntry};
pub use profile::{Profile, ProfileManager};
pub use settings::Settings;

/// Current UNIX timestamp in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
