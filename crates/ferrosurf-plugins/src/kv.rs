//! Per-plugin key-value store (`storage` permission): one SQLite table in
//! `<data dir>/plugins.sqlite3`, opened on first use, with quotas.

use std::path::PathBuf;

use ferrosurf_plugin_sdk::abi::err;
use rusqlite::{Connection, OptionalExtension, params};

pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Total bytes (keys + values) one plugin may keep.
pub const MAX_PLUGIN_BYTES: i64 = 1024 * 1024;
pub const MAX_PLUGIN_KEYS: i64 = 1024;

pub struct Kv {
    /// `None`: in memory (tests).
    path: Option<PathBuf>,
    conn: Option<Connection>,
}

impl Kv {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, conn: None }
    }

    fn conn(&mut self) -> Result<&Connection, i32> {
        if self.conn.is_none() {
            let c = match &self.path {
                Some(p) => {
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    Connection::open(p)
                }
                None => Connection::open_in_memory(),
            }
            .map_err(|e| {
                log::warn!("plugin store unavailable: {e}");
                err::UNAVAILABLE
            })?;
            c.execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE IF NOT EXISTS kv (
                     plugin TEXT NOT NULL,
                     key TEXT NOT NULL,
                     value BLOB NOT NULL,
                     PRIMARY KEY (plugin, key)
                 ) WITHOUT ROWID;",
            )
            .map_err(|_| err::UNAVAILABLE)?;
            self.conn = Some(c);
        }
        self.conn.as_ref().ok_or(err::UNAVAILABLE)
    }

    fn check_key(key: &str) -> Result<(), i32> {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(err::BAD_INPUT);
        }
        Ok(())
    }

    pub fn get(&mut self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>, i32> {
        Self::check_key(key)?;
        self.conn()?
            .query_row(
                "SELECT value FROM kv WHERE plugin = ?1 AND key = ?2",
                params![plugin, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| err::UNAVAILABLE)
    }

    pub fn set(&mut self, plugin: &str, key: &str, value: &[u8]) -> Result<(), i32> {
        Self::check_key(key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(err::LIMIT);
        }
        let c = self.conn()?;
        // Usage without the key being replaced.
        let (keys, bytes): (i64, i64) = c
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(key) + LENGTH(value)), 0)
                 FROM kv WHERE plugin = ?1 AND key != ?2",
                params![plugin, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| err::UNAVAILABLE)?;
        if keys + 1 > MAX_PLUGIN_KEYS || bytes + (key.len() + value.len()) as i64 > MAX_PLUGIN_BYTES
        {
            return Err(err::LIMIT);
        }
        c.execute(
            "INSERT OR REPLACE INTO kv (plugin, key, value) VALUES (?1, ?2, ?3)",
            params![plugin, key, value],
        )
        .map(drop)
        .map_err(|_| err::UNAVAILABLE)
    }

    pub fn delete(&mut self, plugin: &str, key: &str) -> Result<(), i32> {
        Self::check_key(key)?;
        self.conn()?
            .execute(
                "DELETE FROM kv WHERE plugin = ?1 AND key = ?2",
                params![plugin, key],
            )
            .map(drop)
            .map_err(|_| err::UNAVAILABLE)
    }

    /// Removes everything a plugin stored (uninstall).
    pub fn delete_all(&mut self, plugin: &str) -> Result<(), i32> {
        self.conn()?
            .execute("DELETE FROM kv WHERE plugin = ?1", params![plugin])
            .map(drop)
            .map_err(|_| err::UNAVAILABLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_and_quota_limited() {
        let mut kv = Kv::new(None);
        kv.set("a", "k", b"1").unwrap();
        assert_eq!(kv.get("a", "k").unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(kv.get("b", "k").unwrap(), None);
        assert_eq!(kv.set("a", "", b"x"), Err(err::BAD_INPUT));
        assert_eq!(
            kv.set("a", "big", &vec![0; MAX_VALUE_BYTES + 1]),
            Err(err::LIMIT)
        );
        let chunk = vec![0u8; MAX_VALUE_BYTES];
        let mut n = 0;
        while kv.set("a", &format!("c{n}"), &chunk).is_ok() {
            n += 1;
        }
        assert_eq!(n, 15, "1 MiB budget");
        kv.set("a", "c0", &chunk).unwrap();
        kv.delete("a", "c0").unwrap();
        assert_eq!(kv.get("a", "c0").unwrap(), None);
        kv.delete_all("a").unwrap();
        assert_eq!(kv.get("a", "c1").unwrap(), None);
    }

    #[test]
    fn persists_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plugins.sqlite3");
        Kv::new(Some(p.clone())).set("a", "k", b"v").unwrap();
        assert_eq!(
            Kv::new(Some(p)).get("a", "k").unwrap().as_deref(),
            Some(&b"v"[..])
        );
    }
}
