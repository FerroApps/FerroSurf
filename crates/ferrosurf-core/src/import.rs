//! Import bookmarks and history from other browsers installed on this machine.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::history::History;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowserKind {
    Chrome,
    Chromium,
    Edge,
    Brave,
    Vivaldi,
    Opera,
    Firefox,
    LibreWolf,
}

impl BrowserKind {
    pub fn is_firefox_family(self) -> bool {
        matches!(self, Self::Firefox | Self::LibreWolf)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::Chromium => "Chromium",
            Self::Edge => "Microsoft Edge",
            Self::Brave => "Brave",
            Self::Vivaldi => "Vivaldi",
            Self::Opera => "Opera",
            Self::Firefox => "Firefox",
            Self::LibreWolf => "LibreWolf",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DetectedBrowser {
    pub kind: BrowserKind,
    /// E.g. "Google Chrome — Default".
    pub name: String,
    pub profile_dir: PathBuf,
}

/// Base directories used for detection (injectable for tests).
struct Roots {
    home: PathBuf,
    /// `~/.config` (Linux).
    config: PathBuf,
    /// `%LOCALAPPDATA%` (Windows).
    local_appdata: PathBuf,
    /// `%APPDATA%` (Windows).
    appdata: PathBuf,
}

pub fn detect_browsers() -> Vec<DetectedBrowser> {
    let Some(dirs) = directories::BaseDirs::new() else { return Vec::new() };
    let roots = Roots {
        home: dirs.home_dir().to_path_buf(),
        config: dirs.config_dir().to_path_buf(),
        local_appdata: dirs.data_local_dir().to_path_buf(),
        appdata: dirs.data_dir().to_path_buf(),
    };
    detect_in(&roots, cfg!(windows))
}

fn detect_in(r: &Roots, windows: bool) -> Vec<DetectedBrowser> {
    let mut out = Vec::new();
    // (kind, user data dir, profiles live directly in it)
    let chromium: Vec<(BrowserKind, PathBuf, bool)> = if windows {
        vec![
            (BrowserKind::Chrome, r.local_appdata.join("Google/Chrome/User Data"), false),
            (BrowserKind::Chromium, r.local_appdata.join("Chromium/User Data"), false),
            (BrowserKind::Edge, r.local_appdata.join("Microsoft/Edge/User Data"), false),
            (BrowserKind::Brave, r.local_appdata.join("BraveSoftware/Brave-Browser/User Data"), false),
            (BrowserKind::Vivaldi, r.local_appdata.join("Vivaldi/User Data"), false),
            (BrowserKind::Opera, r.appdata.join("Opera Software/Opera Stable"), true),
        ]
    } else {
        vec![
            (BrowserKind::Chrome, r.config.join("google-chrome"), false),
            (BrowserKind::Chromium, r.config.join("chromium"), false),
            (BrowserKind::Edge, r.config.join("microsoft-edge"), false),
            (BrowserKind::Brave, r.config.join("BraveSoftware/Brave-Browser"), false),
            (BrowserKind::Vivaldi, r.config.join("vivaldi"), false),
            (BrowserKind::Opera, r.config.join("opera"), true),
        ]
    };
    for (kind, base, direct) in chromium {
        if direct {
            if has_chromium_data(&base) {
                out.push(DetectedBrowser { kind, name: kind.display_name().to_string(), profile_dir: base });
            }
            continue;
        }
        let Ok(rd) = fs::read_dir(&base) else { continue };
        let mut profiles: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                (n == "Default" || n.starts_with("Profile ")) && p.is_dir() && has_chromium_data(p)
            })
            .collect();
        profiles.sort();
        for p in profiles {
            let pname = p.file_name().unwrap().to_string_lossy().into_owned();
            out.push(DetectedBrowser { kind, name: format!("{} — {}", kind.display_name(), pname), profile_dir: p });
        }
    }

    let firefox_roots: Vec<(BrowserKind, PathBuf)> = if windows {
        vec![
            (BrowserKind::Firefox, r.appdata.join("Mozilla/Firefox")),
            (BrowserKind::LibreWolf, r.appdata.join("librewolf")),
        ]
    } else {
        vec![
            (BrowserKind::Firefox, r.home.join(".mozilla/firefox")),
            (BrowserKind::Firefox, r.home.join("snap/firefox/common/.mozilla/firefox")),
            (BrowserKind::Firefox, r.home.join(".var/app/org.mozilla.firefox/.mozilla/firefox")),
            (BrowserKind::LibreWolf, r.home.join(".librewolf")),
            (BrowserKind::LibreWolf, r.home.join(".var/app/io.gitlab.librewolf-community/.librewolf")),
        ]
    };
    for (kind, base) in firefox_roots {
        for (pname, dir) in firefox_profiles(&base) {
            if dir.join("places.sqlite").is_file() && !out.iter().any(|b| b.profile_dir == dir) {
                out.push(DetectedBrowser {
                    kind,
                    name: format!("{} — {}", kind.display_name(), pname),
                    profile_dir: dir,
                });
            }
        }
    }
    out
}

fn has_chromium_data(dir: &Path) -> bool {
    dir.join("Bookmarks").is_file() || dir.join("History").is_file()
}

/// Profiles of a Firefox-style base dir: from `profiles.ini`, plus any `*.default*` dirs
/// (and, on Windows, everything under `Profiles/`).
fn firefox_profiles(base: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(ini) = fs::read_to_string(base.join("profiles.ini")) {
        let mut name = String::new();
        let mut path = String::new();
        let mut relative = true;
        let mut in_profile = false;
        let flush = |name: &mut String, path: &mut String, relative: &mut bool, out: &mut Vec<(String, PathBuf)>| {
            if !path.is_empty() {
                let p = if *relative { base.join(&*path) } else { PathBuf::from(&*path) };
                let n = if name.is_empty() { path.clone() } else { name.clone() };
                out.push((n, p));
            }
            name.clear();
            path.clear();
            *relative = true;
        };
        for line in ini.lines().map(str::trim) {
            if line.starts_with('[') {
                if in_profile {
                    flush(&mut name, &mut path, &mut relative, &mut out);
                }
                in_profile = line.to_ascii_lowercase().starts_with("[profile");
            } else if in_profile && let Some((k, v)) = line.split_once('=') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "name" => name = v.trim().to_string(),
                    "path" => path = v.trim().to_string(),
                    "isrelative" => relative = v.trim() != "0",
                    _ => {},
                }
            }
        }
        if in_profile {
            flush(&mut name, &mut path, &mut relative, &mut out);
        }
    }
    let mut scan_dirs = vec![base.to_path_buf()];
    let profiles_sub = base.join("Profiles");
    if profiles_sub.is_dir() {
        scan_dirs.push(profiles_sub);
    }
    for d in scan_dirs {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut found: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        for p in found {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let under_profiles = d != base;
            if (under_profiles || n.contains(".default")) && !out.iter().any(|(_, o)| *o == p) {
                let display = n.split_once('.').map(|(_, s)| s.to_string()).unwrap_or_else(|| n.clone());
                out.push((display, p));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------------------

pub type ImportedBookmark = (Vec<String>, String, String);

pub fn read_bookmarks(b: &DetectedBrowser) -> anyhow::Result<Vec<ImportedBookmark>> {
    if b.kind.is_firefox_family() {
        read_firefox_bookmarks(&b.profile_dir.join("places.sqlite"))
    } else {
        read_chromium_bookmarks(&b.profile_dir.join("Bookmarks"))
    }
}

fn importable_url(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    !(l.is_empty() || l.starts_with("javascript:") || l.starts_with("place:"))
}

fn read_chromium_bookmarks(path: &Path) -> anyhow::Result<Vec<ImportedBookmark>> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let json: Value = serde_json::from_slice(&data).context("parsing Bookmarks JSON")?;
    let mut out = Vec::new();
    let roots = json.get("roots").and_then(Value::as_object).context("missing roots")?;
    for (key, label) in [("bookmark_bar", "Toolbar"), ("other", "Other"), ("synced", "Mobile")] {
        if let Some(node) = roots.get(key) {
            let mut stack = vec![label.to_string()];
            walk_chromium(node, &mut stack, &mut out);
        }
    }
    Ok(out)
}

fn walk_chromium(node: &Value, path: &mut Vec<String>, out: &mut Vec<ImportedBookmark>) {
    let Some(children) = node.get("children").and_then(Value::as_array) else { return };
    for c in children {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        match c.get("type").and_then(Value::as_str) {
            Some("url") => {
                let url = c.get("url").and_then(Value::as_str).unwrap_or("");
                if importable_url(url) {
                    let title = if name.trim().is_empty() { url.to_string() } else { name };
                    out.push((path.clone(), title, url.to_string()));
                }
            },
            Some("folder") => {
                path.push(name);
                walk_chromium(c, path, out);
                path.pop();
            },
            _ => {},
        }
    }
}

/// Copy a (possibly locked / WAL-mode) SQLite database to a temp location and open it.
struct TempDb {
    conn: Option<Connection>,
    files: Vec<PathBuf>,
}

impl TempDb {
    fn open(src: &Path) -> anyhow::Result<Self> {
        let id = uuid::Uuid::new_v4();
        let stem = src.file_name().and_then(|n| n.to_str()).unwrap_or("db");
        let dst = std::env::temp_dir().join(format!("ferrosurf-import-{id}-{stem}"));
        fs::copy(src, &dst).with_context(|| format!("copying {}", src.display()))?;
        let mut files = vec![dst.clone()];
        for suffix in ["-wal", "-shm"] {
            let mut s = src.as_os_str().to_owned();
            s.push(suffix);
            let s = PathBuf::from(s);
            if s.is_file() {
                let mut d = dst.as_os_str().to_owned();
                d.push(suffix);
                let d = PathBuf::from(d);
                if fs::copy(&s, &d).is_ok() {
                    files.push(d);
                }
            }
        }
        let conn = Connection::open_with_flags(&dst, OpenFlags::SQLITE_OPEN_READ_WRITE);
        match conn {
            Ok(conn) => Ok(Self { conn: Some(conn), files }),
            Err(e) => {
                for f in &files {
                    let _ = fs::remove_file(f);
                }
                Err(e.into())
            },
        }
    }

    fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("connection open")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.conn.take();
        for f in &self.files {
            let _ = fs::remove_file(f);
        }
    }
}

fn read_firefox_bookmarks(path: &Path) -> anyhow::Result<Vec<ImportedBookmark>> {
    let db = TempDb::open(path)?;
    let conn = db.conn();
    struct Node {
        parent: i64,
        kind: i64,
        title: String,
        url: Option<String>,
    }
    let mut nodes: HashMap<i64, Node> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT b.id, b.type, b.parent, b.title, p.url
         FROM moz_bookmarks b LEFT JOIN moz_places p ON p.id = b.fk",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Node {
                kind: r.get(1)?,
                parent: r.get(2)?,
                title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                url: r.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (id, n) = row?;
        nodes.insert(id, n);
    }
    let mut ids: Vec<i64> = nodes.keys().copied().collect();
    ids.sort_unstable();
    let mut out = Vec::new();
    'outer: for id in ids {
        let n = &nodes[&id];
        if n.kind != 1 {
            continue;
        }
        let Some(url) = n.url.as_deref().filter(|u| importable_url(u)) else { continue };
        // Walk up through folders.
        let mut path = Vec::new();
        let mut cur = n.parent;
        let mut guard = 0;
        while let Some(p) = nodes.get(&cur) {
            guard += 1;
            if guard > 64 {
                continue 'outer;
            }
            if p.parent == 0 {
                break; // the invisible root
            }
            let grand = nodes.get(&p.parent);
            if grand.is_some_and(|g| g.parent == 0) {
                // Top-level special folder.
                match p.title.as_str() {
                    "menu" => path.push("Menu".to_string()),
                    "toolbar" => path.push("Toolbar".to_string()),
                    "unfiled" => path.push("Other".to_string()),
                    "mobile" => path.push("Mobile".to_string()),
                    "tags" => continue 'outer,
                    other => path.push(other.to_string()),
                }
                break;
            }
            path.push(p.title.clone());
            cur = p.parent;
        }
        path.reverse();
        let title = if n.title.trim().is_empty() { url.to_string() } else { n.title.clone() };
        out.push((path, title, url.to_string()));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------

/// Milliseconds between 1601-01-01 and 1970-01-01.
const WEBKIT_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;

fn is_web_url(url: &str) -> bool {
    let l = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("file://") || l.starts_with("ftp://")
}

/// Import up to `max` most recently visited history entries. Returns how many were imported.
pub fn import_history(b: &DetectedBrowser, history: &History, max: usize) -> anyhow::Result<usize> {
    let firefox = b.kind.is_firefox_family();
    let (file, sql, to_ms): (&str, &str, fn(i64) -> i64) = if firefox {
        (
            "places.sqlite",
            "SELECT url, title, visit_count, typed, last_visit_date FROM moz_places
             WHERE last_visit_date IS NOT NULL AND visit_count > 0 ORDER BY last_visit_date DESC",
            |us| us / 1000,
        )
    } else {
        (
            "History",
            "SELECT url, title, visit_count, typed_count, last_visit_time FROM urls
             WHERE visit_count > 0 ORDER BY last_visit_time DESC",
            |t| t / 1000 - WEBKIT_EPOCH_OFFSET_MS,
        )
    };
    let db = TempDb::open(&b.profile_dir.join(file))?;
    let mut stmt = db.conn().prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut count = 0;
    while count < max {
        let Some(r) = rows.next()? else { break };
        let url: String = r.get(0)?;
        if !is_web_url(&url) {
            continue;
        }
        let title: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        let visits: i64 = r.get::<_, Option<i64>>(2)?.unwrap_or(1);
        let typed: i64 = r.get::<_, Option<i64>>(3)?.unwrap_or(0);
        let last: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
        history.import_entry(&url, &title, visits, typed, to_ms(last))?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    const CHROME_JSON: &str = r#"{
      "checksum": "x",
      "roots": {
        "bookmark_bar": {"type":"folder","name":"Bookmarks bar","children":[
          {"type":"url","name":"Servo","url":"https://servo.org/"},
          {"type":"folder","name":"Dev","children":[
            {"type":"url","name":"Rust","url":"https://rust-lang.org/"},
            {"type":"folder","name":"Deep","children":[{"type":"url","name":"","url":"https://deep.test/"}]}
          ]},
          {"type":"url","name":"Evil","url":"javascript:alert(1)"}
        ]},
        "other": {"type":"folder","name":"Other bookmarks","children":[
          {"type":"url","name":"Misc","url":"https://misc.test/"}]},
        "synced": {"type":"folder","name":"Mobile bookmarks","children":[]}
      },
      "version": 1
    }"#;

    #[test]
    fn chromium_bookmarks() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("Default/Bookmarks"), CHROME_JSON);
        let b = DetectedBrowser {
            kind: BrowserKind::Chrome,
            name: "Google Chrome — Default".into(),
            profile_dir: dir.path().join("Default"),
        };
        let bm = read_bookmarks(&b).unwrap();
        assert_eq!(bm.len(), 4);
        assert_eq!(bm[0], (vec!["Toolbar".to_string()], "Servo".to_string(), "https://servo.org/".to_string()));
        assert_eq!(bm[1].0, vec!["Toolbar", "Dev"]);
        assert_eq!(bm[2].0, vec!["Toolbar", "Dev", "Deep"]);
        assert_eq!(bm[2].1, "https://deep.test/");
        assert_eq!(bm[3].0, vec!["Other"]);
    }

    fn make_chromium_history(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE urls(id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR, visit_count INTEGER DEFAULT 0 NOT NULL,
                typed_count INTEGER DEFAULT 0 NOT NULL, last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);",
        )
        .unwrap();
        // 2024-01-01T00:00:00Z = 1704067200000 ms unix.
        let t = (1_704_067_200_000i64 + WEBKIT_EPOCH_OFFSET_MS) * 1000;
        for (url, title, vc, tc, lt) in [
            ("https://a.test/", "A", 5, 2, t),
            ("https://b.test/", "B", 1, 0, t + 1000),
            ("chrome://settings/", "Settings", 3, 0, t + 2000),
            ("https://zero.test/", "Zero", 0, 0, t + 3000),
        ] {
            c.execute(
                "INSERT INTO urls(url,title,visit_count,typed_count,last_visit_time) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![url, title, vc, tc, lt],
            )
            .unwrap();
        }
    }

    #[test]
    fn chromium_history() {
        let dir = tempfile::tempdir().unwrap();
        make_chromium_history(&dir.path().join("Default/History"));
        let b = DetectedBrowser {
            kind: BrowserKind::Brave,
            name: "Brave — Default".into(),
            profile_dir: dir.path().join("Default"),
        };
        let h = History::in_memory().unwrap();
        assert_eq!(import_history(&b, &h, 100).unwrap(), 2);
        let a = h.get("https://a.test/").unwrap().unwrap();
        assert_eq!((a.visit_count, a.typed_count, a.last_visit_ms), (5, 2, 1_704_067_200_000));
        assert_eq!(h.count().unwrap(), 2);
        // Limit applies to the most recent entries.
        let h2 = History::in_memory().unwrap();
        assert_eq!(import_history(&b, &h2, 1).unwrap(), 1);
        assert!(h2.get("https://b.test/").unwrap().is_some());
    }

    fn make_places(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR, visit_count INTEGER DEFAULT 0,
                typed INTEGER DEFAULT 0, last_visit_date INTEGER);
             CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER DEFAULT NULL, parent INTEGER, position INTEGER, title LONGVARCHAR);
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (1,2,0,'');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (2,2,1,'menu');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (3,2,1,'toolbar');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (4,2,1,'tags');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (5,2,1,'unfiled');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (6,2,3,'News');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (7,2,6,'Tech');
             INSERT INTO moz_bookmarks(id,type,parent,title) VALUES (8,2,4,'mytag');
             INSERT INTO moz_places(id,url,title,visit_count,typed,last_visit_date) VALUES (1,'https://servo.org/','Servo',4,1,1704067200000000);
             INSERT INTO moz_places(id,url,title,visit_count,typed,last_visit_date) VALUES (2,'https://lwn.net/','LWN',2,0,1704067300000000);
             INSERT INTO moz_places(id,url,title,visit_count,typed,last_visit_date) VALUES (3,'place:type=6','Recent',0,0,NULL);
             INSERT INTO moz_places(id,url,title,visit_count,typed,last_visit_date) VALUES (4,'https://menu.test/',NULL,0,0,NULL);
             INSERT INTO moz_places(id,url,title,visit_count,typed,last_visit_date) VALUES (5,'about:config','Config',1,0,1704067400000000);
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (10,1,1,3,'Servo (toolbar)');
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (11,1,2,7,'LWN');
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (12,1,3,2,'Smart');
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (13,1,4,2,NULL);
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (14,1,1,8,'tagged');
             INSERT INTO moz_bookmarks(id,type,fk,parent,title) VALUES (15,1,2,5,'Unfiled one');",
        )
        .unwrap();
    }

    #[test]
    fn firefox_bookmarks_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let prof = dir.path().join("abc.default-release");
        make_places(&prof.join("places.sqlite"));
        let b = DetectedBrowser { kind: BrowserKind::Firefox, name: "Firefox — x".into(), profile_dir: prof };
        let bm = read_bookmarks(&b).unwrap();
        assert_eq!(bm.len(), 4, "{bm:?}");
        assert_eq!(bm[0], (vec!["Toolbar".into()], "Servo (toolbar)".into(), "https://servo.org/".into()));
        assert_eq!(bm[1].0, vec!["Toolbar", "News", "Tech"]);
        assert_eq!(bm[2], (vec!["Menu".into()], "https://menu.test/".into(), "https://menu.test/".into()));
        assert_eq!(bm[3].0, vec!["Other"]);

        let h = History::in_memory().unwrap();
        assert_eq!(import_history(&b, &h, 100).unwrap(), 2);
        let e = h.get("https://servo.org/").unwrap().unwrap();
        assert_eq!((e.visit_count, e.typed_count, e.last_visit_ms), (4, 1, 1_704_067_200_000));
        assert!(h.get("about:config").unwrap().is_none());
    }

    #[test]
    fn detection_linux_and_windows() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let r = Roots {
            home: home.clone(),
            config: home.join(".config"),
            local_appdata: home.join("Local"),
            appdata: home.join("Roaming"),
        };
        write(&home.join(".config/google-chrome/Default/Bookmarks"), "{}");
        write(&home.join(".config/google-chrome/Profile 2/History"), "");
        write(&home.join(".config/google-chrome/System Profile/History"), "");
        write(&home.join(".config/opera/Bookmarks"), "{}");
        write(
            &home.join(".mozilla/firefox/profiles.ini"),
            "[Profile0]\nName=default-release\nIsRelative=1\nPath=xyz.default-release\n\n[General]\nStartWithLastProfile=1\n",
        );
        write(&home.join(".mozilla/firefox/xyz.default-release/places.sqlite"), "");
        write(&home.join(".mozilla/firefox/old.default/places.sqlite"), "");
        let found = detect_in(&r, false);
        let names: Vec<&str> = found.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Google Chrome — Default",
                "Google Chrome — Profile 2",
                "Opera",
                "Firefox — default-release",
                "Firefox — default"
            ]
        );
        assert_eq!(found[3].kind, BrowserKind::Firefox);

        write(&home.join("Local/Microsoft/Edge/User Data/Default/Bookmarks"), "{}");
        write(&home.join("Roaming/Opera Software/Opera Stable/History"), "");
        write(&home.join("Roaming/Mozilla/Firefox/Profiles/q1.default/places.sqlite"), "");
        write(&home.join("Roaming/librewolf/Profiles/z9.default-default/places.sqlite"), "");
        let found = detect_in(&r, true);
        let names: Vec<&str> = found.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Microsoft Edge — Default", "Opera", "Firefox — default", "LibreWolf — default-default"]
        );
    }
}
