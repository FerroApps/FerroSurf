//! Installation, persistence and runtime queries for extensions.

use crate::blocker::DnrRule;
use crate::extension::{Extension, InstallSource, safe_join};
use crate::manifest::Manifest;
use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const STATE_FILE: &str = "extensions.json";
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;

#[derive(Serialize, Deserialize)]
struct State {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    extensions: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    id: String,
    enabled: bool,
    installed_ms: i64,
    source: InstallSource,
    /// Only for `Unpacked` extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ExtensionManager {
    dir: PathBuf,
    exts: Vec<Extension>,
    revision: u64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn sanitize_id(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_ascii_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            c
        } else {
            '-'
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_matches(['-', '.']).to_owned();
    out.chars().take(64).collect()
}

fn short_hash(s: &str) -> String {
    hex::encode(&Sha256::digest(s.as_bytes())[..4])
}

/// Stable extension id: gecko id, `key`-derived, or sanitized name + hash of the name.
fn derive_id(m: &Manifest) -> String {
    if let Some(g) = m.gecko_id() {
        let s = sanitize_id(g);
        if !s.is_empty() {
            return s;
        }
    }
    if let Some(k) = m.key.as_deref().filter(|k| !k.trim().is_empty()) {
        let h = Sha256::digest(k.split_whitespace().collect::<String>().as_bytes());
        // Chrome style: first 128 bits mapped to a-p.
        return h[..16]
            .iter()
            .flat_map(|b| [b >> 4, b & 0xf])
            .map(|n| (b'a' + n) as char)
            .collect();
    }
    let s = sanitize_id(&m.name);
    let s = if s.is_empty() {
        "extension".to_owned()
    } else {
        s
    };
    format!("{s}-{}", short_hash(&m.name))
}

fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &to)?;
        } // symlinks and special files are skipped
    }
    Ok(())
}

/// Removes the directory on drop unless disarmed.
struct Cleanup(Option<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn unique_suffix() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Locate the zip payload: raw zip, or after a CRX2/CRX3 header.
fn zip_payload(data: &[u8]) -> anyhow::Result<&[u8]> {
    const PK: &[u8] = b"PK\x03\x04";
    if data.starts_with(PK) {
        return Ok(data);
    }
    if data.starts_with(b"Cr24") {
        let u32_at = |o: usize| -> Option<usize> {
            Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize)
        };
        let offset = match u32_at(4) {
            Some(2) => (|| 16usize.checked_add(u32_at(8)?)?.checked_add(u32_at(12)?))(),
            Some(3) => (|| 12usize.checked_add(u32_at(8)?))(),
            _ => None,
        };
        if let Some(o) = offset.filter(|o| data.get(*o..).is_some_and(|d| d.starts_with(PK))) {
            return Ok(&data[o..]);
        }
        // Fall back to scanning for the first local file header.
        if let Some(i) = data.windows(4).position(|w| w == PK) {
            return Ok(&data[i..]);
        }
        bail!("CRX file contains no zip payload");
    }
    // Zip files with a prefix (e.g. self-extractors): scan.
    data.windows(4)
        .position(|w| w == PK)
        .map(|i| &data[i..])
        .ok_or_else(|| anyhow!("not a zip/xpi/crx archive"))
}

fn extract_zip(data: &[u8], dest: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).context("reading archive")?;
    if zip.len() > MAX_ENTRIES {
        bail!("archive has too many entries");
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name().to_owned();
        let rel = f
            .enclosed_name()
            .ok_or_else(|| anyhow!("unsafe path in archive: {name:?}"))?;
        if name.contains('\\') || name.contains('\0') {
            bail!("unsafe path in archive: {name:?}");
        }
        let out = dest.join(&rel);
        if !out.starts_with(dest) {
            bail!("unsafe path in archive: {name:?}");
        }
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            continue; // never materialize symlinks
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut file = std::fs::File::create(&out)?;
        let remaining = MAX_EXTRACTED_BYTES - total;
        let n = std::io::copy(&mut (&mut f).take(remaining + 1), &mut file)?;
        total += n;
        if total > MAX_EXTRACTED_BYTES {
            bail!("archive expands to too much data");
        }
        file.flush()?;
    }
    Ok(())
}

fn find_manifest_root(dir: &Path) -> anyhow::Result<PathBuf> {
    if dir.join("manifest.json").is_file() {
        return Ok(dir.to_path_buf());
    }
    let subdirs: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    for d in subdirs {
        if d.join("manifest.json").is_file() {
            return Ok(d);
        }
    }
    bail!("archive contains no manifest.json")
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

impl ExtensionManager {
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut mgr = Self {
            dir,
            exts: Vec::new(),
            revision: 0,
        };
        let state_path = mgr.dir.join(STATE_FILE);
        if state_path.exists() {
            let parsed = std::fs::read_to_string(&state_path)
                .map_err(anyhow::Error::from)
                .and_then(|t| serde_json::from_str::<State>(&t).map_err(Into::into));
            match parsed {
                Ok(state) => {
                    for r in state.extensions {
                        let root = match (r.source, &r.path) {
                            (InstallSource::Unpacked, Some(p)) => p.clone(),
                            (InstallSource::Unpacked, None) => continue,
                            _ => mgr.dir.join(&r.id),
                        };
                        match Manifest::load(&root) {
                            Ok(manifest) => mgr.exts.push(Extension {
                                id: r.id,
                                manifest,
                                root,
                                enabled: r.enabled,
                                installed_ms: r.installed_ms,
                                source: r.source,
                            }),
                            Err(e) => log::warn!("skipping extension {}: {e:#}", r.id),
                        }
                    }
                }
                Err(e) => {
                    log::error!("corrupt {}: {e:#}; starting empty", state_path.display());
                    let _ = std::fs::rename(&state_path, mgr.dir.join("extensions.json.corrupt"));
                }
            }
        }
        Ok(mgr)
    }

    pub fn list(&self) -> &[Extension] {
        &self.exts
    }

    pub fn get(&self, id: &str) -> Option<&Extension> {
        self.exts.iter().find(|e| e.id == id)
    }

    /// Increases on every change (install, uninstall, enable, reload).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn save(&mut self) -> anyhow::Result<()> {
        self.revision += 1;
        let state = State {
            version: 1,
            extensions: self
                .exts
                .iter()
                .map(|e| Record {
                    id: e.id.clone(),
                    enabled: e.enabled,
                    installed_ms: e.installed_ms,
                    source: e.source,
                    path: (e.source == InstallSource::Unpacked).then(|| e.root.clone()),
                })
                .collect(),
        };
        let tmp = self.dir.join(format!("{STATE_FILE}.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&state)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, self.dir.join(STATE_FILE)).context("writing extensions.json")?;
        Ok(())
    }

    fn upsert(&mut self, id: &str, manifest: Manifest, root: PathBuf, source: InstallSource) {
        if let Some(e) = self.exts.iter_mut().find(|e| e.id == id) {
            e.manifest = manifest;
            e.root = root;
            e.source = source;
        } else {
            self.exts.push(Extension {
                id: id.to_owned(),
                manifest,
                root,
                enabled: true,
                installed_ms: now_ms(),
                source,
            });
        }
    }

    fn remove_copy(&self, id: &str) {
        let p = self.dir.join(id);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    /// Copy an extension directory into `<dir>/<id>/`.
    pub fn install_from_dir(&mut self, src: &Path) -> anyhow::Result<String> {
        let manifest = Manifest::load(src)?;
        let id = derive_id(&manifest);
        let dest = self.dir.join(&id);
        let same = dest
            .canonicalize()
            .ok()
            .is_some_and(|d| src.canonicalize().ok() == Some(d));
        if !same {
            let staging = self.dir.join(format!(".staging-{}", unique_suffix()));
            let mut guard = Cleanup(Some(staging.clone()));
            copy_dir(src, &staging)?;
            if dest.exists() {
                std::fs::remove_dir_all(&dest)?;
            }
            std::fs::rename(&staging, &dest)?;
            guard.0 = None;
        }
        self.upsert(&id, manifest, dest, InstallSource::Directory);
        self.save()?;
        Ok(id)
    }

    /// Developer mode: reference `src` in place.
    pub fn install_unpacked(&mut self, src: &Path) -> anyhow::Result<String> {
        let root = src
            .canonicalize()
            .with_context(|| format!("resolving {}", src.display()))?;
        let manifest = Manifest::load(&root)?;
        let id = derive_id(&manifest);
        if root.starts_with(&self.dir) {
            bail!("unpacked extensions must live outside the managed extensions directory");
        }
        if self
            .get(&id)
            .is_some_and(|e| e.source != InstallSource::Unpacked)
        {
            self.remove_copy(&id);
        }
        self.upsert(&id, manifest, root, InstallSource::Unpacked);
        self.save()?;
        Ok(id)
    }

    /// Install a `.zip`, `.xpi` or `.crx` (CRX2/CRX3) archive.
    pub fn install_from_archive(&mut self, file: &Path) -> anyhow::Result<String> {
        let len = std::fs::metadata(file)
            .with_context(|| format!("reading {}", file.display()))?
            .len();
        if len > MAX_ARCHIVE_BYTES {
            bail!("archive is too large");
        }
        let data = std::fs::read(file)?;
        let payload = zip_payload(&data)?;
        let staging = self.dir.join(format!(".staging-{}", unique_suffix()));
        let _guard = Cleanup(Some(staging.clone()));
        std::fs::create_dir_all(&staging)?;
        extract_zip(payload, &staging)?;
        let root = find_manifest_root(&staging)?;
        let manifest = Manifest::load(&root)?;
        let id = derive_id(&manifest);
        let dest = self.dir.join(&id);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&root, &dest)?;
        self.upsert(&id, manifest, dest, InstallSource::Archive);
        self.save()?;
        Ok(id)
    }

    pub fn uninstall(&mut self, id: &str) -> anyhow::Result<()> {
        let pos = self
            .exts
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| anyhow!("no such extension: {id}"))?;
        let ext = self.exts.remove(pos);
        if ext.source != InstallSource::Unpacked {
            self.remove_copy(&ext.id);
        }
        self.save()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> anyhow::Result<()> {
        let e = self
            .exts
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow!("no such extension: {id}"))?;
        e.enabled = enabled;
        self.save()
    }

    /// Re-read manifests from disk. Extensions whose manifest no longer
    /// parses keep their previous manifest (a warning is logged).
    pub fn reload(&mut self) -> anyhow::Result<()> {
        for e in &mut self.exts {
            match Manifest::load(&e.root) {
                Ok(m) => e.manifest = m,
                Err(err) => log::warn!("reload of {} failed: {err:#}", e.id),
            }
        }
        self.save()
    }

    fn enabled(&self) -> impl Iterator<Item = &Extension> {
        self.exts.iter().filter(|e| e.enabled)
    }

    /// One JS program per enabled extension that has content scripts or user styles.
    pub fn user_scripts(&self) -> Vec<(String, String)> {
        self.enabled()
            .filter_map(|e| crate::userscript::generate(e).map(|js| (e.id.clone(), js)))
            .collect()
    }

    pub fn newtab_override(&self) -> Option<PathBuf> {
        self.enabled().find_map(|e| {
            let p = e
                .manifest
                .chrome_url_overrides
                .as_ref()?
                .newtab
                .as_deref()?;
            e.resolve(p)
        })
    }

    /// Read a file of an enabled extension; `path` may be percent-encoded and carry a
    /// leading `/`, query or fragment.
    pub fn read_resource(&self, id: &str, path: &str) -> Option<(Vec<u8>, &'static str)> {
        let ext = self.get(id).filter(|e| e.enabled)?;
        let path = path.split(['?', '#']).next()?;
        let decoded = percent_decode(path)?;
        let file = ext.resolve(&decoded)?;
        let data = std::fs::read(&file).ok()?;
        Some((data, mime_for(&file)))
    }

    pub fn dnr_rules(&self) -> Vec<DnrRule> {
        let mut out = Vec::new();
        for e in self.enabled() {
            let Some(dnr) = &e.manifest.declarative_net_request else {
                continue;
            };
            for rr in dnr.rule_resources.iter().filter(|r| r.enabled) {
                let Some(p) = e.resolve(&rr.path) else {
                    log::warn!("extension {}: missing rule file {}", e.id, rr.path);
                    continue;
                };
                let parsed = std::fs::read_to_string(&p)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| {
                        serde_json::from_str::<Vec<serde_json::Value>>(
                            t.trim_start_matches('\u{feff}'),
                        )
                        .map_err(Into::into)
                    });
                let values = match parsed {
                    Ok(v) => v,
                    Err(err) => {
                        log::warn!("extension {}: bad rule file {}: {err:#}", e.id, rr.path);
                        continue;
                    }
                };
                for v in values {
                    match serde_json::from_value::<DnrRule>(v) {
                        Ok(mut rule) => {
                            if let Some(rd) = rule.action.redirect.as_mut()
                                && let Some(ep) = rd.extension_path.take()
                                && safe_join(&e.root, &ep).is_some()
                            {
                                rd.url = Some(format!(
                                    "ferro-extension://{}/{}",
                                    e.id,
                                    ep.trim_start_matches('/')
                                ));
                            }
                            out.push(rule);
                        }
                        Err(err) => log::warn!("extension {}: bad DNR rule: {err}", e.id),
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn sample_ext(root: &Path, name: &str) {
        write(
            root,
            "manifest.json",
            &format!(
                r#"{{"manifest_version":3,"name":"{name}","version":"1.0","description":"d",
                "icons":{{"16":"i16.png","48":"i48.png","128":"i128.png"}},
                "content_scripts":[
                  {{"matches":["https://*.example.com/*"],"exclude_matches":["https://x.example.com/*"],
                    "js":["a.js"],"css":["a.css"],"run_at":"document_start"}},
                  {{"matches":["<all_urls>"],"js":["b.js"],"all_frames":true}}],
                "declarative_net_request":{{"rule_resources":[{{"id":"r","path":"rules.json"}}]}},
                "chrome_url_overrides":{{"newtab":"new.html"}},
                "ferrosurf":{{"user_styles":[{{"matches":["*://*/*"],"css":"body{{color:red}}"}}]}}}}"#
            ),
        );
        write(root, "a.js", "console.log('a');");
        write(
            root,
            "b.js",
            "chrome.storage.local.set({k:1}); // trailing comment",
        );
        write(root, "a.css", "p{color:blue}");
        write(root, "new.html", "<h1>new</h1>");
        write(root, "i16.png", "16");
        write(root, "i48.png", "48");
        write(root, "i128.png", "128");
        write(
            root,
            "rules.json",
            r#"[{"id":1,"action":{"type":"block"},"condition":{"urlFilter":"||ads.com^"}},
                {"id":2,"action":{"type":"redirect","redirect":{"extensionPath":"/blocked.html"}},
                 "condition":{"urlFilter":"||bad.com^","resourceTypes":["main_frame"]}},
                {"id":3,"action":{"type":"nonsense"}}]"#,
        );
    }

    fn mgr() -> (tempfile::TempDir, ExtensionManager) {
        let d = tempfile::tempdir().unwrap();
        let m = ExtensionManager::open(d.path().join("extensions")).unwrap();
        (d, m)
    }

    #[test]
    fn install_dir_persist_and_queries() {
        let (d, mut m) = mgr();
        let src = d.path().join("src");
        sample_ext(&src, "My Ext");
        let r0 = m.revision();
        let id = m.install_from_dir(&src).unwrap();
        assert!(id.starts_with("my-ext-"), "{id}");
        assert!(m.revision() > r0);
        assert_eq!(m.list().len(), 1);
        let e = m.get(&id).unwrap();
        assert!(e.enabled && e.source == InstallSource::Directory);
        assert!(e.root.join("a.js").is_file());
        // id is stable across reinstall
        assert_eq!(m.install_from_dir(&src).unwrap(), id);
        assert_eq!(m.list().len(), 1);

        // icons
        let e = m.get(&id).unwrap();
        assert!(e.icon_path(16).unwrap().ends_with("i16.png"));
        assert!(e.icon_path(20).unwrap().ends_with("i48.png"));
        assert!(e.icon_path(1000).unwrap().ends_with("i128.png"));

        // resources
        let (data, mime) = m.read_resource(&id, "/new.html?x=1").unwrap();
        assert_eq!(data, b"<h1>new</h1>");
        assert!(mime.starts_with("text/html"));
        assert!(m.read_resource(&id, "../../extensions.json").is_none());
        assert!(m.read_resource(&id, "%2e%2e/extensions.json").is_none());
        assert!(m.read_resource(&id, "sub/../a.js").is_none());
        assert!(m.read_resource(&id, "nope.js").is_none());
        assert!(m.read_resource("other", "a.js").is_none());
        assert!(m.newtab_override().unwrap().ends_with("new.html"));

        // dnr
        let rules = m.dnr_rules();
        assert_eq!(rules.len(), 3); // unsupported action types still parse (Blocker skips them)
        assert_eq!(
            rules[1].action.redirect.as_ref().unwrap().url.as_deref(),
            Some(format!("ferro-extension://{id}/blocked.html").as_str())
        );

        // disable
        m.set_enabled(&id, false).unwrap();
        assert!(m.user_scripts().is_empty());
        assert!(m.newtab_override().is_none());
        assert!(m.read_resource(&id, "a.js").is_none());
        assert!(m.dnr_rules().is_empty());

        // persistence
        let dir = d.path().join("extensions");
        let m2 = ExtensionManager::open(dir.clone()).unwrap();
        assert_eq!(m2.list().len(), 1);
        assert!(!m2.list()[0].enabled);
        assert!(dir.join("extensions.json").is_file());
        assert!(!dir.join("extensions.json.tmp").exists());

        // uninstall
        m.uninstall(&id).unwrap();
        assert!(m.list().is_empty() && !dir.join(&id).exists());
        assert!(m.uninstall(&id).is_err());
    }

    #[test]
    fn unpacked_reload() {
        let (d, mut m) = mgr();
        let src = d.path().join("dev");
        write(&src, "manifest.json", r#"{"name":"Dev","version":"1"}"#);
        let id = m.install_unpacked(&src).unwrap();
        assert_eq!(m.get(&id).unwrap().source, InstallSource::Unpacked);
        assert!(!d.path().join("extensions").join(&id).exists());
        write(&src, "manifest.json", r#"{"name":"Dev","version":"2"}"#);
        assert_eq!(m.get(&id).unwrap().manifest.version, "1");
        let r = m.revision();
        m.reload().unwrap();
        assert_eq!(m.get(&id).unwrap().manifest.version, "2");
        assert!(m.revision() > r);
        // broken manifest keeps old one
        write(&src, "manifest.json", "{ nope");
        m.reload().unwrap();
        assert_eq!(m.get(&id).unwrap().manifest.version, "2");
        write(&src, "manifest.json", r#"{"name":"Dev","version":"3"}"#);
        // persisted by path; uninstall keeps source folder
        let m2 = ExtensionManager::open(d.path().join("extensions")).unwrap();
        assert_eq!(m2.list()[0].manifest.version, "3");
        m.uninstall(&id).unwrap();
        assert!(src.join("manifest.json").is_file());
    }

    #[test]
    fn id_derivation() {
        let g = Manifest::from_json(
            r#"{"name":"x","version":"1","browser_specific_settings":{"gecko":{"id":"{abc-123}@Foo.org"}}}"#,
        )
        .unwrap();
        assert_eq!(derive_id(&g), "abc-123-foo.org");
        let k = Manifest::from_json(r#"{"name":"x","version":"1","key":"MIIB abc"}"#).unwrap();
        let id = derive_id(&k);
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| ('a'..='p').contains(&c)));
        let n1 = Manifest::from_json(r#"{"name":"Some Name!","version":"1"}"#).unwrap();
        let n2 = Manifest::from_json(r#"{"name":"Some Name!","version":"2"}"#).unwrap();
        assert_eq!(derive_id(&n1), derive_id(&n2));
        assert!(derive_id(&n1).starts_with("some-name-"));
    }

    fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (n, c) in entries {
            w.start_file(*n, opts).unwrap();
            w.write_all(c.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn install_zip_xpi_and_nested_folder() {
        let (d, mut m) = mgr();
        let z = build_zip(&[
            (
                "manifest.json",
                r#"{"name":"Zipped","version":"1","content_scripts":[{"matches":["<all_urls>"],"js":["c.js"]}]}"#,
            ),
            ("c.js", "1;"),
        ]);
        let f = d.path().join("e.xpi");
        std::fs::write(&f, &z).unwrap();
        let id = m.install_from_archive(&f).unwrap();
        assert_eq!(m.get(&id).unwrap().source, InstallSource::Archive);
        assert!(m.get(&id).unwrap().root.join("c.js").is_file());

        let z2 = build_zip(&[("inner/manifest.json", r#"{"name":"Nested","version":"1"}"#)]);
        let f2 = d.path().join("n.zip");
        std::fs::write(&f2, &z2).unwrap();
        let id2 = m.install_from_archive(&f2).unwrap();
        assert!(m.get(&id2).unwrap().root.join("manifest.json").is_file());
        // staging dirs are cleaned
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join("extensions"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn install_crx2_and_crx3() {
        let z = build_zip(&[("manifest.json", r#"{"name":"Crx","version":"1"}"#)]);
        // CRX3: magic, version 3, header_len, header (contains a fake PK marker to test parsing)
        let mut crx3 = b"Cr24".to_vec();
        crx3.extend(3u32.to_le_bytes());
        let header = b"xxPK\x03\x04yy".to_vec();
        crx3.extend((header.len() as u32).to_le_bytes());
        crx3.extend(&header);
        crx3.extend(&z);
        // CRX2: magic, version 2, pk_len, sig_len, key, sig
        let mut crx2 = b"Cr24".to_vec();
        crx2.extend(2u32.to_le_bytes());
        crx2.extend(5u32.to_le_bytes());
        crx2.extend(3u32.to_le_bytes());
        crx2.extend(b"KEYKEsig");
        crx2.extend(&z);
        for (name, data) in [("a.crx", crx3), ("b.crx", crx2)] {
            let (d, mut m) = mgr();
            let f = d.path().join(name);
            std::fs::write(&f, &data).unwrap();
            let id = m.install_from_archive(&f).unwrap();
            assert_eq!(m.get(&id).unwrap().manifest.name, "Crx");
        }
        assert!(zip_payload(b"Cr24\x03\x00\x00\x00").is_err());
        assert!(zip_payload(b"garbage").is_err());
    }

    #[test]
    fn zip_slip_rejected() {
        let (d, mut m) = mgr();
        let z = build_zip(&[
            ("manifest.json", r#"{"name":"Evil","version":"1"}"#),
            ("../evil.txt", "pwned"),
        ]);
        let f = d.path().join("evil.zip");
        std::fs::write(&f, &z).unwrap();
        let err = m.install_from_archive(&f).unwrap_err();
        assert!(format!("{err:#}").contains("unsafe path"), "{err:#}");
        assert!(m.list().is_empty());
        assert!(!d.path().join("extensions").join("evil.txt").exists());
        assert!(!d.path().join("evil.txt").exists());
        let z = build_zip(&[("/abs.txt", "x"), ("manifest.json", "{}")]);
        std::fs::write(&f, &z).unwrap();
        assert!(m.install_from_archive(&f).is_err());
    }

    #[test]
    fn archive_without_manifest_fails() {
        let (d, mut m) = mgr();
        let z = build_zip(&[("readme.txt", "hi")]);
        let f = d.path().join("x.zip");
        std::fs::write(&f, &z).unwrap();
        assert!(m.install_from_archive(&f).is_err());
    }

    #[test]
    fn user_scripts_output() {
        let (d, mut m) = mgr();
        let src = d.path().join("src");
        sample_ext(&src, "My Ext");
        let id = m.install_from_dir(&src).unwrap();
        let scripts = m.user_scripts();
        assert_eq!(scripts.len(), 1);
        let (sid, js) = &scripts[0];
        assert_eq!(sid, &id);
        // regexes for include and exclude patterns
        let mp = crate::MatchPattern::parse("https://*.example.com/*").unwrap();
        let lit = serde_json::to_string(&mp.to_js_regex()).unwrap();
        assert!(js.contains(&format!("new RegExp({lit})")), "{js}");
        let ex = crate::MatchPattern::parse("https://x.example.com/*").unwrap();
        assert!(js.contains(&serde_json::to_string(&ex.to_js_regex()).unwrap()));
        // shim pieces
        for needle in [
            "ferro-extension://",
            "__ferrosurf_ext_",
            "getManifest",
            "getMessage",
            "storage",
            "document_start",
            "DOMContentLoaded",
            "requestIdleCallback",
            "console.error",
            "[ext My Ext]",
            "console.log('a');",
            "body{color:red}",
            "p{color:blue}",
            "trailing comment",
        ] {
            assert!(js.contains(needle), "missing {needle}");
        }
        assert!(js.contains(&format!("var EXT_ID=\"{id}\"")));
        // plausibility: balanced brackets
        for (o, c) in [('{', '}'), ('(', ')'), ('[', ']')] {
            assert_eq!(
                js.matches(o).count(),
                js.matches(c).count(),
                "unbalanced {o}{c}"
            );
        }
        assert!(js.trim_start().starts_with("(function(){") && js.trim_end().ends_with("})();"));
    }

    #[test]
    fn user_scripts_skip_extensions_without_content() {
        let (d, mut m) = mgr();
        let src = d.path().join("s");
        write(&src, "manifest.json", r#"{"name":"Nothing","version":"1"}"#);
        m.install_from_dir(&src).unwrap();
        assert!(m.user_scripts().is_empty());
    }
}
