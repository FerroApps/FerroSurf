//! Client for the official Bitwarden CLI (`bw`). All functions block; call them from
//! worker threads. The master password is passed through an environment variable and the
//! session key through `BW_SESSION`, never on argv.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::b64;
use crate::passkey::{PasskeySource, StoredPasskey};

const TIMEOUT: Duration = Duration::from_secs(90);

#[cfg(windows)]
const BIN_NAMES: &[&str] = &["bw.exe", "bw.cmd"];
#[cfg(not(windows))]
const BIN_NAMES: &[&str] = &["bw"];

/// Locate the `bw` binary: explicit override, `FERROSURF_BW`, PATH, then common installs.
pub fn locate(override_path: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = override_path.filter(|p| !p.trim().is_empty()) {
        candidates.push(PathBuf::from(p.trim()));
    }
    if let Ok(p) = std::env::var("FERROSURF_BW")
        && !p.trim().is_empty()
    {
        candidates.push(PathBuf::from(p.trim()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in BIN_NAMES {
                candidates.push(dir.join(n));
            }
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        for rel in [
            ".local/bin",
            ".npm-global/bin",
            ".yarn/bin",
            "bin",
            ".bun/bin",
            "AppData/Roaming/npm",
            ".nvm/current/bin",
        ] {
            dirs.push(h.join(rel));
        }
        dirs.push(h.join(".local/share/flatpak/exports/bin"));
    }
    for d in [
        "/usr/local/bin",
        "/usr/bin",
        "/opt/homebrew/bin",
        "/snap/bin",
        "/var/lib/flatpak/exports/bin",
    ] {
        dirs.push(PathBuf::from(d));
    }
    for d in dirs {
        for n in BIN_NAMES {
            candidates.push(d.join(n));
        }
    }
    // Flatpak / snap wrappers exposed under their app ids.
    if let Some(h) = &home {
        candidates.push(h.join(".local/share/flatpak/exports/bin/com.bitwarden.desktop.bw"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    Unauthenticated,
    Locked,
    Unlocked,
}

#[derive(Clone, Debug)]
pub struct BwStatus {
    pub state: VaultState,
    pub server_url: Option<String>,
    pub user_email: Option<String>,
    pub last_sync: Option<String>,
}

/// Session key; kept only in memory. `Debug` never prints it.
#[derive(Clone)]
pub struct SessionKey(String);

impl SessionKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(***)")
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoginItem {
    pub id: String,
    pub name: String,
    pub username: String,
    pub password: String,
    /// TOTP seed / otpauth URI (use [`Bitwarden::totp_code`] for the current code).
    pub totp: Option<String>,
    pub uris: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Bitwarden {
    bin: PathBuf,
}

struct Output {
    stdout: String,
    stderr: String,
    ok: bool,
}

impl Bitwarden {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    pub fn locate(override_path: Option<&str>) -> Option<Self> {
        locate(override_path).map(Self::new)
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    fn run(
        &self,
        args: &[&str],
        session: Option<&SessionKey>,
        env: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> Result<Output> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("BW_NOINTERACTION", "true")
            .env("BW_QUIET", "true")
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(s) = session {
            cmd.env("BW_SESSION", s.expose());
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot run {}", self.bin.display()))?;
        if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
            let _ = si.write_all(data.as_bytes());
        }
        let mut so = child.stdout.take().expect("piped");
        let mut se = child.stderr.take().expect("piped");
        let t_out = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let t_err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = se.read_to_string(&mut s);
            s
        });
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("bw timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        Ok(Output {
            stdout: t_out.join().unwrap_or_default(),
            stderr: t_err.join().unwrap_or_default(),
            ok: status.success(),
        })
    }

    fn run_ok(
        &self,
        args: &[&str],
        session: Option<&SessionKey>,
        env: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> Result<String> {
        let out = self.run(args, session, env, stdin)?;
        if !out.ok {
            let msg = out.stderr.trim();
            bail!("{}", if msg.is_empty() { "bw failed" } else { msg });
        }
        Ok(out.stdout)
    }

    pub fn version(&self) -> Result<String> {
        Ok(self
            .run_ok(&["--version"], None, &[], None)?
            .trim()
            .to_string())
    }

    /// `bw status` (works without a session; pass it to detect the unlocked state).
    pub fn status(&self, session: Option<&SessionKey>) -> Result<BwStatus> {
        let out = self.run_ok(&["status"], session, &[], None)?;
        parse_status(&out)
    }

    /// Unlock the vault and return the session key.
    pub fn unlock(&self, master_password: &str) -> Result<SessionKey> {
        let out = self.run(
            &["unlock", "--raw", "--passwordenv", "BW_FERRO_PASSWORD"],
            None,
            &[("BW_FERRO_PASSWORD", master_password)],
            None,
        )?;
        if !out.ok {
            let m = out.stderr.trim();
            bail!(
                "{}",
                if m.is_empty() {
                    "No se pudo desbloquear"
                } else {
                    m
                }
            );
        }
        let key = out.stdout.trim().to_string();
        if key.is_empty() {
            bail!("bw returned no session key");
        }
        Ok(SessionKey(key))
    }

    /// Log in with email + master password (accounts without 2FA; otherwise use `bw login`
    /// in a terminal).
    pub fn login(&self, email: &str, master_password: &str) -> Result<SessionKey> {
        let out = self.run_ok(
            &[
                "login",
                email,
                "--raw",
                "--passwordenv",
                "BW_FERRO_PASSWORD",
            ],
            None,
            &[("BW_FERRO_PASSWORD", master_password)],
            None,
        )?;
        Ok(SessionKey(out.trim().to_string()))
    }

    pub fn lock(&self) -> Result<()> {
        self.run_ok(&["lock"], None, &[], None).map(|_| ())
    }

    pub fn sync(&self, session: &SessionKey) -> Result<()> {
        self.run_ok(&["sync"], Some(session), &[], None).map(|_| ())
    }

    fn list_items(&self, session: &SessionKey, args: &[&str]) -> Result<Vec<Value>> {
        let mut a = vec!["list", "items"];
        a.extend_from_slice(args);
        let out = self.run_ok(&a, Some(session), &[], None)?;
        match serde_json::from_str::<Value>(out.trim()).context("bw list items")? {
            Value::Array(v) => Ok(v),
            _ => bail!("unexpected bw output"),
        }
    }

    /// Login items matching `url` (Bitwarden's own URI matching rules).
    pub fn logins_for_url(&self, session: &SessionKey, url: &str) -> Result<Vec<LoginItem>> {
        let items = self.list_items(session, &["--url", url])?;
        Ok(items.iter().filter_map(parse_login).collect())
    }

    /// Current TOTP code of an item.
    pub fn totp_code(&self, session: &SessionKey, item_id: &str) -> Result<String> {
        Ok(self
            .run_ok(&["get", "totp", item_id], Some(session), &[], None)?
            .trim()
            .to_string())
    }

    /// Passkeys (`login.fido2Credentials`) stored for `rp_id`.
    pub fn passkeys_for_rp(&self, session: &SessionKey, rp_id: &str) -> Result<Vec<StoredPasskey>> {
        let mut items = self
            .list_items(session, &["--url", &format!("https://{rp_id}")])
            .unwrap_or_default();
        // Items without a matching URI: fall back to a text search.
        if let Ok(more) = self.list_items(session, &["--search", rp_id]) {
            for m in more {
                let id = m["id"].as_str();
                if !items.iter().any(|i| i["id"].as_str() == id) {
                    items.push(m);
                }
            }
        }
        let mut out = Vec::new();
        for it in &items {
            let id = it["id"].as_str().unwrap_or("").to_string();
            for c in it["login"]["fido2Credentials"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(mut k) = parse_fido2(c)
                    && k.rp_id == rp_id
                {
                    k.item_id = Some(id.clone());
                    k.source = PasskeySource::Bitwarden;
                    out.push(k);
                }
            }
        }
        Ok(out)
    }

    /// Store a passkey in the vault: appended to `existing_item` when given, otherwise a
    /// new login item is created. Returns the item id. Best effort: older CLIs may
    /// drop `fido2Credentials`, which is detected by reading the item back.
    pub fn save_passkey(
        &self,
        session: &SessionKey,
        key: &StoredPasskey,
        existing_item: Option<&str>,
    ) -> Result<String> {
        let cred = fido2_json(key);
        let (item, edit_id) = match existing_item {
            Some(id) => {
                let mut item: Value = serde_json::from_str(
                    self.run_ok(&["get", "item", id], Some(session), &[], None)?
                        .trim(),
                )?;
                let list = item["login"]["fido2Credentials"].take();
                let mut arr = list.as_array().cloned().unwrap_or_default();
                arr.push(cred);
                item["login"]["fido2Credentials"] = Value::Array(arr);
                (item, Some(id.to_string()))
            }
            None => {
                let name = if key.rp_name.is_empty() {
                    key.rp_id.clone()
                } else {
                    key.rp_name.clone()
                };
                (
                    json!({
                        "organizationId": null, "collectionIds": null, "folderId": null,
                        "type": 1, "name": name, "notes": null, "favorite": false,
                        "login": {
                            "uris": [{ "match": null, "uri": format!("https://{}", key.rp_id) }],
                            "username": key.user_name, "password": null, "totp": null,
                            "fido2Credentials": [cred],
                        },
                    }),
                    None,
                )
            }
        };
        let encoded = self.run_ok(&["encode"], None, &[], Some(&item.to_string()))?;
        let encoded = encoded.trim();
        let out = match &edit_id {
            Some(id) => self.run_ok(&["edit", "item", id, encoded], Some(session), &[], None)?,
            None => self.run_ok(&["create", "item", encoded], Some(session), &[], None)?,
        };
        let saved: Value = serde_json::from_str(out.trim()).context("bw create/edit output")?;
        let id = saved["id"].as_str().context("no item id")?.to_string();
        let has = saved["login"]["fido2Credentials"]
            .as_array()
            .is_some_and(|a| !a.is_empty());
        if !has {
            // This CLI build ignores fido2Credentials: don't leave a useless item behind.
            if edit_id.is_none() {
                let _ = self.run_ok(&["delete", "item", &id], Some(session), &[], None);
            }
            bail!("esta versión de bw no admite llaves de acceso");
        }
        Ok(id)
    }
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "serverUrl")]
    server_url: Option<String>,
    #[serde(rename = "lastSync")]
    last_sync: Option<String>,
    #[serde(rename = "userEmail")]
    user_email: Option<String>,
    status: String,
}

pub fn parse_status(json: &str) -> Result<BwStatus> {
    let r: RawStatus = serde_json::from_str(json.trim()).context("bw status")?;
    let state = match r.status.as_str() {
        "unauthenticated" => VaultState::Unauthenticated,
        "locked" => VaultState::Locked,
        "unlocked" => VaultState::Unlocked,
        other => bail!("unknown bw status {other}"),
    };
    Ok(BwStatus {
        state,
        server_url: r.server_url,
        user_email: r.user_email,
        last_sync: r.last_sync,
    })
}

fn parse_login(item: &Value) -> Option<LoginItem> {
    let login = item.get("login").filter(|l| l.is_object())?;
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    Some(LoginItem {
        id: s(&item["id"]),
        name: s(&item["name"]),
        username: s(&login["username"]),
        password: s(&login["password"]),
        totp: login["totp"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(String::from),
        uris: login["uris"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|u| u["uri"].as_str().map(String::from))
            .collect(),
    })
}

/// Bitwarden stores credential ids as GUID strings when they are 16 bytes, otherwise
/// base64url.
fn decode_credential_id(s: &str) -> Option<Vec<u8>> {
    let s = s.strip_prefix("b64.").unwrap_or(s);
    if let Ok(u) = uuid::Uuid::parse_str(s) {
        return Some(u.as_bytes().to_vec());
    }
    b64::dec(s).ok()
}

pub fn parse_fido2(c: &Value) -> Option<StoredPasskey> {
    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
    let alg = s("keyAlgorithm");
    if !alg.is_empty() && !alg.eq_ignore_ascii_case("ECDSA") && !alg.eq_ignore_ascii_case("ES256") {
        return None;
    }
    let curve = s("keyCurve");
    if !curve.is_empty() && curve != "P-256" {
        return None;
    }
    let counter = match &c["counter"] {
        Value::String(x) => x.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0) as u32,
        _ => 0,
    };
    let discoverable = match &c["discoverable"] {
        Value::String(x) => x == "true",
        Value::Bool(b) => *b,
        _ => true,
    };
    Some(StoredPasskey {
        credential_id: decode_credential_id(&s("credentialId"))?,
        rp_id: s("rpId"),
        rp_name: s("rpName"),
        user_handle: b64::dec(&s("userHandle")).unwrap_or_default(),
        user_name: s("userName"),
        user_display_name: s("userDisplayName"),
        private_key_pkcs8: b64::dec(&s("keyValue")).ok()?,
        counter,
        discoverable,
        created: 0,
        source: PasskeySource::Bitwarden,
        item_id: None,
    })
}

fn fido2_json(k: &StoredPasskey) -> Value {
    let cid = if k.credential_id.len() == 16 {
        uuid::Uuid::from_slice(&k.credential_id)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("b64.{}", b64::enc(&k.credential_id)))
    } else {
        format!("b64.{}", b64::enc(&k.credential_id))
    };
    let date = {
        // RFC 3339 without extra deps: seconds resolution, UTC.
        let secs = if k.created > 0 {
            k.created
        } else {
            crate::now_secs()
        };
        rfc3339(secs)
    };
    json!({
        "credentialId": cid,
        "keyType": "public-key",
        "keyAlgorithm": "ECDSA",
        "keyCurve": "P-256",
        "keyValue": b64::enc(&k.private_key_pkcs8),
        "rpId": k.rp_id,
        "rpName": k.rp_name,
        "userHandle": b64::enc(&k.user_handle),
        "userName": k.user_name,
        "userDisplayName": k.user_display_name,
        "counter": k.counter.to_string(),
        "discoverable": k.discoverable.to_string(),
        "creationDate": date,
    })
}

fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        let s = parse_status(r#"{"serverUrl":"https://vault.example","lastSync":"2024-01-01T00:00:00.000Z","userEmail":"a@b.c","userId":"x","status":"locked"}"#).unwrap();
        assert_eq!(s.state, VaultState::Locked);
        assert_eq!(s.user_email.as_deref(), Some("a@b.c"));
        assert_eq!(
            parse_status(r#"{"serverUrl":null,"status":"unauthenticated"}"#)
                .unwrap()
                .state,
            VaultState::Unauthenticated
        );
        assert!(parse_status("garbage").is_err());
    }

    #[test]
    fn login_and_fido2_parsing() {
        let item = json!({"id":"i1","name":"Ex","login":{"username":"u","password":"p","totp":"SEED","uris":[{"uri":"https://example.com"}],
            "fido2Credentials":[{"credentialId":"6f5a8c1e-0000-4000-8000-000000000001","keyType":"public-key","keyAlgorithm":"ECDSA","keyCurve":"P-256",
            "keyValue":"AQID","rpId":"example.com","userHandle":"AAEC","userName":"u","counter":"3","discoverable":"true"}]}});
        let l = parse_login(&item).unwrap();
        assert_eq!(
            (l.username.as_str(), l.password.as_str(), l.totp.as_deref()),
            ("u", "p", Some("SEED"))
        );
        let k = parse_fido2(&item["login"]["fido2Credentials"][0]).unwrap();
        assert_eq!(k.credential_id.len(), 16);
        assert_eq!(k.counter, 3);
        assert_eq!(k.user_handle, vec![0, 1, 2]);
        // roundtrip through our writer
        let again = parse_fido2(&fido2_json(&k)).unwrap();
        assert_eq!(again.credential_id, k.credential_id);
        assert_eq!(again.private_key_pkcs8, k.private_key_pkcs8);
        let mut bad = item["login"]["fido2Credentials"][0].clone();
        bad["keyCurve"] = json!("P-384");
        assert!(parse_fido2(&bad).is_none());
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_709_210_096), "2024-02-29T12:34:56.000Z");
    }
}
