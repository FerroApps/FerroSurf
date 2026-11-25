//! Local passkey store: `<profile>/passkeys.json` + `<profile>/passkeys.key`.
//!
//! Private keys are encrypted with AES-256-GCM (nonce || ciphertext, credential id as AAD).
//! The key lives next to the data in `passkeys.key` (mode 0600 on unix), so this protects
//! only against casual disclosure of `passkeys.json` (backups, sync tools), NOT against
//! anything that can read your profile directory. For real protection use Bitwarden.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::b64;
use crate::passkey::{PasskeySource, StoredPasskey};

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    meta: StoredPasskey,
    /// base64url(nonce(12) || ciphertext)
    private_key_enc: String,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    passkeys: Vec<Entry>,
}

pub struct LocalPasskeyStore {
    dir: PathBuf,
    keys: Vec<StoredPasskey>,
}

impl LocalPasskeyStore {
    pub fn data_path(dir: &Path) -> PathBuf {
        dir.join("passkeys.json")
    }
    pub fn key_path(dir: &Path) -> PathBuf {
        dir.join("passkeys.key")
    }

    /// Open (and decrypt) the store in `dir`. A missing file is an empty store; an
    /// unreadable entry is skipped with a warning.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let mut keys = Vec::new();
        let data = Self::data_path(&dir);
        if data.exists() {
            let file: File =
                serde_json::from_slice(&std::fs::read(&data)?).context("passkeys.json")?;
            let cipher = cipher(&dir, false)?;
            for e in file.passkeys {
                match decrypt(&cipher, &e.meta.credential_id, &e.private_key_enc) {
                    Ok(pk) => {
                        let mut k = e.meta;
                        k.private_key_pkcs8 = pk;
                        k.source = PasskeySource::Local;
                        keys.push(k);
                    }
                    Err(err) => log::warn!("skipping undecryptable passkey: {err}"),
                }
            }
        }
        Ok(Self { dir, keys })
    }

    pub fn all(&self) -> &[StoredPasskey] {
        &self.keys
    }

    pub fn for_rp(&self, rp_id: &str) -> Vec<StoredPasskey> {
        self.keys
            .iter()
            .filter(|k| k.rp_id == rp_id)
            .cloned()
            .collect()
    }

    /// Add (replacing a same-id entry, or the same user for the same RP) and persist.
    pub fn add(&mut self, mut key: StoredPasskey) -> Result<()> {
        key.source = PasskeySource::Local;
        self.keys.retain(|k| {
            k.credential_id != key.credential_id
                && !(k.rp_id == key.rp_id
                    && !k.user_handle.is_empty()
                    && k.user_handle == key.user_handle)
        });
        self.keys.push(key);
        self.save()
    }

    pub fn remove(&mut self, credential_id: &[u8]) -> Result<bool> {
        let before = self.keys.len();
        self.keys.retain(|k| k.credential_id != credential_id);
        let changed = self.keys.len() != before;
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    /// Persist a new signature counter.
    pub fn set_counter(&mut self, credential_id: &[u8], counter: u32) -> Result<()> {
        if let Some(k) = self
            .keys
            .iter_mut()
            .find(|k| k.credential_id == credential_id)
        {
            k.counter = counter;
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let cipher = cipher(&self.dir, true)?;
        let mut file = File {
            version: 1,
            passkeys: Vec::new(),
        };
        for k in &self.keys {
            let enc = encrypt(&cipher, &k.credential_id, &k.private_key_pkcs8)?;
            file.passkeys.push(Entry {
                meta: k.clone(),
                private_key_enc: enc,
            });
        }
        let path = Self::data_path(&self.dir);
        let tmp = path.with_extension("json.tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(&file)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)?;
        Ok(())
    }
}

/// Load the AES key, creating it when `create` is set.
fn cipher(dir: &Path, create: bool) -> Result<Aes256Gcm> {
    let path = LocalPasskeyStore::key_path(dir);
    let key = match std::fs::read(&path) {
        Ok(k) if k.len() == 32 => k,
        Ok(_) => bail!("passkeys.key is corrupt"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            std::fs::create_dir_all(dir)?;
            let mut k = [0u8; 32];
            crate::random_bytes(&mut k)?;
            write_private(&path, &k)?;
            k.to_vec()
        }
        Err(e) => return Err(e).context("passkeys.key"),
    };
    Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow::anyhow!("cipher: {e}"))
}

fn encrypt(c: &Aes256Gcm, aad: &[u8], plain: &[u8]) -> Result<String> {
    let mut nonce = [0u8; 12];
    crate::random_bytes(&mut nonce)?;
    let ct = c
        .encrypt(
            &Nonce::try_from(&nonce[..]).expect("12 bytes"),
            Payload { msg: plain, aad },
        )
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(b64::enc(&out))
}

fn decrypt(c: &Aes256Gcm, aad: &[u8], enc: &str) -> Result<Vec<u8>> {
    let raw = b64::dec(enc)?;
    if raw.len() < 12 + 16 {
        bail!("ciphertext too short");
    }
    let (nonce, ct) = raw.split_at(12);
    c.decrypt(
        &Nonce::try_from(nonce).map_err(|_| anyhow::anyhow!("bad nonce"))?,
        Payload { msg: ct, aad },
    )
    .map_err(|_| anyhow::anyhow!("decryption failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webauthn::*;

    #[test]
    fn roundtrip_and_encryption() {
        let dir = tempfile::tempdir().unwrap();
        let u = UserEntity {
            id: vec![9; 8],
            name: "a".into(),
            display_name: "A".into(),
        };
        let r = make_credential(&MakeCredential {
            rp_id: "example.com",
            rp_name: "Ex",
            origin: "https://example.com",
            client_data_hash: &[0; 32],
            user: &u,
            exclude_list: &[],
            existing: &[],
            discoverable: true,
            backed_up: false,
            source: PasskeySource::Local,
        })
        .unwrap();
        let mut s = LocalPasskeyStore::open(dir.path()).unwrap();
        s.add(r.passkey.clone()).unwrap();
        s.set_counter(&r.credential_id, 5).unwrap();
        let raw = std::fs::read_to_string(LocalPasskeyStore::data_path(dir.path())).unwrap();
        assert!(!raw.contains(&b64::enc(&r.passkey.private_key_pkcs8)));
        let s2 = LocalPasskeyStore::open(dir.path()).unwrap();
        assert_eq!(s2.all().len(), 1);
        assert_eq!(s2.all()[0].private_key_pkcs8, r.passkey.private_key_pkcs8);
        assert_eq!(s2.all()[0].counter, 5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(LocalPasskeyStore::key_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
        }
        let mut s2 = s2;
        assert!(s2.remove(&r.credential_id).unwrap());
        assert!(
            LocalPasskeyStore::open(dir.path())
                .unwrap()
                .all()
                .is_empty()
        );
    }
}
