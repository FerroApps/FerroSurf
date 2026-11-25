//! All per-profile state that lives on the UI thread.

use std::cell::RefCell;
use std::sync::Arc;

use ferrosurf_core::downloads::DownloadManager;
use ferrosurf_core::paths::ProfilePaths;
use ferrosurf_core::permissions::PermissionStore;
use ferrosurf_core::session::ClosedTabs;
use ferrosurf_core::{BookmarkStore, History, Profile, ProfileManager, Settings};
use ferrosurf_extensions::{Blocker, ExtensionManager};

pub struct ProfileData {
    pub profile: Profile,
    pub paths: ProfilePaths,
    pub settings: Settings,
    pub bookmarks: BookmarkStore,
    pub history: History,
    pub permissions: PermissionStore,
    pub downloads: DownloadManager,
    pub extensions: ExtensionManager,
    /// Shared with the network thread (content blocking happens on request interception,
    /// which runs on the UI thread, but we keep it in an Arc for cheap snapshots).
    pub blocker: Arc<parking_lot::RwLock<Blocker>>,
    pub closed_tabs: ClosedTabs,
    pub favicons: crate::favicons::FaviconStore,
    /// Extension packages being downloaded from a store: (label, result).
    pub store_results:
        std::sync::Arc<parking_lot::Mutex<Vec<(String, Result<std::path::PathBuf, String>)>>>,
    /// Labels of store installs in progress.
    pub store_busy: Vec<String>,
    /// Store pages whose install bar the user dismissed.
    pub store_dismissed: std::collections::HashSet<String>,
    pub profiles: RefCell<ProfileManager>,
    settings_dirty: bool,
}

impl ProfileData {
    pub fn load(
        profiles: ProfileManager,
        profile: Profile,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let paths = profiles.paths(&profile);
        paths.ensure()?;
        let settings = Settings::load(&paths.settings());
        let bookmarks = BookmarkStore::open(paths.bookmarks());
        let mut history = History::open(&paths.history())?;
        history.set_enabled(!profile.ephemeral && settings.privacy.remember_history);
        let permissions = PermissionStore::open(paths.permissions());
        let favicons =
            crate::favicons::FaviconStore::new(paths.root.join("favicons"), Some(wake.clone()));
        let downloads = DownloadManager::new(Some(paths.downloads()), wake);
        let extensions = ExtensionManager::open(paths.extensions())?;
        let mut data = Self {
            profile,
            paths,
            settings,
            bookmarks,
            history,
            permissions,
            downloads,
            extensions,
            blocker: Arc::new(parking_lot::RwLock::new(Blocker::new())),
            closed_tabs: ClosedTabs::default(),
            favicons,
            store_results: Default::default(),
            store_busy: Vec::new(),
            store_dismissed: Default::default(),
            profiles: RefCell::new(profiles),
            settings_dirty: false,
        };
        data.rebuild_blocker();
        Ok(data)
    }

    /// Rebuild the content blocker from settings + extension rules.
    pub fn rebuild_blocker(&mut self) {
        let mut blocker = if self.settings.privacy.block_trackers {
            Blocker::with_builtin_lists()
        } else {
            Blocker::new()
        };
        blocker.add_dnr_rules(self.extensions.dnr_rules());
        *self.blocker.write() = blocker;
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    /// Persist settings if they changed. Cheap to call every frame.
    pub fn flush_settings(&mut self) {
        if self.settings_dirty {
            self.settings_dirty = false;
            if let Err(e) = self.settings.save(&self.paths.settings()) {
                log::error!("Failed to save settings: {e}");
            }
            self.history
                .set_enabled(!self.profile.ephemeral && self.settings.privacy.remember_history);
        }
    }

    pub fn accent_hex(&self) -> String {
        if self.settings.appearance.accent.is_empty() {
            self.profile.color.clone()
        } else {
            self.settings.appearance.accent.clone()
        }
    }

    /// Called on shutdown.
    pub fn on_exit(&mut self) {
        self.flush_settings();
        if self.settings.privacy.clear_history_on_exit {
            let _ = self.history.clear();
        }
        if self.profile.ephemeral {
            let _ = std::fs::remove_dir_all(&self.paths.root);
        }
    }
}
