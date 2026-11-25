//! Download manager: background threads, resumable HTTP downloads, `data:` URLs.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

pub type DownloadId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    Queued,
    InProgress,
    Paused,
    Completed,
    Failed(String),
    Cancelled,
}

impl DownloadState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::InProgress)
    }
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: DownloadId,
    pub url: String,
    pub file_name: String,
    pub path: PathBuf,
    pub total_bytes: Option<u64>,
    pub received_bytes: u64,
    pub state: DownloadState,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub mime: Option<String>,
    #[serde(default)]
    pub bytes_per_sec: f64,
}

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

struct Worker {
    ctl: AtomicU8,
    done: AtomicBool,
}

impl Worker {
    fn new() -> Arc<Self> {
        Arc::new(Self { ctl: AtomicU8::new(RUN), done: AtomicBool::new(false) })
    }
    fn finished() -> Arc<Self> {
        Arc::new(Self { ctl: AtomicU8::new(RUN), done: AtomicBool::new(true) })
    }
}

struct Entry {
    item: DownloadItem,
    dir: PathBuf,
    suggested: Option<String>,
    headers: Vec<(String, String)>,
    worker: Arc<Worker>,
    /// True once `item.path` is the real, reserved destination.
    resolved: bool,
}

struct Inner {
    entries: Vec<Entry>,
    next_id: DownloadId,
    history_path: Option<PathBuf>,
}

struct Shared {
    inner: Mutex<Inner>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
pub struct DownloadManager {
    shared: Arc<Shared>,
}

fn part_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn persist(inner: &Inner) {
    if let Some(p) = &inner.history_path {
        let items: Vec<&DownloadItem> = inner.entries.iter().map(|e| &e.item).collect();
        if let Err(e) = crate::storage::write_json(p, &items) {
            log::error!("Failed to save download list: {e}");
        }
    }
}

impl DownloadManager {
    pub fn new(history_path: Option<PathBuf>, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        let mut entries = Vec::new();
        let mut next_id = 1;
        if let Some(p) = &history_path {
            let items: Vec<DownloadItem> = crate::storage::read_json_or_default(p);
            for mut item in items {
                next_id = next_id.max(item.id + 1);
                if item.state.is_active() {
                    item.state = if part_path(&item.path).exists() {
                        DownloadState::Paused
                    } else {
                        DownloadState::Failed("Interrupted".into())
                    };
                }
                let dir = item.path.parent().map(Path::to_path_buf).unwrap_or_default();
                entries.push(Entry {
                    item,
                    dir,
                    suggested: None,
                    headers: Vec::new(),
                    worker: Worker::finished(),
                    resolved: true,
                });
            }
        }
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner { entries, next_id, history_path }),
                on_change,
            }),
        }
    }

    pub fn start(
        &self,
        url: &str,
        dir: &Path,
        suggested_name: Option<&str>,
        headers: Vec<(String, String)>,
    ) -> DownloadId {
        let suggested = suggested_name
            .map(sanitize_file_name)
            .filter(|s| !s.is_empty());
        let guess = suggested.clone().unwrap_or_else(|| file_name_from_url(url));
        let worker = Worker::new();
        let id = {
            let mut g = self.shared.inner.lock();
            let id = g.next_id;
            g.next_id += 1;
            g.entries.push(Entry {
                item: DownloadItem {
                    id,
                    url: url.to_string(),
                    file_name: guess.clone(),
                    path: dir.join(&guess),
                    total_bytes: None,
                    received_bytes: 0,
                    state: DownloadState::Queued,
                    started_ms: crate::now_ms(),
                    finished_ms: None,
                    mime: None,
                    bytes_per_sec: 0.0,
                },
                dir: dir.to_path_buf(),
                suggested,
                headers,
                worker: worker.clone(),
                resolved: false,
            });
            persist(&g);
            id
        };
        self.spawn(id, worker, None);
        self.notify();
        id
    }

    fn notify(&self) {
        (self.shared.on_change)();
    }

    fn spawn(&self, id: DownloadId, me: Arc<Worker>, prev: Option<Arc<Worker>>) {
        let ctx = Ctx { shared: self.shared.clone(), id, me };
        std::thread::Builder::new()
            .name(format!("download-{id}"))
            .spawn(move || ctx.run(prev))
            .expect("spawn download thread");
    }

    pub fn cancel(&self, id: DownloadId) {
        {
            let mut g = self.shared.inner.lock();
            let Some(e) = g.entries.iter_mut().find(|e| e.item.id == id) else { return };
            if e.item.state.is_finished() {
                return;
            }
            e.worker.ctl.store(CANCEL, Ordering::SeqCst);
            e.item.state = DownloadState::Cancelled;
            e.item.finished_ms = Some(crate::now_ms());
            e.item.bytes_per_sec = 0.0;
            if e.resolved {
                let _ = fs::remove_file(part_path(&e.item.path));
            }
            persist(&g);
        }
        self.notify();
    }

    /// Stop the transfer but keep the `.part` file for [`resume`](Self::resume).
    pub fn pause(&self, id: DownloadId) {
        {
            let mut g = self.shared.inner.lock();
            let Some(e) = g.entries.iter_mut().find(|e| e.item.id == id) else { return };
            if !e.item.state.is_active() {
                return;
            }
            e.worker.ctl.store(PAUSE, Ordering::SeqCst);
            e.item.state = DownloadState::Paused;
            e.item.bytes_per_sec = 0.0;
            persist(&g);
        }
        self.notify();
    }

    pub fn resume(&self, id: DownloadId) {
        let spawned = {
            let mut g = self.shared.inner.lock();
            let Some(e) = g.entries.iter_mut().find(|e| e.item.id == id) else { return };
            let resumable = match e.item.state {
                DownloadState::Paused => true,
                DownloadState::Failed(_) => e.resolved && part_path(&e.item.path).exists(),
                _ => false,
            };
            if !resumable {
                return;
            }
            let worker = Worker::new();
            let prev = std::mem::replace(&mut e.worker, worker.clone());
            e.item.state = DownloadState::Queued;
            e.item.finished_ms = None;
            persist(&g);
            (worker, prev)
        };
        self.spawn(id, spawned.0, Some(spawned.1));
        self.notify();
    }

    /// Restart a failed or cancelled download from scratch.
    pub fn retry(&self, id: DownloadId) {
        let spawned = {
            let mut g = self.shared.inner.lock();
            let Some(e) = g.entries.iter_mut().find(|e| e.item.id == id) else { return };
            if !matches!(e.item.state, DownloadState::Failed(_) | DownloadState::Cancelled) {
                return;
            }
            let worker = Worker::new();
            let prev = std::mem::replace(&mut e.worker, worker.clone());
            if e.resolved {
                let _ = fs::remove_file(part_path(&e.item.path));
            }
            e.resolved = false;
            let guess = e.suggested.clone().unwrap_or_else(|| file_name_from_url(&e.item.url));
            e.item.path = e.dir.join(&guess);
            e.item.file_name = guess;
            e.item.state = DownloadState::Queued;
            e.item.received_bytes = 0;
            e.item.total_bytes = None;
            e.item.finished_ms = None;
            e.item.started_ms = crate::now_ms();
            persist(&g);
            (worker, prev)
        };
        self.spawn(id, spawned.0, Some(spawned.1));
        self.notify();
    }

    /// Remove from the list (not from disk, except for unfinished partial files).
    pub fn remove(&self, id: DownloadId) {
        {
            let mut g = self.shared.inner.lock();
            let Some(pos) = g.entries.iter().position(|e| e.item.id == id) else { return };
            let e = g.entries.remove(pos);
            if e.item.state != DownloadState::Completed {
                e.worker.ctl.store(CANCEL, Ordering::SeqCst);
                if e.resolved {
                    let _ = fs::remove_file(part_path(&e.item.path));
                }
            }
            persist(&g);
        }
        self.notify();
    }

    pub fn clear_finished(&self) {
        {
            let mut g = self.shared.inner.lock();
            g.entries.retain(|e| !e.item.state.is_finished());
            persist(&g);
        }
        self.notify();
    }

    pub fn items(&self) -> Vec<DownloadItem> {
        self.shared.inner.lock().entries.iter().map(|e| e.item.clone()).collect()
    }

    pub fn get(&self, id: DownloadId) -> Option<DownloadItem> {
        self.shared.inner.lock().entries.iter().find(|e| e.item.id == id).map(|e| e.item.clone())
    }

    pub fn active_count(&self) -> usize {
        self.shared.inner.lock().entries.iter().filter(|e| e.item.state.is_active()).count()
    }

    /// Combined progress (0..=1) of active downloads with a known size.
    pub fn overall_progress(&self) -> Option<f32> {
        let g = self.shared.inner.lock();
        let (mut got, mut total) = (0u64, 0u64);
        for e in g.entries.iter().filter(|e| e.item.state.is_active()) {
            if let Some(t) = e.item.total_bytes.filter(|t| *t > 0) {
                got += e.item.received_bytes.min(t);
                total += t;
            }
        }
        (total > 0).then(|| got as f32 / total as f32)
    }

    /// Open a downloaded file with the system default application.
    pub fn open_file(path: &Path) -> std::io::Result<()> {
        use std::process::Command;
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "start", ""]).arg(path);
            c
        };
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = Command::new("open");
            c.arg(path);
            c
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let mut cmd = {
            let mut c = Command::new("xdg-open");
            c.arg(path);
            c
        };
        cmd.spawn().map(|_| ())
    }

    /// Reveal a file in the platform file manager.
    pub fn show_in_folder(path: &Path) -> std::io::Result<()> {
        use std::process::Command;
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("explorer");
            c.arg(format!("/select,{}", path.display()));
            c
        };
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = Command::new("open");
            c.arg("-R").arg(path);
            c
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let mut cmd = {
            let mut c = Command::new("xdg-open");
            c.arg(path.parent().unwrap_or(Path::new(".")));
            c
        };
        cmd.spawn().map(|_| ())
    }
}

// ---------------------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------------------

struct Ctx {
    shared: Arc<Shared>,
    id: DownloadId,
    me: Arc<Worker>,
}

struct Snapshot {
    url: String,
    headers: Vec<(String, String)>,
    dir: PathBuf,
    suggested: Option<String>,
    resolved: bool,
    path: PathBuf,
}

enum Outcome {
    Done,
    Stopped,
}

struct DoneGuard(Arc<Worker>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::SeqCst);
    }
}

impl Ctx {
    fn notify(&self) {
        (self.shared.on_change)();
    }

    fn ctl(&self) -> u8 {
        self.me.ctl.load(Ordering::SeqCst)
    }

    /// Apply `f` to our entry; `None` if the entry was removed or taken over by a newer worker.
    fn with<R>(&self, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut g = self.shared.inner.lock();
        let e = g
            .entries
            .iter_mut()
            .find(|e| e.item.id == self.id && Arc::ptr_eq(&e.worker, &self.me))?;
        Some(f(e))
    }

    fn persist(&self) {
        persist(&self.shared.inner.lock());
    }

    fn transition(&self, state: DownloadState) {
        let changed = self.with(|e| {
            if e.item.state == DownloadState::Cancelled || e.item.state == DownloadState::Paused {
                return false;
            }
            if state.is_finished() {
                e.item.finished_ms = Some(crate::now_ms());
                e.item.bytes_per_sec = 0.0;
            }
            e.item.state = state;
            true
        });
        if changed == Some(true) {
            self.persist();
            self.notify();
        }
    }

    fn run(self, prev: Option<Arc<Worker>>) {
        let _guard = DoneGuard(self.me.clone());
        if let Some(p) = prev {
            while !p.done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        match self.download() {
            Ok(Outcome::Done) | Ok(Outcome::Stopped) => {},
            Err(e) => {
                if self.ctl() == RUN {
                    self.transition(DownloadState::Failed(format!("{e:#}")));
                }
            },
        }
        if self.ctl() == CANCEL
            && let Some(path) = self.with(|e| e.resolved.then(|| e.item.path.clone())).flatten()
        {
            let _ = fs::remove_file(part_path(&path));
        }
    }

    fn snapshot(&self) -> Option<Snapshot> {
        self.with(|e| {
            if e.item.state == DownloadState::Queued {
                e.item.state = DownloadState::InProgress;
            }
            Snapshot {
                url: e.item.url.clone(),
                headers: e.headers.clone(),
                dir: e.dir.clone(),
                suggested: e.suggested.clone(),
                resolved: e.resolved,
                path: e.item.path.clone(),
            }
        })
    }

    /// Choose a unique destination for `name`, reserve it by creating its `.part` file.
    fn reserve(&self, name: &str) -> anyhow::Result<File> {
        let mut g = self.shared.inner.lock();
        let taken: Vec<PathBuf> =
            g.entries.iter().filter(|e| e.item.id != self.id && e.resolved).map(|e| e.item.path.clone()).collect();
        let Some(e) = g.entries.iter_mut().find(|e| e.item.id == self.id) else {
            anyhow::bail!("download removed");
        };
        fs::create_dir_all(&e.dir)?;
        let path = unique_path(&e.dir, name, |p| p.exists() || part_path(p).exists() || taken.iter().any(|t| t == p));
        let file = File::create(part_path(&path))?;
        e.item.file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        e.item.path = path;
        e.resolved = true;
        Ok(file)
    }

    fn download(&self) -> anyhow::Result<Outcome> {
        let Some(snap) = self.snapshot() else { return Ok(Outcome::Stopped) };
        if self.ctl() != RUN {
            return Ok(Outcome::Stopped);
        }
        self.persist();
        self.notify();

        if snap.url.len() >= 5 && snap.url[..5].eq_ignore_ascii_case("data:") {
            return self.download_data(&snap);
        }
        self.download_http(&snap)
    }

    fn download_data(&self, snap: &Snapshot) -> anyhow::Result<Outcome> {
        let (mime, bytes) = parse_data_url(&snap.url)?;
        let mut file = if snap.resolved {
            File::create(part_path(&snap.path))?
        } else {
            let name = snap.suggested.clone().unwrap_or_else(|| default_name_for_mime(&mime));
            self.reserve(&name)?
        };
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        self.with(|e| {
            e.item.total_bytes = Some(bytes.len() as u64);
            e.item.received_bytes = bytes.len() as u64;
            e.item.mime = Some(mime.clone());
        });
        self.finalize()
    }

    fn finalize(&self) -> anyhow::Result<Outcome> {
        if self.ctl() != RUN {
            return Ok(Outcome::Stopped);
        }
        let Some(path) = self.with(|e| e.item.path.clone()) else { return Ok(Outcome::Stopped) };
        let part = part_path(&path);
        let mut dest = path.clone();
        if dest.exists() {
            let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
            let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            dest = unique_path(&dir, &name, |p| p.exists());
        }
        fs::rename(&part, &dest)?;
        self.with(|e| {
            e.item.path = dest.clone();
            e.item.file_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if e.item.total_bytes.is_none() {
                e.item.total_bytes = Some(e.item.received_bytes);
            }
        });
        self.transition(DownloadState::Completed);
        Ok(Outcome::Done)
    }

    fn download_http(&self, snap: &Snapshot) -> anyhow::Result<Outcome> {
        let agent = make_agent(&snap.url);
        let mut offset = if snap.resolved {
            fs::metadata(part_path(&snap.path)).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let mut attempts = 0;
        let resp = loop {
            attempts += 1;
            let mut req = agent.get(&snap.url).header("Accept-Encoding", "identity");
            for (k, v) in &snap.headers {
                req = req.header(k.as_str(), v.as_str());
            }
            if offset > 0 {
                req = req.header("Range", format!("bytes={offset}-"));
            }
            let resp = req.call()?;
            let status = resp.status().as_u16();
            if status == 416 && offset > 0 && attempts < 2 {
                offset = 0;
                continue;
            }
            if status >= 400 {
                anyhow::bail!("HTTP {status}");
            }
            break resp;
        };
        if self.ctl() != RUN {
            return Ok(Outcome::Stopped);
        }
        let status = resp.status().as_u16();
        let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
        let content_length = header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
        let mime = header("content-type")
            .map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
            .filter(|m| !m.is_empty());

        let (mut file, total, mut received) = if status == 206 && offset > 0 {
            let total = header("content-range")
                .and_then(|r| parse_content_range(&r));
            match total {
                Some((start, _)) if start != offset => anyhow::bail!("server returned unexpected range"),
                _ => {},
            }
            let total = total.and_then(|t| t.1).or(content_length.map(|l| l + offset));
            let f = OpenOptions::new().append(true).open(part_path(&snap.path))?;
            (f, total, offset)
        } else {
            // Fresh download (or the server ignored our Range request: start over).
            let f = if snap.resolved {
                File::create(part_path(&snap.path))?
            } else {
                let name = snap
                    .suggested
                    .clone()
                    .or_else(|| header("content-disposition").and_then(|cd| parse_content_disposition(&cd)))
                    .unwrap_or_else(|| file_name_from_url(&snap.url));
                self.reserve(&name)?
            };
            (f, content_length, 0)
        };
        let _ = &snap.dir;
        self.with(|e| {
            e.item.total_bytes = total;
            e.item.received_bytes = received;
            if mime.is_some() {
                e.item.mime = mime.clone();
            }
        });
        self.notify();

        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut last_tick = Instant::now();
        let mut tick_bytes = 0u64;
        let mut speed = 0.0f64;
        loop {
            match self.ctl() {
                PAUSE => {
                    file.flush()?;
                    self.with(|e| {
                        e.item.received_bytes = received;
                        e.item.bytes_per_sec = 0.0;
                    });
                    self.persist();
                    self.notify();
                    return Ok(Outcome::Stopped);
                },
                CANCEL => return Ok(Outcome::Stopped),
                _ => {},
            }
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) if self.ctl() != RUN => {
                    log::debug!("download interrupted: {e}");
                    continue;
                },
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            received += n as u64;
            tick_bytes += n as u64;
            let elapsed = last_tick.elapsed();
            if elapsed >= Duration::from_millis(100) {
                let inst = tick_bytes as f64 / elapsed.as_secs_f64();
                speed = if speed == 0.0 { inst } else { speed * 0.6 + inst * 0.4 };
                last_tick = Instant::now();
                tick_bytes = 0;
                if self.with(|e| {
                    e.item.received_bytes = received;
                    e.item.bytes_per_sec = speed;
                })
                .is_none()
                {
                    return Ok(Outcome::Stopped);
                }
                self.notify();
            }
        }
        file.flush()?;
        file.sync_all()?;
        drop(file);
        if let Some(t) = total
            && received < t
        {
            anyhow::bail!("connection closed early ({received} of {t} bytes)");
        }
        self.with(|e| {
            e.item.received_bytes = received;
            e.item.total_bytes = Some(total.unwrap_or(received));
        });
        self.finalize()
    }
}

fn is_loopback(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h == "::1")
}

fn make_agent(url: &str) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(30)));
    if is_loopback(url) {
        b = b.proxy(None);
    }
    ureq::Agent::new_with_config(b.build())
}

// ---------------------------------------------------------------------------------------
// File name logic
// ---------------------------------------------------------------------------------------

const MAX_NAME_CHARS: usize = 200;

/// Make `name` safe as a file name on Windows and Linux.
pub fn sanitize_file_name(name: &str) -> String {
    // Only keep the final component, in case a path was supplied.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let mut s = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.chars().count() > MAX_NAME_CHARS {
        let (stem, ext) = split_ext(&s);
        let ext: String = ext.chars().take(20).collect();
        let keep = MAX_NAME_CHARS - ext.chars().count();
        let stem: String = stem.chars().take(keep).collect();
        s = format!("{stem}{ext}");
        s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    }
    let stem_upper = s.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(stem_upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem_upper.starts_with("COM") || stem_upper.starts_with("LPT"))
            && stem_upper.len() == 4
            && matches!(stem_upper.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        s = format!("_{s}");
    }
    if s.is_empty() { "download".to_string() } else { s }
}

/// Split into (stem, ".ext"); ext is empty when there is none (leading dots don't count).
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// `dir/name`, or `dir/stem (n).ext` for the first n >= 1 not reported as taken.
pub fn unique_path(dir: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = split_ext(name);
    for n in 1u32.. {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Extract a file name from a `Content-Disposition` header (`filename*` preferred).
pub fn parse_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for param in split_params(value).into_iter().skip(1) {
        let Some((k, v)) = param.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            if let Some(n) = decode_ext_value(v) {
                extended = Some(n);
            }
        } else if k == "filename" {
            plain = Some(unquote(v));
        }
    }
    let name = extended.or(plain)?;
    let name = sanitize_file_name(&name);
    (!name.is_empty()).then_some(name)
}

fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
        } else if quoted && c == '\\' {
            cur.push(c);
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c == ';' && !quoted {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        v.to_string()
    }
}

/// RFC 5987 `charset'lang'percent-encoded`.
fn decode_ext_value(v: &str) -> Option<String> {
    let v = unquote(v);
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.to_ascii_lowercase();
    let _lang = parts.next()?;
    let data = parts.next()?;
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(data).collect();
    match charset.as_str() {
        "utf-8" | "utf8" | "" => String::from_utf8(bytes).ok(),
        "iso-8859-1" | "latin1" => Some(bytes.iter().map(|b| *b as char).collect()),
        _ => None,
    }
}

/// Last path segment of the URL, percent-decoded and sanitized, or `"download"`.
pub fn file_name_from_url(url: &str) -> String {
    let seg = url::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
        .unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(&seg).decode_utf8_lossy().into_owned();
    let name = sanitize_file_name(&decoded);
    if decoded.trim().is_empty() { "download".to_string() } else { name }
}

fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    // "bytes 100-199/200" or "bytes 100-199/*"
    let rest = v.trim().strip_prefix("bytes")?.trim();
    let (range, total) = rest.split_once('/')?;
    let start = range.split('-').next()?.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

fn default_name_for_mime(mime: &str) -> String {
    let ext = match mime {
        "text/plain" => ".txt",
        "text/html" => ".html",
        "text/css" => ".css",
        "text/csv" => ".csv",
        "application/json" => ".json",
        "application/pdf" => ".pdf",
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/svg+xml" => ".svg",
        _ => "",
    };
    format!("download{ext}")
}

/// Parse a `data:` URL into (mime, bytes).
pub fn parse_data_url(url: &str) -> anyhow::Result<(String, Vec<u8>)> {
    let rest = url
        .get(5..)
        .ok_or_else(|| anyhow::anyhow!("invalid data URL"))?;
    let (meta, data) = rest.split_once(',').ok_or_else(|| anyhow::anyhow!("invalid data URL"))?;
    let mut base64 = false;
    let mut mime = String::new();
    for (i, part) in meta.split(';').enumerate() {
        let part = part.trim();
        if part.eq_ignore_ascii_case("base64") {
            base64 = true;
        } else if i == 0 && part.contains('/') {
            mime = part.to_ascii_lowercase();
        }
    }
    if mime.is_empty() {
        mime = "text/plain".to_string();
    }
    let raw: Vec<u8> = percent_encoding::percent_decode_str(data).collect();
    let bytes = if base64 {
        let cleaned: Vec<u8> = raw.into_iter().filter(|b| !b.is_ascii_whitespace() && *b != b'=').collect();
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(cleaned)
            .map_err(|e| anyhow::anyhow!("invalid base64 in data URL: {e}"))?
    } else {
        raw
    };
    Ok((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_file_name("a/b\\c:d*e?f\"g<h>i|j.txt"), "cdefghij.txt");
        assert_eq!(sanitize_file_name("a:b*c?.txt"), "abc.txt");
        assert_eq!(sanitize_file_name("  ..name.. "), "name");
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_file_name("com1.tar.gz"), "_com1.tar.gz");
        assert_eq!(sanitize_file_name("console.txt"), "console.txt");
        assert_eq!(sanitize_file_name("..."), "download");
        assert_eq!(sanitize_file_name("a\u{0}b\nc"), "abc");
        let long = format!("{}.pdf", "x".repeat(500));
        let s = sanitize_file_name(&long);
        assert_eq!(s.chars().count(), 200);
        assert!(s.ends_with(".pdf"));
    }

    #[test]
    fn content_disposition() {
        let p = parse_content_disposition;
        assert_eq!(p("attachment; filename=\"a b.txt\"").as_deref(), Some("a b.txt"));
        assert_eq!(p("attachment; filename=plain.zip").as_deref(), Some("plain.zip"));
        assert_eq!(p("attachment; filename=\"semi;colon.txt\"").as_deref(), Some("semi;colon.txt"));
        assert_eq!(
            p("attachment; filename=\"fallback.txt\"; filename*=UTF-8''%E2%82%AC%20rates.txt").as_deref(),
            Some("€ rates.txt")
        );
        assert_eq!(p("attachment; filename*=iso-8859-1'en'%E9.txt").as_deref(), Some("é.txt"));
        assert_eq!(p("attachment; filename=\"../../etc/passwd\"").as_deref(), Some("passwd"));
        assert_eq!(p("inline").as_deref(), None);
        assert_eq!(p("attachment; filename=\"q\\\"x.txt\"").as_deref(), Some("qx.txt"));
    }

    #[test]
    fn url_names() {
        assert_eq!(file_name_from_url("https://x.test/a/b/file%20one.tar.gz?x=1"), "file one.tar.gz");
        assert_eq!(file_name_from_url("https://x.test/"), "download");
        assert_eq!(file_name_from_url("https://x.test"), "download");
        assert_eq!(file_name_from_url("not a url"), "download");
    }

    #[test]
    fn uniqueness() {
        let dir = tempfile::tempdir().unwrap();
        let exists = |p: &Path| p.exists();
        let a = unique_path(dir.path(), "f.txt", exists);
        assert_eq!(a.file_name().unwrap(), "f.txt");
        fs::write(&a, b"1").unwrap();
        let b = unique_path(dir.path(), "f.txt", exists);
        assert_eq!(b.file_name().unwrap(), "f (1).txt");
        fs::write(&b, b"1").unwrap();
        assert_eq!(unique_path(dir.path(), "f.txt", exists).file_name().unwrap(), "f (2).txt");
        fs::write(dir.path().join("noext"), b"1").unwrap();
        assert_eq!(unique_path(dir.path(), "noext", exists).file_name().unwrap(), "noext (1)");
    }

    #[test]
    fn data_urls_parse() {
        let (m, b) = parse_data_url("data:text/plain;base64,aGVsbG8=").unwrap();
        assert_eq!((m.as_str(), b.as_slice()), ("text/plain", &b"hello"[..]));
        let (_, b) = parse_data_url("data:,Hello%20World%21").unwrap();
        assert_eq!(b, b"Hello World!");
        assert!(parse_data_url("data:nocomma").is_err());
    }

    fn manager(history: Option<PathBuf>) -> (DownloadManager, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let m = DownloadManager::new(
            history,
            Arc::new(move || {
                n2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (m, n)
    }

    fn wait_for(m: &DownloadManager, id: DownloadId, pred: impl Fn(&DownloadItem) -> bool) -> DownloadItem {
        let start = Instant::now();
        loop {
            let it = m.get(id).expect("item exists");
            if pred(&it) {
                return it;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "timeout; state = {:?}", it.state);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn data_url_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("downloads.json");
        let (m, changes) = manager(Some(hist.clone()));
        let id = m.start("data:text/plain;base64,aGVsbG8gd29ybGQ=", dir.path(), Some("greet:ing.txt"), vec![]);
        let it = wait_for(&m, id, |i| i.state.is_finished());
        assert_eq!(it.state, DownloadState::Completed);
        assert_eq!(it.file_name, "greeting.txt");
        assert_eq!(fs::read(&it.path).unwrap(), b"hello world");
        assert_eq!(it.total_bytes, Some(11));
        assert!(!part_path(&it.path).exists());
        assert!(changes.load(Ordering::SeqCst) > 0);

        // Same name again gets a suffix.
        let id2 = m.start("data:,second", dir.path(), Some("greeting.txt"), vec![]);
        let it2 = wait_for(&m, id2, |i| i.state.is_finished());
        assert_eq!(it2.file_name, "greeting (1).txt");
        assert_eq!(fs::read(&it2.path).unwrap(), b"second");

        // No suggestion: default name by mime.
        let id3 = m.start("data:image/png;base64,iVBORw0KGgo=", dir.path(), None, vec![]);
        assert_eq!(wait_for(&m, id3, |i| i.state.is_finished()).file_name, "download.png");

        // Persistence across restarts.
        let (m2, _) = manager(Some(hist));
        assert_eq!(m2.items().len(), 3);
        assert!(m2.items().iter().all(|i| i.state == DownloadState::Completed));
        let id4 = m2.start("data:,x", dir.path(), Some("x.txt"), vec![]);
        assert!(id4 > id3);
        wait_for(&m2, id4, |i| i.state.is_finished());
        m2.remove(id4);
        m2.clear_finished();
        assert!(m2.items().is_empty());
        assert_eq!(m2.active_count(), 0);
        assert!(m2.overall_progress().is_none());
    }

    struct Server {
        port: u16,
        range_requests: Arc<AtomicUsize>,
    }

    /// Tiny HTTP server. `/file` serves `data` (with Content-Disposition `cd` if given);
    /// bytes are sent in 64 KiB chunks with `delay_ms` between them.
    fn spawn_server(data: Vec<u8>, honor_range: bool, cd: Option<&'static str>, delay_ms: u64) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let range_requests = Arc::new(AtomicUsize::new(0));
        let rr = range_requests.clone();
        let data = Arc::new(data);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let data = data.clone();
                let rr = rr.clone();
                std::thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut stream = stream;
                    let mut range: Option<u64> = None;
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        let l = line.to_ascii_lowercase();
                        if let Some(v) = l.strip_prefix("range: bytes=") {
                            range = v.trim().trim_end_matches('-').parse().ok();
                        }
                    }
                    if !first.contains("/file") {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    let total = data.len() as u64;
                    let (head, body) = match range {
                        Some(start) if honor_range => {
                            rr.fetch_add(1, Ordering::SeqCst);
                            (
                                format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\n",
                                    total - 1,
                                    total - start
                                ),
                                &data[start as usize..],
                            )
                        },
                        _ => (format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n"), &data[..]),
                    };
                    let mut head = head;
                    head.push_str("Content-Type: application/octet-stream\r\nConnection: close\r\n");
                    if let Some(cd) = cd {
                        head.push_str(&format!("Content-Disposition: {cd}\r\n"));
                    }
                    head.push_str("\r\n");
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for chunk in body.chunks(64 * 1024) {
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        if delay_ms > 0 {
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                    }
                });
            }
        });
        Server { port, range_requests }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
    }

    #[test]
    fn http_download_with_content_disposition() {
        let data = pattern(300_000);
        let srv = spawn_server(data.clone(), true, Some("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.bin"), 0);
        let dir = tempfile::tempdir().unwrap();
        let (m, _) = manager(None);
        let url = format!("http://127.0.0.1:{}/file", srv.port);
        let id = m.start(&url, dir.path(), None, vec![("X-Test".into(), "1".into())]);
        let it = wait_for(&m, id, |i| i.state.is_finished());
        assert_eq!(it.state, DownloadState::Completed, "{it:?}");
        assert_eq!(it.file_name, "résumé.bin");
        assert_eq!(it.total_bytes, Some(300_000));
        assert_eq!(it.mime.as_deref(), Some("application/octet-stream"));
        assert_eq!(fs::read(&it.path).unwrap(), data);
        // Second download of the same resource is uniquified.
        let id2 = m.start(&url, dir.path(), None, vec![]);
        let it2 = wait_for(&m, id2, |i| i.state.is_finished());
        assert_eq!(it2.file_name, "résumé (1).bin");
    }

    #[test]
    fn http_error_and_retry() {
        let srv = spawn_server(pattern(10), true, None, 0);
        let dir = tempfile::tempdir().unwrap();
        let (m, _) = manager(None);
        let id = m.start(&format!("http://127.0.0.1:{}/missing", srv.port), dir.path(), None, vec![]);
        let it = wait_for(&m, id, |i| i.state.is_finished());
        assert_eq!(it.state, DownloadState::Failed("HTTP 404".into()));
        m.retry(id);
        let it = wait_for(&m, id, |i| i.state.is_finished());
        assert!(matches!(it.state, DownloadState::Failed(_)));
    }

    fn pause_resume(honor_range: bool) {
        let data = pattern(2 * 1024 * 1024);
        let srv = spawn_server(data.clone(), honor_range, None, 15);
        let dir = tempfile::tempdir().unwrap();
        let (m, _) = manager(None);
        let id = m.start(&format!("http://127.0.0.1:{}/file", srv.port), dir.path(), Some("big.bin"), vec![]);
        let it = wait_for(&m, id, |i| i.received_bytes > 0);
        assert!(it.received_bytes < data.len() as u64);
        assert_eq!(it.state, DownloadState::InProgress);
        m.pause(id);
        assert_eq!(m.get(id).unwrap().state, DownloadState::Paused);
        std::thread::sleep(Duration::from_millis(100));
        let part = part_path(&dir.path().join("big.bin"));
        let part_len = fs::metadata(&part).unwrap().len();
        assert!(part_len > 0 && part_len < data.len() as u64);
        m.resume(id);
        let it = wait_for(&m, id, |i| i.state.is_finished());
        assert_eq!(it.state, DownloadState::Completed, "{it:?}");
        assert_eq!(fs::read(&it.path).unwrap(), data);
        assert!(!part.exists());
        assert_eq!(srv.range_requests.load(Ordering::SeqCst), usize::from(honor_range));
    }

    #[test]
    fn resume_with_range() {
        pause_resume(true);
    }

    #[test]
    fn resume_when_server_ignores_range() {
        pause_resume(false);
    }

    #[test]
    fn cancel_removes_part_file() {
        let data = pattern(4 * 1024 * 1024);
        let srv = spawn_server(data, true, None, 20);
        let dir = tempfile::tempdir().unwrap();
        let (m, _) = manager(None);
        let id = m.start(&format!("http://127.0.0.1:{}/file", srv.port), dir.path(), Some("c.bin"), vec![]);
        wait_for(&m, id, |i| i.received_bytes > 0);
        assert_eq!(m.active_count(), 1);
        assert!(m.overall_progress().is_some());
        m.cancel(id);
        assert_eq!(m.get(id).unwrap().state, DownloadState::Cancelled);
        assert_eq!(m.active_count(), 0);
        let start = Instant::now();
        let part = part_path(&dir.path().join("c.bin"));
        while part.exists() {
            assert!(start.elapsed() < Duration::from_secs(5), "part file not removed");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!dir.path().join("c.bin").exists());
        assert_eq!(m.get(id).unwrap().state, DownloadState::Cancelled);
    }
}
