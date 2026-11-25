//! Filesystem layout.
//!
//! ```text
//! <data root>/
//!   profiles.json            profile registry
//!   profiles/<id>/
//!     settings.json
//!     bookmarks.json
//!     history.sqlite
//!     session.json
//!     permissions.json
//!     extensions/<ext-id>/   installed extensions
//!     engine/                Servo's own storage (cookies, local storage, cache)
//! ```

use std::path::{Path, PathBuf};

/// Root data directory. Overridable with `FERROSURF_HOME` for portable installs and tests.
pub fn data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("FERROSURF_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("org", "FerroApps", "FerroSurf")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".ferrosurf"))
}

/// Default download directory for the current user.
pub fn default_download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::temp_dir().join("FerroSurf Downloads"))
}

/// Paths of a single profile.
#[derive(Clone, Debug)]
pub struct ProfilePaths {
    pub root: PathBuf,
}

impl ProfilePaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn bookmarks(&self) -> PathBuf {
        self.root.join("bookmarks.json")
    }
    pub fn history(&self) -> PathBuf {
        self.root.join("history.sqlite")
    }
    pub fn session(&self) -> PathBuf {
        self.root.join("session.json")
    }
    pub fn permissions(&self) -> PathBuf {
        self.root.join("permissions.json")
    }
    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads.json")
    }
    pub fn extensions(&self) -> PathBuf {
        self.root.join("extensions")
    }
    pub fn engine(&self) -> PathBuf {
        self.root.join("engine")
    }
    pub fn mcp_token(&self) -> PathBuf {
        self.root.join("mcp-endpoint.json")
    }
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.extensions())?;
        std::fs::create_dir_all(self.engine())
    }
}
