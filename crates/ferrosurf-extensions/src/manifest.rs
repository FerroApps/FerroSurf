//! Subset of the Chrome/Firefox `manifest.json` format (MV2 and MV3).

use anyhow::{Context, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

fn default_mv() -> u32 {
    2
}

/// Accepts a JSON array and keeps only its string elements (MV2 allows
/// objects inside `permissions`).
fn string_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    Ok(v.into_iter()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect())
}

fn string_map<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let v = Option::<BTreeMap<String, serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    Ok(v.into_iter()
        .filter_map(|(k, x)| x.as_str().map(|s| (k, s.to_owned())))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAt {
    DocumentStart,
    DocumentEnd,
    #[default]
    #[serde(other)]
    DocumentIdle,
}

impl RunAt {
    pub fn as_str(self) -> &'static str {
        match self {
            RunAt::DocumentStart => "document_start",
            RunAt::DocumentEnd => "document_end",
            RunAt::DocumentIdle => "document_idle",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentScriptDecl {
    #[serde(default, deserialize_with = "string_vec")]
    pub matches: Vec<String>,
    #[serde(default, deserialize_with = "string_vec")]
    pub exclude_matches: Vec<String>,
    #[serde(default, deserialize_with = "string_vec")]
    pub js: Vec<String>,
    #[serde(default, deserialize_with = "string_vec")]
    pub css: Vec<String>,
    #[serde(default)]
    pub run_at: RunAt,
    #[serde(default)]
    pub all_frames: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleResource {
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DnrDecl {
    #[serde(default)]
    pub rule_resources: Vec<RuleResource>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UrlOverrides {
    #[serde(default)]
    pub newtab: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserStyleDecl {
    #[serde(default, deserialize_with = "string_vec")]
    pub matches: Vec<String>,
    /// Either a path of a file inside the extension or inline CSS text.
    #[serde(default)]
    pub css: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FerroSurfExt {
    #[serde(default)]
    pub mcp_servers: Vec<serde_json::Value>,
    #[serde(default)]
    pub user_styles: Vec<UserStyleDecl>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeckoSettings {
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrowserSpecificSettings {
    #[serde(default)]
    pub gecko: Option<GeckoSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "default_mv")]
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "string_map")]
    pub icons: BTreeMap<String, String>,
    #[serde(default)]
    pub content_scripts: Vec<ContentScriptDecl>,
    #[serde(default, deserialize_with = "string_vec")]
    pub permissions: Vec<String>,
    #[serde(default, deserialize_with = "string_vec")]
    pub host_permissions: Vec<String>,
    #[serde(default)]
    pub declarative_net_request: Option<DnrDecl>,
    #[serde(default)]
    pub chrome_url_overrides: Option<UrlOverrides>,
    #[serde(default)]
    pub ferrosurf: Option<FerroSurfExt>,
    #[serde(default)]
    pub homepage_url: Option<String>,
    #[serde(default)]
    pub author: Option<serde_json::Value>,
    #[serde(default)]
    pub default_locale: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub browser_specific_settings: Option<BrowserSpecificSettings>,
    #[serde(default)]
    pub applications: Option<BrowserSpecificSettings>,
}

impl Manifest {
    /// Parse manifest JSON without i18n resolution.
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let text = text.trim_start_matches('\u{feff}');
        let m: Manifest = serde_json::from_str(text).context("invalid manifest.json")?;
        if m.name.trim().is_empty() {
            bail!("manifest has an empty name");
        }
        Ok(m)
    }

    /// Read `<root>/manifest.json` and resolve `__MSG_x__` placeholders.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join("manifest.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut m = Self::from_json(&text)?;
        let msgs = load_messages(root, m.default_locale.as_deref());
        if !msgs.is_empty() {
            m.name = resolve_messages(&m.name, &msgs);
            m.description = m.description.map(|d| resolve_messages(&d, &msgs));
            m.homepage_url = m.homepage_url.map(|d| resolve_messages(&d, &msgs));
        }
        Ok(m)
    }

    /// `browser_specific_settings.gecko.id` (or legacy `applications.gecko.id`).
    pub fn gecko_id(&self) -> Option<&str> {
        self.browser_specific_settings
            .as_ref()
            .or(self.applications.as_ref())
            .and_then(|b| b.gecko.as_ref())
            .and_then(|g| g.id.as_deref())
            .filter(|s| !s.is_empty())
    }
}

fn msg_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"__MSG_([A-Za-z0-9_@]+?)__").unwrap())
}

fn placeholder_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\$([A-Za-z0-9_@]+)\$").unwrap())
}

/// Replace `__MSG_name__` occurrences using the (lowercase-keyed) message map.
pub fn resolve_messages(s: &str, msgs: &BTreeMap<String, String>) -> String {
    msg_re()
        .replace_all(s, |c: &regex::Captures| {
            msgs.get(&c[1].to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| c[0].to_owned())
        })
        .into_owned()
}

/// Load `_locales/<locale>/messages.json` (default locale, then `en`, `en_US`,
/// then any locale). Keys are lowercased; named placeholders are expanded but
/// positional `$1` substitutions are left for the caller.
pub fn load_messages(root: &Path, default_locale: Option<&str>) -> BTreeMap<String, String> {
    let locales = root.join("_locales");
    let mut candidates: Vec<String> = Vec::new();
    if let Some(d) = default_locale {
        candidates.push(d.to_owned());
    }
    candidates.extend(["en".into(), "en_US".into(), "en-US".into()]);
    if let Ok(rd) = std::fs::read_dir(&locales) {
        let mut others: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        others.sort();
        candidates.extend(others);
    }
    for loc in candidates {
        if loc.contains(['/', '\\']) || loc.contains("..") {
            continue;
        }
        let p = locales.join(&loc).join("messages.json");
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
        else {
            continue;
        };
        let Some(obj) = v.as_object() else { continue };
        let mut out = BTreeMap::new();
        for (k, entry) in obj {
            let Some(msg) = entry.get("message").and_then(|m| m.as_str()) else {
                continue;
            };
            let phs: BTreeMap<String, String> = entry
                .get("placeholders")
                .and_then(|p| p.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(n, d)| {
                            Some((
                                n.to_ascii_lowercase(),
                                d.get("content")?.as_str()?.to_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let expanded = placeholder_re().replace_all(msg, |c: &regex::Captures| {
                phs.get(&c[1].to_ascii_lowercase())
                    .cloned()
                    .unwrap_or_else(|| c[0].to_owned())
            });
            out.insert(k.to_ascii_lowercase(), expanded.replace("$$", "$"));
        }
        return out;
    }
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mv2_sample() {
        let m = Manifest::from_json(
            r#"{
            "manifest_version": 2, "name": "Dark", "version": "1.2",
            "description": "d", "icons": {"16": "i16.png", "48": "i48.png"},
            "permissions": ["storage", "https://*/*", {"fileSystem": ["write"]}],
            "content_scripts": [{"matches": ["<all_urls>"], "js": ["a.js"], "css": ["a.css"],
                "run_at": "document_start", "all_frames": true, "unknown": 1}],
            "browser_specific_settings": {"gecko": {"id": "dark@example.org"}},
            "author": "me", "something_else": {"x": 1}
        }"#,
        )
        .unwrap();
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.permissions, vec!["storage", "https://*/*"]);
        assert_eq!(m.content_scripts[0].run_at, RunAt::DocumentStart);
        assert!(m.content_scripts[0].all_frames);
        assert_eq!(m.icons["48"], "i48.png");
        assert_eq!(m.gecko_id(), Some("dark@example.org"));
    }

    #[test]
    fn mv3_sample_and_defaults() {
        let m = Manifest::from_json(
            r#"{
            "manifest_version": 3, "name": "Blk", "version": "0.1",
            "host_permissions": ["<all_urls>"],
            "permissions": ["declarativeNetRequest"],
            "declarative_net_request": {"rule_resources": [
                {"id": "r1", "enabled": true, "path": "rules.json"}, {"id": "r2", "path": "x.json"}]},
            "chrome_url_overrides": {"newtab": "new.html"},
            "content_scripts": [{"matches": ["https://a.com/*"], "js": ["x.js"], "run_at": "weird"}],
            "ferrosurf": {"mcp_servers": [{"name": "s"}], "user_styles": [{"matches": ["*://*/*"], "css": "a{}"}]}
        }"#,
        )
        .unwrap();
        assert_eq!(m.manifest_version, 3);
        assert_eq!(m.content_scripts[0].run_at, RunAt::DocumentIdle);
        let d = m.declarative_net_request.unwrap();
        assert_eq!(d.rule_resources.len(), 2);
        assert!(d.rule_resources[1].enabled);
        assert_eq!(m.chrome_url_overrides.unwrap().newtab.unwrap(), "new.html");
        let f = m.ferrosurf.unwrap();
        assert_eq!(f.mcp_servers.len(), 1);
        assert_eq!(f.user_styles[0].css, "a{}");
    }

    #[test]
    fn minimal_and_invalid() {
        let m = Manifest::from_json(r#"{"name":"x","version":"1"}"#).unwrap();
        assert!(m.content_scripts.is_empty() && m.icons.is_empty());
        assert!(Manifest::from_json(r#"{"version":"1"}"#).is_err());
        assert!(Manifest::from_json("not json").is_err());
        assert!(Manifest::from_json(r#"{"name":" ","version":"1"}"#).is_err());
    }

    #[test]
    fn i18n_resolution() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("_locales/en")).unwrap();
        std::fs::create_dir_all(r.join("_locales/de")).unwrap();
        std::fs::write(
            r.join("manifest.json"),
            r#"{"name":"__MSG_extName__","version":"1","description":"__MSG_desc__ (__MSG_missing__)","default_locale":"de"}"#,
        )
        .unwrap();
        std::fs::write(
            r.join("_locales/de/messages.json"),
            r#"{"extName":{"message":"Dunkel"},"desc":{"message":"Hi $USER$ $1 $$","placeholders":{"user":{"content":"$1"}}}}"#,
        )
        .unwrap();
        std::fs::write(
            r.join("_locales/en/messages.json"),
            r#"{"extName":{"message":"Dark"}}"#,
        )
        .unwrap();
        let m = Manifest::load(r).unwrap();
        assert_eq!(m.name, "Dunkel");
        assert_eq!(m.description.unwrap(), "Hi $1 $1 $ (__MSG_missing__)");
    }
}
