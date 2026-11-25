//! Small helpers for crash-safe JSON persistence.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Atomically write `value` as pretty JSON to `path` (write to temp file + rename).
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Read JSON from `path`. Returns `Ok(None)` if the file doesn't exist. A corrupt file is
/// moved aside (`*.corrupt`) so that the browser can start with defaults instead of failing.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match serde_json::from_slice(&data) {
        Ok(v) => Ok(Some(v)),
        Err(err) => {
            log::error!("Corrupt file {}: {err}; moving it aside", path.display());
            let _ = fs::rename(path, path.with_extension("json.corrupt"));
            Ok(None)
        },
    }
}

/// Read JSON or fall back to `T::default()`.
pub fn read_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    match read_json(path) {
        Ok(Some(v)) => v,
        Ok(None) => T::default(),
        Err(e) => {
            log::error!("Failed to read {}: {e}", path.display());
            T::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        assert!(read_json::<Vec<u32>>(&p).unwrap().is_none());
        write_json(&p, &vec![1u32, 2, 3]).unwrap();
        assert_eq!(read_json::<Vec<u32>>(&p).unwrap(), Some(vec![1, 2, 3]));
        std::fs::write(&p, b"{not json").unwrap();
        assert!(read_json::<Vec<u32>>(&p).unwrap().is_none());
        assert!(dir.path().join("x.json.corrupt").exists());
    }
}
