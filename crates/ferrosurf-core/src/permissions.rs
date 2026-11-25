//! Per-origin site permissions.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionKind {
    Geolocation,
    Notifications,
    Camera,
    Microphone,
    Clipboard,
    Midi,
    Popups,
    Autoplay,
    Downloads,
    Other(String),
}

impl PermissionKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Geolocation => "geolocation",
            Self::Notifications => "notifications",
            Self::Camera => "camera",
            Self::Microphone => "microphone",
            Self::Clipboard => "clipboard",
            Self::Midi => "midi",
            Self::Popups => "popups",
            Self::Autoplay => "autoplay",
            Self::Downloads => "downloads",
            Self::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "geolocation" => Self::Geolocation,
            "notifications" => Self::Notifications,
            "camera" => Self::Camera,
            "microphone" => Self::Microphone,
            "clipboard" => Self::Clipboard,
            "midi" => Self::Midi,
            "popups" => Self::Popups,
            "autoplay" => Self::Autoplay,
            "downloads" => Self::Downloads,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for PermissionKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PermissionKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    #[default]
    Ask,
}

type Map = BTreeMap<String, BTreeMap<PermissionKind, Decision>>;

#[derive(Debug, Default)]
pub struct PermissionStore {
    path: Option<PathBuf>,
    map: Map,
}

impl PermissionStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let map = crate::storage::read_json_or_default::<Map>(&path);
        Self { path: Some(path), map }
    }

    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Stored decision, or `Ask` when none (the UI decides defaults such as popups).
    pub fn get(&self, origin: &str, kind: &PermissionKind) -> Decision {
        self.map
            .get(origin)
            .and_then(|m| m.get(kind))
            .copied()
            .unwrap_or(Decision::Ask)
    }

    pub fn set(&mut self, origin: &str, kind: PermissionKind, decision: Decision) {
        if decision == Decision::Ask {
            if let Some(m) = self.map.get_mut(origin) {
                m.remove(&kind);
                if m.is_empty() {
                    self.map.remove(origin);
                }
            }
        } else {
            self.map.entry(origin.to_string()).or_default().insert(kind, decision);
        }
        self.persist();
    }

    pub fn clear_origin(&mut self, origin: &str) {
        if self.map.remove(origin).is_some() {
            self.persist();
        }
    }

    pub fn clear_all(&mut self) {
        self.map.clear();
        self.persist();
    }

    pub fn all(&self) -> Vec<(String, PermissionKind, Decision)> {
        self.map
            .iter()
            .flat_map(|(o, m)| m.iter().map(move |(k, d)| (o.clone(), k.clone(), *d)))
            .collect()
    }

    /// `scheme://host[:port]` for a URL (opaque origins map to the scheme only).
    pub fn origin_of(url: &Url) -> String {
        match url.host_str() {
            Some(h) => match url.port() {
                Some(p) => format!("{}://{}:{}", url.scheme(), h, p),
                None => format!("{}://{}", url.scheme(), h),
            },
            None => format!("{}://", url.scheme()),
        }
    }

    fn persist(&self) {
        if let Some(p) = &self.path
            && let Err(e) = crate::storage::write_json(p, &self.map)
        {
            log::error!("Failed to save permissions: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_persist() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perm.json");
        let mut s = PermissionStore::open(&p);
        let o = PermissionStore::origin_of(&Url::parse("https://example.com:8443/x?y").unwrap());
        assert_eq!(o, "https://example.com:8443");
        assert_eq!(s.get(&o, &PermissionKind::Camera), Decision::Ask);
        s.set(&o, PermissionKind::Camera, Decision::Allow);
        s.set(&o, PermissionKind::Other("foo".into()), Decision::Deny);
        let s2 = PermissionStore::open(&p);
        assert_eq!(s2.get(&o, &PermissionKind::Camera), Decision::Allow);
        assert_eq!(s2.get(&o, &PermissionKind::Other("foo".into())), Decision::Deny);
        assert_eq!(s2.all().len(), 2);
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"allow\"") && raw.contains("\"camera\""));
        let mut s2 = s2;
        s2.set(&o, PermissionKind::Camera, Decision::Ask);
        assert_eq!(s2.all().len(), 1);
        s2.clear_origin(&o);
        assert!(s2.all().is_empty());
    }

    #[test]
    fn origin_default_port_dropped() {
        let u = Url::parse("http://a.test:80/").unwrap();
        assert_eq!(PermissionStore::origin_of(&u), "http://a.test");
    }
}
