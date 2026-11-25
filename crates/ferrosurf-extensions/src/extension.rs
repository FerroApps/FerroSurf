//! A single installed extension.

use crate::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// Copied from a directory.
    Directory,
    /// Referenced in place (developer mode).
    Unpacked,
    /// Extracted from a .zip / .xpi / .crx archive.
    Archive,
}

#[derive(Debug, Clone)]
pub struct Extension {
    pub id: String,
    pub manifest: Manifest,
    pub root: PathBuf,
    pub enabled: bool,
    pub installed_ms: i64,
    pub source: InstallSource,
}

/// Join `rel` onto `root` refusing anything that could escape it. Does not
/// touch the filesystem.
pub(crate) fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() || rel.contains(['\\', '\0']) {
        return None;
    }
    let mut out = root.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (out != root).then_some(out)
}

impl Extension {
    /// Resolve a path relative to the extension root, rejecting traversal
    /// (lexically and via symlinks). Returns the canonical path of an
    /// existing file.
    pub fn resolve(&self, rel: &str) -> Option<PathBuf> {
        let joined = safe_join(&self.root, rel)?;
        let real = joined.canonicalize().ok()?;
        let root = self.root.canonicalize().ok()?;
        (real.starts_with(&root) && real.is_file()).then_some(real)
    }

    /// Best icon for `size`: the smallest one that is >= `size`, otherwise the largest.
    pub fn icon_path(&self, size: u32) -> Option<PathBuf> {
        let mut icons: Vec<(u32, &str)> = self
            .manifest
            .icons
            .iter()
            .filter_map(|(k, v)| Some((k.trim().parse().ok()?, v.as_str())))
            .collect();
        icons.sort_by_key(|(s, _)| *s);
        let pick = icons
            .iter()
            .find(|(s, _)| *s >= size)
            .or_else(|| icons.last())?;
        self.resolve(pick.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_traversal() {
        let r = Path::new("/ext/a");
        assert_eq!(safe_join(r, "x/y.js"), Some(PathBuf::from("/ext/a/x/y.js")));
        assert_eq!(safe_join(r, "/x.js"), Some(PathBuf::from("/ext/a/x.js")));
        assert_eq!(safe_join(r, "./x.js"), Some(PathBuf::from("/ext/a/x.js")));
        assert!(safe_join(r, "../x").is_none());
        assert!(safe_join(r, "a/../../x").is_none());
        assert!(safe_join(r, "a\\b").is_none());
        assert!(safe_join(r, "").is_none());
        assert!(safe_join(r, "/").is_none());
    }
}
