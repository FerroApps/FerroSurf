//! Installing extensions from the Chrome Web Store and addons.mozilla.org.
//!
//! The stores' own "Add to Chrome/Firefox" buttons only work in those browsers, so
//! FerroSurf detects an extension page, offers its own install button and downloads the
//! package (`.crx` / `.xpi`) directly.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use url::Url;

/// Chrome version reported to the Web Store update service.
const CHROME_PRODVERSION: &str = "131.0.6778.85";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreItem {
    /// Chrome Web Store extension id (32 chars a-p).
    Chrome { id: String },
    /// addons.mozilla.org slug or numeric id.
    Firefox { slug: String },
    /// A direct link to a package file.
    Direct { url: Url },
}

impl StoreItem {
    pub fn store_name(&self) -> &'static str {
        match self {
            StoreItem::Chrome { .. } => "Chrome Web Store",
            StoreItem::Firefox { .. } => "Firefox Add-ons",
            StoreItem::Direct { .. } => "enlace directo",
        }
    }
}

fn is_chrome_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Recognise an extension detail page (or package link).
pub fn detect(url: &Url) -> Option<StoreItem> {
    let host = url.host_str()?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    match host {
        // https://chromewebstore.google.com/detail/<slug>/<id>
        // https://chrome.google.com/webstore/detail/<slug>/<id>
        "chromewebstore.google.com" | "chrome.google.com" => {
            let pos = segments.iter().position(|s| *s == "detail")?;
            segments[pos + 1..]
                .iter()
                .find(|s| is_chrome_id(s))
                .map(|id| StoreItem::Chrome { id: id.to_string() })
        }
        // https://microsoftedge.microsoft.com/addons/detail/<slug>/<id> (Edge ids are not
        // Chrome ids; the Edge store isn't supported).
        // https://addons.mozilla.org/<locale>/firefox/addon/<slug>/
        "addons.mozilla.org" => {
            let pos = segments.iter().position(|s| *s == "addon")?;
            segments.get(pos + 1).map(|slug| StoreItem::Firefox {
                slug: slug.to_string(),
            })
        }
        _ => {
            let path = url.path().to_ascii_lowercase();
            (path.ends_with(".xpi") || path.ends_with(".crx"))
                .then(|| StoreItem::Direct { url: url.clone() })
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) FerroSurf")
        .build()
        .into()
}

/// URL of the package to download for `item` (may hit the AMO API).
pub fn package_url(item: &StoreItem) -> anyhow::Result<(Url, &'static str)> {
    match item {
        StoreItem::Chrome { id } => {
            let url = format!(
                "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={CHROME_PRODVERSION}&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
            );
            Ok((Url::parse(&url)?, "crx"))
        }
        StoreItem::Firefox { slug } => {
            let api = format!("https://addons.mozilla.org/api/v5/addons/addon/{slug}/");
            let body = agent().get(&api).call()?.body_mut().read_to_string()?;
            let json: serde_json::Value = serde_json::from_str(&body)?;
            let file_url = json
                .pointer("/current_version/file/url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("addons.mozilla.org no devolvió ningún archivo para «{slug}»")
                })?;
            Ok((Url::parse(file_url)?, "xpi"))
        }
        StoreItem::Direct { url } => {
            let ext = if url.path().to_ascii_lowercase().ends_with(".xpi") {
                "xpi"
            } else {
                "crx"
            };
            Ok((url.clone(), ext))
        }
    }
}

/// Resolve and download the package into a temporary file. Blocking: run off the UI thread.
pub fn download(item: &StoreItem) -> anyhow::Result<PathBuf> {
    let (url, ext) = package_url(item)?;
    let mut resp = agent().get(url.as_str()).call()?;
    let mut bytes = Vec::new();
    resp.body_mut()
        .with_config()
        .limit(200 * 1024 * 1024)
        .reader()
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(!bytes.is_empty(), "la descarga está vacía");
    let dir = std::env::temp_dir().join("ferrosurf-extensions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.{}", uuid_like(), ext));
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

/// Features of `manifest` that FerroSurf can't run yet, for a post-install notice.
pub fn unsupported_features(
    manifest: &ferrosurf_extensions::Manifest,
    raw: &serde_json::Value,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if raw.get("background").is_some() {
        out.push("scripts en segundo plano");
    }
    if raw
        .get("action")
        .or_else(|| raw.get("browser_action"))
        .or_else(|| raw.get("page_action"))
        .is_some()
    {
        out.push("botón con ventana emergente");
    }
    if raw
        .get("options_page")
        .or_else(|| raw.get("options_ui"))
        .is_some()
    {
        out.push("página de opciones");
    }
    if raw
        .get("side_panel")
        .or_else(|| raw.get("sidebar_action"))
        .is_some()
    {
        out.push("panel lateral");
    }
    let _ = manifest;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Option<StoreItem> {
        detect(&Url::parse(s).unwrap())
    }

    #[test]
    fn detects_store_pages() {
        assert_eq!(
            d(
                "https://chromewebstore.google.com/detail/ublock-origin-lite/ddkjiahejlhfcafbddmgiahcphecmpfh?hl=es"
            ),
            Some(StoreItem::Chrome {
                id: "ddkjiahejlhfcafbddmgiahcphecmpfh".into()
            })
        );
        assert_eq!(
            d(
                "https://chrome.google.com/webstore/detail/dark-reader/eimadpbcbfnmbkopoojfekhnkhdbieeh"
            ),
            Some(StoreItem::Chrome {
                id: "eimadpbcbfnmbkopoojfekhnkhdbieeh".into()
            })
        );
        assert_eq!(
            d("https://addons.mozilla.org/es-ES/firefox/addon/darkreader/"),
            Some(StoreItem::Firefox {
                slug: "darkreader".into()
            })
        );
        assert!(matches!(
            d("https://example.com/x/ext.XPI"),
            Some(StoreItem::Direct { .. })
        ));
        assert_eq!(d("https://chromewebstore.google.com/"), None);
        assert_eq!(d("https://addons.mozilla.org/es-ES/firefox/"), None);
        assert_eq!(d("https://example.com/"), None);
    }

    #[test]
    fn chrome_package_url() {
        let (u, ext) = package_url(&StoreItem::Chrome {
            id: "ddkjiahejlhfcafbddmgiahcphecmpfh".into(),
        })
        .unwrap();
        assert_eq!(ext, "crx");
        assert!(
            u.as_str()
                .contains("id%3Dddkjiahejlhfcafbddmgiahcphecmpfh%26uc")
        );
    }
}
