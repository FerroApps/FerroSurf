//! Finding, reading and installing plugins: a folder or a `.ferroplug` zip,
//! each holding `plugin.toml`, `plugin.wasm` and optional static assets
//! (content scripts and styles).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::manifest::{MAX_MANIFEST_BYTES, Manifest, valid_asset_path};

/// Largest `plugin.wasm` we load.
pub const MAX_WASM_BYTES: u64 = 8 * 1024 * 1024;
/// Largest content script / stylesheet.
pub const MAX_ASSET_BYTES: u64 = 512 * 1024;
/// Largest `.ferroplug` file we open.
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Most files a package or folder install copies.
const MAX_FILES: usize = 64;
/// Most entries a plugins directory scan looks at.
const MAX_ENTRIES: usize = 256;
/// Extension of zipped plugins.
pub const PACKAGE_EXT: &str = "ferroplug";

/// One plugin found on disk.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub manifest: Manifest,
}

/// A directory entry that looked like a plugin but could not be read.
#[derive(Debug, Clone)]
pub struct Broken {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub found: Vec<Found>,
    pub broken: Vec<Broken>,
}

impl Scan {
    pub fn get(&self, id: &str) -> Option<&Found> {
        self.found.iter().find(|f| f.manifest.id == id)
    }
}

pub fn is_package(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == PACKAGE_EXT)
}

/// Every plugin in `dir` (sorted by name). A missing directory is empty.
/// Two plugins with the same id: the first by path wins, the other is
/// reported as broken.
pub fn scan(dir: &Path) -> Scan {
    let mut out = Scan::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() || is_package(p))
        .take(MAX_ENTRIES)
        .collect();
    paths.sort();
    for path in paths {
        match read_manifest(&path) {
            Ok(manifest) => {
                if out.get(&manifest.id).is_some() {
                    out.broken.push(Broken {
                        path,
                        error: format!("otro plugin ya usa el id {:?}", manifest.id),
                    });
                } else {
                    out.found.push(Found { path, manifest });
                }
            }
            Err(error) => out.broken.push(Broken { path, error }),
        }
    }
    out.found.sort_by(|a, b| {
        a.manifest
            .name
            .to_lowercase()
            .cmp(&b.manifest.name.to_lowercase())
    });
    out
}

/// Reads at most `max` bytes; more is an error (no zip bombs, no huge files).
fn read_capped(r: impl Read, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    r.take(max + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{what}: {e}"))?;
    if buf.len() as u64 > max {
        return Err(format!("{what} is larger than {} KiB", max / 1024));
    }
    Ok(buf)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if len > MAX_PACKAGE_BYTES {
        return Err("el paquete es demasiado grande".into());
    }
    zip::ZipArchive::new(f).map_err(|e| format!("not a valid .{PACKAGE_EXT}: {e}"))
}

/// Reads file `name` (a validated relative path) of the plugin at `path`.
pub fn read_file(path: &Path, name: &str, max: u64) -> Result<Vec<u8>, String> {
    if !valid_asset_path(name) {
        return Err(format!("bad file name {name:?}"));
    }
    if is_package(path) {
        let mut z = open_zip(path)?;
        let e = z
            .by_name(name)
            .map_err(|_| format!("the package has no {name}"))?;
        if e.size() > max {
            return Err(format!("{name} is too large"));
        }
        read_capped(e, max, name)
    } else {
        let f = std::fs::File::open(path.join(name)).map_err(|_| format!("missing {name}"))?;
        read_capped(f, max, name)
    }
}

/// The manifest of the plugin at `path` (folder or package).
pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let bytes = read_file(path, "plugin.toml", MAX_MANIFEST_BYTES as u64)?;
    let text = String::from_utf8(bytes).map_err(|_| "plugin.toml is not UTF-8".to_owned())?;
    Manifest::parse(&text)
}

/// The module of the plugin at `path`.
pub fn read_wasm(path: &Path) -> Result<Vec<u8>, String> {
    let wasm = read_file(path, "plugin.wasm", MAX_WASM_BYTES)?;
    if !wasm.starts_with(b"\0asm") {
        return Err("plugin.wasm is not a WebAssembly module".into());
    }
    Ok(wasm)
}

/// Text of one `[[content]]` entry's `(js, css)` files.
pub type ContentAssets = (Option<String>, Option<String>);

/// Static text assets a manifest references (content scripts, styles), one
/// [`ContentAssets`] per `[[content]]` entry.
pub fn read_content(path: &Path, m: &Manifest) -> Result<Vec<ContentAssets>, String> {
    let text = |name: &Option<String>| -> Result<Option<String>, String> {
        match name {
            Some(n) => {
                let b = read_file(path, n, MAX_ASSET_BYTES)?;
                Ok(Some(
                    String::from_utf8(b)
                        .map_err(|_| format!("{n} is not UTF-8"))?
                        .trim_start_matches('\u{feff}')
                        .to_owned(),
                ))
            }
            None => Ok(None),
        }
    };
    m.content
        .iter()
        .map(|c| Ok((text(&c.js)?, text(&c.css)?)))
        .collect()
}

/// Installs the plugin at `src` (a folder or `.ferroplug`) into `plugins_dir`
/// as the folder `<id>/`, replacing an older version. Returns the manifest.
/// The files are validated (sizes, paths, manifest, module header) before
/// anything is written.
pub fn install(src: &Path, plugins_dir: &Path) -> Result<Manifest, String> {
    let manifest = read_manifest(src)?;
    read_wasm(src)?;
    read_content(src, &manifest)?;

    // Collect (relative name, bytes) of every file to install.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if is_package(src) {
        let mut z = open_zip(src)?;
        let names: Vec<String> = z.file_names().map(str::to_owned).collect();
        for name in names {
            if name.ends_with('/') {
                continue;
            }
            if !valid_asset_path(&name) {
                return Err(format!("unsafe path in the package: {name:?}"));
            }
            if files.len() >= MAX_FILES {
                return Err("el paquete tiene demasiados archivos".into());
            }
            let e = z.by_name(&name).map_err(|e| e.to_string())?;
            let max = if name == "plugin.wasm" {
                MAX_WASM_BYTES
            } else {
                MAX_ASSET_BYTES
            };
            files.push((name.clone(), read_capped(e, max, &name)?));
        }
    } else {
        copy_dir_files(src, src, &mut files)?;
    }

    std::fs::create_dir_all(plugins_dir).map_err(|e| e.to_string())?;
    let dest = plugins_dir.join(&manifest.id);
    let tmp = plugins_dir.join(format!(".{}.installing", manifest.id));
    let _ = std::fs::remove_dir_all(&tmp);
    for (name, bytes) in &files {
        let p = tmp.join(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
    }
    // A same-id plugin installed from a package file is replaced too.
    if let Some(old) = scan(plugins_dir).get(&manifest.id) {
        let old = old.path.clone();
        if is_package(&old) {
            let _ = std::fs::remove_file(old);
        }
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn copy_dir_files(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for e in rd.flatten() {
        let p = e.path();
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            copy_dir_files(root, &p, out)?;
            continue;
        }
        if out.len() >= MAX_FILES {
            return Err("el plugin tiene demasiados archivos".into());
        }
        let rel = p
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let max = if rel == "plugin.wasm" {
            MAX_WASM_BYTES
        } else {
            MAX_ASSET_BYTES
        };
        let f = std::fs::File::open(&p).map_err(|e| e.to_string())?;
        out.push((rel.clone(), read_capped(f, max, &rel)?));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TOML: &str = "id = \"z\"\nname = \"Zed\"\nversion = \"1\"\napi_version = 1\n";

    fn make_zip(path: &Path, toml: &str, extra: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("plugin.toml", o).unwrap();
        z.write_all(toml.as_bytes()).unwrap();
        z.start_file("plugin.wasm", o).unwrap();
        z.write_all(b"\0asm\x01\0\0\0").unwrap();
        for (n, b) in extra {
            z.start_file(*n, o).unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn scans_folders_and_packages() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("plugin.toml"), TOML.replace("\"z\"", "\"a\"")).unwrap();
        std::fs::write(a.join("plugin.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let zp = dir.path().join("z.ferroplug");
        make_zip(&zp, TOML, &[]);
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        let dup = dir.path().join("dup");
        std::fs::create_dir(&dup).unwrap();
        std::fs::write(dup.join("plugin.toml"), TOML).unwrap();
        std::fs::write(dir.path().join("state.json"), "{}").unwrap();

        let s = scan(dir.path());
        let ids: Vec<_> = s.found.iter().map(|f| f.manifest.id.as_str()).collect();
        assert_eq!(ids, ["a", "z"]);
        assert_eq!(s.broken.len(), 2, "{:?}", s.broken);
        assert!(read_wasm(&zp).is_ok());
        assert!(read_wasm(&a).is_ok());
        assert!(read_wasm(&dup).is_err());
    }

    #[test]
    fn installs_package_and_folder() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let zp = src.path().join("z.ferroplug");
        make_zip(&zp, TOML, &[("inject.js", b"1")]);
        let m = install(&zp, dest.path()).unwrap();
        assert_eq!(m.id, "z");
        assert!(dest.path().join("z/plugin.wasm").is_file());
        assert!(dest.path().join("z/inject.js").is_file());
        // Folder install replaces.
        let f = src.path().join("folder");
        std::fs::create_dir(&f).unwrap();
        std::fs::write(f.join("plugin.toml"), TOML.replace("\"1\"", "\"2\"")).unwrap();
        std::fs::write(f.join("plugin.wasm"), b"\0asm\x01\0\0\0").unwrap();
        install(&f, dest.path()).unwrap();
        assert_eq!(read_manifest(&dest.path().join("z")).unwrap().version, "2");
        assert!(!dest.path().join("z/inject.js").exists());
    }

    #[test]
    fn refuses_unsafe_packages() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let zp = src.path().join("evil.ferroplug");
        make_zip(&zp, TOML, &[("../evil.txt", b"x")]);
        assert!(install(&zp, dest.path()).is_err());
        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());
        let bad = src.path().join("bad.ferroplug");
        make_zip(&bad, "id = ", &[]);
        assert!(install(&bad, dest.path()).is_err());
    }

    #[test]
    fn missing_dir_is_empty() {
        let s = scan(Path::new("/nonexistent/ferrosurf/plugins"));
        assert!(s.found.is_empty() && s.broken.is_empty());
    }

    #[test]
    fn caps_sizes() {
        let big = [0u8; 100];
        assert!(read_capped(&big[..], 99, "x").is_err());
        assert_eq!(read_capped(&big[..], 100, "x").unwrap().len(), 100);
    }
}
