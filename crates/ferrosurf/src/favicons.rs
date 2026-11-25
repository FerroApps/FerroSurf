//! Favicon cache shared by tabs, bookmarks, history and the new-tab page.
//!
//! Icons are stored per host as PNG in `<profile>/favicons/<host>.png`. They are written
//! when Servo reports a page favicon, and fetched in the background (`/favicon.ico`) for
//! hosts that only appear in bookmarks or history. Misses are remembered for a day so
//! unreachable hosts aren't retried on every frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use url::Url;

/// Normalised cache key for a URL: its host without `www.`.
pub fn host_key(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    // Keep the key filesystem-safe on Windows and Linux.
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        .then_some(host)
}

#[derive(Default)]
struct Inner {
    /// Hosts whose fetch is in flight.
    fetching: HashSet<String>,
    /// Hosts that failed recently.
    misses: HashMap<String, SystemTime>,
    /// Bumped whenever a new icon lands, so UIs can refresh texture caches.
    revision: u64,
}

#[derive(Clone)]
pub struct FaviconStore {
    dir: PathBuf,
    inner: Arc<Mutex<Inner>>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FaviconStore {
    pub fn new(dir: PathBuf, wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            inner: Default::default(),
            wake,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn revision(&self) -> u64 {
        self.inner.lock().revision
    }

    pub fn path_for_host(&self, host: &str) -> PathBuf {
        self.dir.join(format!("{host}.png"))
    }

    /// Cached PNG bytes for `url`'s host, if any.
    #[cfg(test)]
    pub fn png_for_url(&self, url: &str) -> Option<Vec<u8>> {
        let host = host_key(url)?;
        std::fs::read(self.path_for_host(&host)).ok()
    }

    /// Store an icon decoded by Servo (RGBA8).
    pub fn store_rgba(&self, url: &str, width: u32, height: u32, rgba: &[u8]) {
        let Some(host) = host_key(url) else { return };
        let Some(img) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) else {
            return;
        };
        let img = if width > 64 || height > 64 {
            image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        if img
            .save_with_format(self.path_for_host(&host), image::ImageFormat::Png)
            .is_ok()
        {
            let mut inner = self.inner.lock();
            inner.misses.remove(&host);
            inner.revision += 1;
        }
    }

    /// Make sure an icon for `url` exists, fetching `/favicon.ico` in the background.
    pub fn ensure(&self, url: &str) {
        let Some(host) = host_key(url) else { return };
        if self.path_for_host(&host).exists() {
            return;
        }
        {
            let mut inner = self.inner.lock();
            if inner.fetching.contains(&host) {
                return;
            }
            if let Some(t) = inner.misses.get(&host)
                && t.elapsed().unwrap_or_default() < Duration::from_secs(24 * 3600)
            {
                return;
            }
            // Cap concurrent fetches.
            if inner.fetching.len() >= 6 {
                return;
            }
            inner.fetching.insert(host.clone());
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let ok = fetch_icon(&host).and_then(|img| {
                img.save_with_format(this.path_for_host(&host), image::ImageFormat::Png)
                    .ok()
            });
            let mut inner = this.inner.lock();
            inner.fetching.remove(&host);
            match ok {
                Some(()) => inner.revision += 1,
                None => {
                    inner.misses.insert(host, SystemTime::now());
                }
            }
            drop(inner);
            if let Some(wake) = &this.wake {
                wake();
            }
        });
    }
}

/// Download and decode `https://<host>/favicon.ico`.
fn fetch_icon(host: &str) -> Option<image::RgbaImage> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .user_agent("Mozilla/5.0 FerroSurf")
        .build()
        .into();
    for candidate in [
        format!("https://{host}/favicon.ico"),
        format!("https://www.{host}/favicon.ico"),
    ] {
        let Ok(mut resp) = agent.get(&candidate).call() else {
            continue;
        };
        let Ok(bytes) = resp
            .body_mut()
            .with_config()
            .limit(512 * 1024)
            .read_to_vec()
        else {
            continue;
        };
        if let Ok(img) = image::load_from_memory(&bytes) {
            let img = img.to_rgba8();
            return Some(if img.width() > 64 {
                image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle)
            } else {
                img
            });
        }
    }
    None
}

/// egui texture for `url`'s cached favicon (loads lazily, re-checks when the store changes).
pub fn texture(
    ctx: &egui::Context,
    store: &FaviconStore,
    url: &str,
) -> Option<egui::TextureHandle> {
    type Cache = Arc<Mutex<(u64, HashMap<String, Option<egui::TextureHandle>>)>>;
    let host = host_key(url)?;
    let cache: Cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Cache>(egui::Id::new("favicon-textures"))
            .clone()
    });
    let mut cache = cache.lock();
    let rev = store.revision();
    if cache.0 != rev {
        // New icons arrived: forget misses (keep loaded textures).
        cache.0 = rev;
        cache.1.retain(|_, t| t.is_some());
    }
    if let Some(t) = cache.1.get(&host) {
        return t.clone();
    }
    let tex = std::fs::read(store.path_for_host(&host))
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok())
        .map(|img| {
            let img = img.to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            ctx.load_texture(format!("fav-{host}"), ci, egui::TextureOptions::LINEAR)
        });
    if tex.is_none() {
        store.ensure(url);
    }
    cache.1.insert(host, tex.clone());
    tex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            host_key("https://www.Servo.org/blog").as_deref(),
            Some("servo.org")
        );
        assert_eq!(host_key("ferro://newtab"), None);
        assert_eq!(
            host_key("http://127.0.0.1:8099/").as_deref(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn store_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = FaviconStore::new(dir.path().to_path_buf(), None);
        store.store_rgba("https://example.com/x", 2, 2, &[255u8; 16]);
        assert!(store.png_for_url("https://www.example.com/").is_some());
        assert_eq!(store.revision(), 1);
    }
}
