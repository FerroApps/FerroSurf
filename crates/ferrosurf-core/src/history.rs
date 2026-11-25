//! Browsing history in SQLite, with frecency ranking for the address bar.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub typed_count: i64,
    pub last_visit_ms: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visit_ms: i64,
}

pub struct History {
    conn: Connection,
    enabled: bool,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS urls (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL DEFAULT '',
    visit_count INTEGER NOT NULL DEFAULT 0,
    typed_count INTEGER NOT NULL DEFAULT 0,
    last_visit_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS visits (
    id INTEGER PRIMARY KEY,
    url_id INTEGER NOT NULL REFERENCES urls(id) ON DELETE CASCADE,
    visit_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS visits_time ON visits(visit_ms);
CREATE INDEX IF NOT EXISTS urls_last ON urls(last_visit_ms);
"#;

/// URLs that should never be recorded.
fn is_recordable(url: &str) -> bool {
    !(url.is_empty()
        || url.starts_with("about:")
        || url.starts_with("ferro:")
        || url.starts_with("data:")
        || url.starts_with("blob:")
        || url.starts_with("javascript:"))
}

impl History {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn, enabled: true })
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn, enabled: true })
    }

    /// When disabled (e.g. private/guest profile), visits are silently dropped.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn record_visit(&self, url: &str, title: &str, typed: bool) -> anyhow::Result<()> {
        self.record_visit_at(url, title, typed, crate::now_ms())
    }

    pub fn record_visit_at(&self, url: &str, title: &str, typed: bool, when_ms: i64) -> anyhow::Result<()> {
        if !self.enabled || !is_recordable(url) {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO urls (url, title, visit_count, typed_count, last_visit_ms) VALUES (?1, ?2, 1, ?3, ?4)
             ON CONFLICT(url) DO UPDATE SET
               visit_count = visit_count + 1,
               typed_count = typed_count + ?3,
               last_visit_ms = ?4,
               title = CASE WHEN ?2 = '' THEN title ELSE ?2 END",
            params![url, title, typed as i64, when_ms],
        )?;
        let id: i64 = self.conn.query_row("SELECT id FROM urls WHERE url = ?1", [url], |r| r.get(0))?;
        self.conn
            .execute("INSERT INTO visits (url_id, visit_ms) VALUES (?1, ?2)", params![id, when_ms])?;
        Ok(())
    }

    /// Upsert an entry imported from another browser: sums counts, keeps the newest
    /// timestamp and records a single visit row (at `last_visit_ms`).
    pub fn import_entry(
        &self,
        url: &str,
        title: &str,
        visit_count: i64,
        typed_count: i64,
        last_visit_ms: i64,
    ) -> anyhow::Result<()> {
        if !self.enabled || !is_recordable(url) {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO urls (url, title, visit_count, typed_count, last_visit_ms) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url) DO UPDATE SET
               visit_count = visit_count + ?3,
               typed_count = typed_count + ?4,
               last_visit_ms = MAX(last_visit_ms, ?5),
               title = CASE WHEN title = '' THEN ?2 ELSE title END",
            params![url, title, visit_count.max(1), typed_count.max(0), last_visit_ms],
        )?;
        let id: i64 = self.conn.query_row("SELECT id FROM urls WHERE url = ?1", [url], |r| r.get(0))?;
        self.conn
            .execute("INSERT INTO visits (url_id, visit_ms) VALUES (?1, ?2)", params![id, last_visit_ms])?;
        Ok(())
    }

    pub fn update_title(&self, url: &str, title: &str) -> anyhow::Result<()> {
        if !self.enabled || title.is_empty() {
            return Ok(());
        }
        self.conn
            .execute("UPDATE urls SET title = ?2 WHERE url = ?1", params![url, title])?;
        Ok(())
    }

    pub fn get(&self, url: &str) -> anyhow::Result<Option<HistoryEntry>> {
        Ok(self
            .conn
            .query_row(
                "SELECT url, title, visit_count, typed_count, last_visit_ms FROM urls WHERE url = ?1",
                [url],
                row_to_entry,
            )
            .optional()?)
    }

    /// Frecency-ranked matches for the address bar. Every whitespace separated word must
    /// match the title or URL.
    pub fn suggest(&self, query: &str, limit: usize) -> anyhow::Result<Vec<(u32, HistoryEntry)>> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT url, title, visit_count, typed_count, last_visit_ms FROM urls WHERE 1=1",
        );
        for i in 0..words.len() {
            sql.push_str(&format!(
                " AND (url LIKE ?{n} ESCAPE '\\' OR title LIKE ?{n} ESCAPE '\\')",
                n = i + 1
            ));
        }
        sql.push_str(" ORDER BY last_visit_ms DESC LIMIT 500");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(words.iter()), row_to_entry)?;
        let now = crate::now_ms();
        let q = query.to_lowercase();
        let mut out: Vec<(u32, HistoryEntry)> = rows
            .filter_map(Result::ok)
            .map(|e| (frecency(&e, now, &q), e))
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.0));
        out.truncate(limit);
        Ok(out)
    }

    /// Most visited sites (for the new-tab page), one entry per host.
    pub fn top_sites(&self, limit: usize) -> anyhow::Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, title, visit_count, typed_count, last_visit_ms FROM urls
             WHERE last_visit_ms > ?1 ORDER BY visit_count + typed_count * 3 DESC LIMIT 200",
        )?;
        let cutoff = crate::now_ms() - 90 * 24 * 3600 * 1000;
        let rows = stmt.query_map([cutoff], row_to_entry)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in rows.filter_map(Result::ok) {
            let host = url::Url::parse(&e.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            if seen.insert(host) {
                out.push(e);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Visits in reverse-chronological order, optionally filtered by text.
    pub fn visits(&self, filter: &str, before_ms: Option<i64>, limit: usize) -> anyhow::Result<Vec<Visit>> {
        let like = format!("%{}%", filter.trim());
        let mut stmt = self.conn.prepare(
            "SELECT u.url, u.title, v.visit_ms FROM visits v JOIN urls u ON u.id = v.url_id
             WHERE v.visit_ms < ?1 AND (u.url LIKE ?2 OR u.title LIKE ?2)
             ORDER BY v.visit_ms DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![before_ms.unwrap_or(i64::MAX), like, limit as i64],
            |r| Ok(Visit { url: r.get(0)?, title: r.get(1)?, visit_ms: r.get(2)? }),
        )?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn delete_url(&self, url: &str) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM urls WHERE url = ?1", [url])?;
        Ok(())
    }

    /// Delete visits in `[from_ms, to_ms)` and prune URLs without visits.
    pub fn delete_range(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM visits WHERE visit_ms >= ?1 AND visit_ms < ?2", params![from_ms, to_ms])?;
        self.conn.execute(
            "DELETE FROM urls WHERE id NOT IN (SELECT DISTINCT url_id FROM visits)",
            [],
        )?;
        Ok(())
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        self.conn.execute_batch("DELETE FROM visits; DELETE FROM urls;")?;
        Ok(())
    }

    pub fn count(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM urls", [], |r| r.get(0))?)
    }
}

fn row_to_entry(r: &rusqlite::Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        url: r.get(0)?,
        title: r.get(1)?,
        visit_count: r.get(2)?,
        typed_count: r.get(3)?,
        last_visit_ms: r.get(4)?,
    })
}

/// Score combining frequency, recency, typed-ness and match quality.
fn frecency(e: &HistoryEntry, now_ms: i64, query: &str) -> u32 {
    let age_days = ((now_ms - e.last_visit_ms).max(0) / 86_400_000) as f64;
    let recency = match age_days {
        d if d < 1.0 => 100.0,
        d if d < 4.0 => 70.0,
        d if d < 14.0 => 50.0,
        d if d < 31.0 => 30.0,
        d if d < 90.0 => 10.0,
        _ => 5.0,
    };
    let freq = (e.visit_count as f64 + 2.0 * e.typed_count as f64).ln_1p() * 30.0;
    let pretty = crate::util::pretty_url(&e.url).to_lowercase();
    let mut bonus = 0.0;
    if pretty.starts_with(query) {
        bonus += 80.0;
        // Prefer short (root) URLs when they match by prefix.
        bonus += (40.0 - pretty.len() as f64).max(0.0);
    }
    if e.title.to_lowercase().starts_with(query) {
        bonus += 25.0;
    }
    (recency + freq + bonus) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_suggest() {
        let h = History::in_memory().unwrap();
        h.record_visit("https://servo.org/", "Servo", true).unwrap();
        h.record_visit("https://servo.org/", "", false).unwrap();
        h.record_visit("https://servo.org/blog/2025/", "Servo blog", false).unwrap();
        h.record_visit("https://rust-lang.org/", "Rust", false).unwrap();
        h.record_visit("about:blank", "", false).unwrap();
        h.record_visit("ferro://newtab", "", false).unwrap();
        assert_eq!(h.count().unwrap(), 3);
        let e = h.get("https://servo.org/").unwrap().unwrap();
        assert_eq!((e.visit_count, e.typed_count, e.title.as_str()), (2, 1, "Servo"));
        let s = h.suggest("serv", 10).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].1.url, "https://servo.org/");
        assert_eq!(h.suggest("servo blog", 10).unwrap().len(), 1);
        assert!(h.suggest("100%", 10).unwrap().is_empty());
        assert_eq!(h.top_sites(10).unwrap().len(), 2);
        assert_eq!(h.visits("", None, 100).unwrap().len(), 4);
    }

    #[test]
    fn deletion() {
        let h = History::in_memory().unwrap();
        h.record_visit_at("https://a.com/", "A", false, 1000).unwrap();
        h.record_visit_at("https://b.com/", "B", false, 5000).unwrap();
        h.delete_range(0, 2000).unwrap();
        assert!(h.get("https://a.com/").unwrap().is_none());
        assert!(h.get("https://b.com/").unwrap().is_some());
        h.delete_url("https://b.com/").unwrap();
        assert_eq!(h.visits("", None, 10).unwrap().len(), 0);
        h.record_visit("https://c.com/", "C", false).unwrap();
        h.clear().unwrap();
        assert_eq!(h.count().unwrap(), 0);
    }

    #[test]
    fn disabled() {
        let mut h = History::in_memory().unwrap();
        h.set_enabled(false);
        h.record_visit("https://a.com/", "A", false).unwrap();
        assert_eq!(h.count().unwrap(), 0);
    }
}
