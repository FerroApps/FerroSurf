//! Multiple user profiles. Each profile is a fully isolated directory (settings, bookmarks,
//! history, extensions, cookies & site storage). FerroSurf runs one process per open
//! profile, which gives hard isolation of engine state for free.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{ProfilePaths, data_root};
use crate::storage::{read_json_or_default, write_json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// Accent color as `#rrggbb`; shown in the profile badge and used as UI accent.
    #[serde(default = "default_color")]
    pub color: String,
    /// A short emoji/initial avatar.
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub created_ms: i64,
    #[serde(default)]
    pub last_used_ms: i64,
    /// Ephemeral profiles are wiped when the browser exits (guest mode).
    #[serde(default)]
    pub ephemeral: bool,
}

fn default_color() -> String {
    "#ff6a3d".into()
}

pub const PROFILE_COLORS: &[&str] = &[
    "#ff6a3d", "#3d8bff", "#2fbf71", "#b05cff", "#ffb020", "#ff4d8d", "#20c4c4", "#8a8f98",
];

impl Profile {
    pub fn initial(&self) -> String {
        if !self.avatar.is_empty() {
            return self.avatar.clone();
        }
        self.name
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".into())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    profiles: Vec<Profile>,
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    ask_on_startup: bool,
}

/// Owns `profiles.json` and the per-profile directories.
pub struct ProfileManager {
    root: PathBuf,
    registry: Registry,
}

impl ProfileManager {
    /// Open the registry under the default data root, creating a "Personal" profile on
    /// first run.
    pub fn open_default() -> anyhow::Result<Self> {
        Self::open(data_root())
    }

    pub fn open(root: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        let registry: Registry = read_json_or_default(&root.join("profiles.json"));
        let mut manager = Self { root, registry };
        if manager.registry.profiles.is_empty() {
            let p = manager.create("Personal", None)?;
            manager.registry.default_profile = Some(p.id);
            manager.save()?;
        }
        Ok(manager)
    }

    fn save(&self) -> anyhow::Result<()> {
        write_json(&self.root.join("profiles.json"), &self.registry)
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.registry.profiles
    }

    pub fn ask_on_startup(&self) -> bool {
        self.registry.ask_on_startup
    }

    pub fn set_ask_on_startup(&mut self, ask: bool) -> anyhow::Result<()> {
        self.registry.ask_on_startup = ask;
        self.save()
    }

    pub fn get(&self, id_or_name: &str) -> Option<&Profile> {
        self.registry
            .profiles
            .iter()
            .find(|p| p.id == id_or_name)
            .or_else(|| {
                self.registry
                    .profiles
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(id_or_name))
            })
    }

    pub fn default_profile(&self) -> &Profile {
        self.registry
            .default_profile
            .as_deref()
            .and_then(|id| self.get(id))
            .or_else(|| self.registry.profiles.iter().max_by_key(|p| p.last_used_ms))
            .expect("registry always holds at least one profile")
    }

    pub fn set_default(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.get(id).is_some(), "unknown profile {id}");
        self.registry.default_profile = Some(id.to_string());
        self.save()
    }

    pub fn paths(&self, profile: &Profile) -> ProfilePaths {
        ProfilePaths::new(self.root.join("profiles").join(&profile.id))
    }

    pub fn create(&mut self, name: &str, color: Option<&str>) -> anyhow::Result<Profile> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "profile name cannot be empty");
        let color = color.map(str::to_string).unwrap_or_else(|| {
            PROFILE_COLORS[self.registry.profiles.len() % PROFILE_COLORS.len()].to_string()
        });
        let profile = Profile {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            name: name.to_string(),
            color,
            avatar: String::new(),
            created_ms: crate::now_ms(),
            last_used_ms: 0,
            ephemeral: false,
        };
        self.paths(&profile).ensure()?;
        self.registry.profiles.push(profile.clone());
        self.save()?;
        Ok(profile)
    }

    /// Create a throw-away guest profile. It is not persisted in the registry.
    pub fn create_guest(&self) -> anyhow::Result<Profile> {
        let profile = Profile {
            id: format!("guest-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            name: "Guest".into(),
            color: "#8a8f98".into(),
            avatar: "👤".into(),
            created_ms: crate::now_ms(),
            last_used_ms: crate::now_ms(),
            ephemeral: true,
        };
        self.paths(&profile).ensure()?;
        Ok(profile)
    }

    pub fn update(&mut self, profile: Profile) -> anyhow::Result<()> {
        let slot = self
            .registry
            .profiles
            .iter_mut()
            .find(|p| p.id == profile.id)
            .ok_or_else(|| anyhow::anyhow!("unknown profile"))?;
        *slot = profile;
        self.save()
    }

    pub fn touch(&mut self, id: &str) -> anyhow::Result<()> {
        if let Some(p) = self.registry.profiles.iter_mut().find(|p| p.id == id) {
            p.last_used_ms = crate::now_ms();
        }
        self.save()
    }

    /// Delete a profile and all its data. The last profile cannot be deleted.
    pub fn delete(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.registry.profiles.len() > 1, "cannot delete the last profile");
        let idx = self
            .registry
            .profiles
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown profile"))?;
        let profile = self.registry.profiles.remove(idx);
        if self.registry.default_profile.as_deref() == Some(id) {
            self.registry.default_profile = self.registry.profiles.first().map(|p| p.id.clone());
        }
        self.save()?;
        let dir = self.paths(&profile).root;
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let mut pm = ProfileManager::open(dir.path().to_path_buf()).unwrap();
        assert_eq!(pm.profiles().len(), 1);
        let work = pm.create("Work", Some("#3d8bff")).unwrap();
        assert!(pm.paths(&work).extensions().exists());
        assert_eq!(pm.get("work").unwrap().id, work.id);
        pm.set_default(&work.id).unwrap();
        drop(pm);
        let mut pm = ProfileManager::open(dir.path().to_path_buf()).unwrap();
        assert_eq!(pm.default_profile().name, "Work");
        pm.delete(&work.id).unwrap();
        assert_eq!(pm.profiles().len(), 1);
        assert!(pm.delete(&pm.profiles()[0].id.clone()).is_err());
        assert!(pm.create("  ", None).is_err());
    }
}
