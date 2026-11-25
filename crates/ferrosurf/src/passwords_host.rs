//! App-side controller for the password manager (Bitwarden CLI) and the software passkey
//! authenticator. Lives on the UI thread; `bw` calls run on short-lived `std::thread`s and
//! report back through a channel drained by [`PasswordsHost::poll`].
//!
//! Security model: everything coming from `window.__ferrosurf.call("webauthn", …)` is
//! untrusted page input. The origin is the tab's URL origin (never the payload), the
//! page-built clientDataJSON is re-validated against it, the RP ID must be a registrable
//! suffix of that origin, and *every* create/get asks the user in browser UI first.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ferrosurf_passwords::bitwarden::{Bitwarden, BwStatus, LoginItem, SessionKey, VaultState};
use ferrosurf_passwords::store::LocalPasskeyStore;
use ferrosurf_passwords::webauthn::{self, MakeCredential, UserEntity};
use ferrosurf_passwords::{PasskeySource, StoredPasskey, b64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bridge::Reply;
use crate::tab::TabId;
use crate::window::WakeFn;

/// Where new passkeys are saved by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PasskeyStorage {
    #[default]
    Bitwarden,
    Local,
}

/// Persisted in `<profile>/passwords.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordsConfig {
    /// Explicit path to `bw` (empty = auto-detect).
    pub bw_path: String,
    /// Lock the vault after this many idle minutes (0 = never).
    pub auto_lock_minutes: u32,
    pub default_store: PasskeyStorage,
}

impl Default for PasswordsConfig {
    fn default() -> Self {
        Self {
            bw_path: String::new(),
            auto_lock_minutes: 15,
            default_store: PasskeyStorage::Bitwarden,
        }
    }
}

/// What the UI shows about Bitwarden.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BwUi {
    /// Status not fetched yet.
    Unknown,
    NotInstalled,
    Unauthenticated,
    Locked,
    Unlocked,
}

pub enum ConsentKind {
    Create {
        rp_name: String,
        user: UserEntity,
        exclude: Vec<Vec<u8>>,
        discoverable: bool,
    },
    Get {
        allow: Vec<Vec<u8>>,
    },
}

/// A passkey request waiting for the user.
pub struct PendingConsent {
    pub id: u64,
    pub tab: TabId,
    pub origin: String,
    pub rp_id: String,
    pub kind: ConsentKind,
    /// All known passkeys for this RP (local + Bitwarden). See [`PendingConsent::candidates`].
    keys: Vec<StoredPasskey>,
    /// Index into [`PendingConsent::candidates`].
    pub selected: usize,
    pub save_to: PasskeyStorage,
    /// A Bitwarden lookup is in flight.
    pub loading: bool,
    pub error: Option<String>,
    /// Master password typed in the modal when the vault is locked.
    pub unlock_input: String,
    client_data_hash: [u8; 32],
    reply: Sender<Result<Value, String>>,
}

impl PendingConsent {
    /// Passkeys that can answer this request (for `get`), or existing ones (for `create`).
    pub fn candidates(&self) -> Vec<&StoredPasskey> {
        match &self.kind {
            ConsentKind::Get { allow } => webauthn::matching_keys(&self.rp_id, allow, &self.keys),
            ConsentKind::Create { .. } => {
                self.keys.iter().filter(|k| k.rp_id == self.rp_id).collect()
            }
        }
    }
    pub fn is_create(&self) -> bool {
        matches!(self.kind, ConsentKind::Create { .. })
    }
}

enum Job {
    Status {
        bin: Option<Bitwarden>,
        status: Result<BwStatus, String>,
    },
    Unlock(Result<SessionKey, String>),
    Locked,
    Synced(Result<(), String>),
    Logins {
        url: String,
        result: Result<Vec<LoginItem>, String>,
    },
    Totp(Result<String, String>),
    Passkeys {
        consent: u64,
        result: Result<Vec<StoredPasskey>, String>,
    },
    PasskeySaved {
        consent: u64,
        result: Result<String, String>,
    },
}

/// A create request that is waiting for Bitwarden to confirm the save.
struct PendingSave {
    reply: Sender<Result<Value, String>>,
    response: Value,
    key: StoredPasskey,
}

pub struct PasswordsHost {
    dir: PathBuf,
    wake: WakeFn,
    pub config: PasswordsConfig,
    bw: Option<Bitwarden>,
    pub state: BwUi,
    pub server_url: Option<String>,
    pub user_email: Option<String>,
    session: Option<SessionKey>,
    last_activity: Instant,
    /// Number of jobs running (for spinners).
    pub busy: u32,
    pub last_error: Option<String>,
    pub last_info: Option<String>,
    /// Master password field of the toolbar popover (cleared after use).
    pub unlock_input: String,
    local: Option<LocalPasskeyStore>,
    pub consents: Vec<PendingConsent>,
    saves: Vec<(u64, PendingSave)>,
    /// Logins for `logins_url`.
    pub logins: Vec<LoginItem>,
    pub logins_url: String,
    pub logins_loading: bool,
    clipboard: Option<String>,
    next_id: u64,
    tx: Sender<Job>,
    rx: Receiver<Job>,
}

impl PasswordsHost {
    /// `dir` is the profile directory (`passwords.json`, `passkeys.json`, `passkeys.key`).
    pub fn new(dir: PathBuf, wake: WakeFn) -> Self {
        let config = std::fs::read(dir.join("passwords.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let local = match LocalPasskeyStore::open(&dir) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("passkey store: {e:#}");
                None
            }
        };
        let (tx, rx) = mpsc::channel();
        let mut host = Self {
            dir,
            wake,
            config,
            bw: None,
            state: BwUi::Unknown,
            server_url: None,
            user_email: None,
            session: None,
            last_activity: Instant::now(),
            busy: 0,
            last_error: None,
            last_info: None,
            unlock_input: String::new(),
            local,
            consents: Vec::new(),
            saves: Vec::new(),
            logins: Vec::new(),
            logins_url: String::new(),
            logins_loading: false,
            clipboard: None,
            next_id: 1,
            tx,
            rx,
        };
        host.refresh_status();
        host
    }

    pub fn save_config(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(&self.config) {
            let _ = std::fs::write(self.dir.join("passwords.json"), b);
        }
    }

    /// Path of the detected `bw` binary, if any.
    pub fn bw_binary(&self) -> Option<String> {
        self.bw.as_ref().map(|b| b.bin().display().to_string())
    }

    pub fn is_unlocked(&self) -> bool {
        self.state == BwUi::Unlocked && self.session.is_some()
    }

    pub fn local_passkeys(&self) -> &[StoredPasskey] {
        self.local.as_ref().map(|s| s.all()).unwrap_or(&[])
    }

    pub fn delete_local_passkey(&mut self, credential_id: &[u8]) {
        if let Some(s) = &mut self.local
            && let Err(e) = s.remove(credential_id)
        {
            self.last_error = Some(format!("No se pudo borrar la llave: {e}"));
        }
    }

    /// Text the UI should copy to the clipboard (e.g. a fetched TOTP code).
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    // ---- background jobs ---------------------------------------------------------------

    fn spawn(&mut self, f: impl FnOnce() -> Job + Send + 'static) {
        self.busy += 1;
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f());
            wake();
        });
    }

    /// Re-detect `bw` and fetch `bw status`.
    pub fn refresh_status(&mut self) {
        let path = self.config.bw_path.clone();
        let session = self.session.clone();
        self.spawn(move || {
            let bin = Bitwarden::locate(Some(&path));
            let status = match &bin {
                Some(b) => b.status(session.as_ref()).map_err(|e| format!("{e:#}")),
                None => Err("bw no encontrado".into()),
            };
            Job::Status { bin, status }
        });
    }

    pub fn unlock(&mut self, master_password: String) {
        let Some(bw) = self.bw.clone() else { return };
        self.last_error = None;
        self.spawn(move || Job::Unlock(bw.unlock(&master_password).map_err(|e| format!("{e:#}"))));
    }

    pub fn lock(&mut self) {
        self.session = None;
        self.state = if self.state == BwUi::Unlocked {
            BwUi::Locked
        } else {
            self.state.clone()
        };
        self.logins.clear();
        self.logins_url.clear();
        let Some(bw) = self.bw.clone() else { return };
        self.spawn(move || {
            let _ = bw.lock();
            Job::Locked
        });
    }

    pub fn sync(&mut self) {
        let (Some(bw), Some(s)) = (self.bw.clone(), self.session.clone()) else {
            return;
        };
        self.spawn(move || Job::Synced(bw.sync(&s).map_err(|e| format!("{e:#}"))));
    }

    /// Start fetching the logins that match `url` (no-op while unlocked state is missing or
    /// the same URL was already requested).
    pub fn autofill_candidates(&mut self, url: &str) {
        if !self.is_unlocked() || !url.starts_with("http") {
            return;
        }
        if self.logins_url == url {
            return;
        }
        let (Some(bw), Some(s)) = (self.bw.clone(), self.session.clone()) else {
            return;
        };
        self.logins_url = url.to_string();
        self.logins.clear();
        self.logins_loading = true;
        self.touch();
        let u = url.to_string();
        self.spawn(move || Job::Logins {
            url: u.clone(),
            result: bw.logins_for_url(&s, &u).map_err(|e| format!("{e:#}")),
        });
    }

    /// Force a new lookup next time (after sync or unlock).
    pub fn invalidate_logins(&mut self) {
        self.logins_url.clear();
    }

    /// Fetch the current TOTP code of `item_id`; it lands in [`Self::take_clipboard`].
    pub fn copy_totp(&mut self, item_id: &str) {
        let (Some(bw), Some(s)) = (self.bw.clone(), self.session.clone()) else {
            return;
        };
        let id = item_id.to_string();
        self.touch();
        self.spawn(move || Job::Totp(bw.totp_code(&s, &id).map_err(|e| format!("{e:#}"))));
    }

    /// Drain job results, apply auto-lock, return `true` if the UI should repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(job) = self.rx.try_recv() {
            self.busy = self.busy.saturating_sub(1);
            changed = true;
            self.apply(job);
        }
        let mins = self.config.auto_lock_minutes;
        if mins > 0
            && self.is_unlocked()
            && self.consents.is_empty()
            && self.last_activity.elapsed() > Duration::from_secs(u64::from(mins) * 60)
        {
            self.lock();
            self.last_info = Some("Bitwarden se bloqueó por inactividad".into());
            changed = true;
        }
        changed
    }

    fn apply(&mut self, job: Job) {
        match job {
            Job::Status { bin, status } => {
                self.bw = bin;
                match status {
                    Ok(s) => {
                        self.server_url = s.server_url;
                        self.user_email = s.user_email;
                        self.state = match s.state {
                            VaultState::Unauthenticated => BwUi::Unauthenticated,
                            VaultState::Locked => BwUi::Locked,
                            VaultState::Unlocked => BwUi::Unlocked,
                        };
                        if self.state != BwUi::Unlocked {
                            self.session = None;
                        }
                    }
                    Err(_) if self.bw.is_none() => self.state = BwUi::NotInstalled,
                    Err(e) => self.last_error = Some(e),
                }
            }
            Job::Unlock(res) => match res {
                Ok(key) => {
                    self.session = Some(key);
                    self.state = BwUi::Unlocked;
                    self.touch();
                    self.invalidate_logins();
                    // Consents waiting on the vault can now look up Bitwarden passkeys.
                    let ids: Vec<u64> = self.consents.iter().map(|c| c.id).collect();
                    for id in ids {
                        self.lookup_bw_passkeys(id);
                    }
                    self.sync_after_unlock();
                }
                Err(e) => self.last_error = Some(clean_err(&e)),
            },
            Job::Locked => {}
            Job::Synced(res) => match res {
                Ok(()) => {
                    self.last_info = Some("Bóveda sincronizada".into());
                    self.invalidate_logins();
                }
                Err(e) => self.last_error = Some(clean_err(&e)),
            },
            Job::Logins { url, result } => {
                if url == self.logins_url {
                    self.logins_loading = false;
                    match result {
                        Ok(v) => self.logins = v,
                        Err(e) => {
                            self.last_error = Some(clean_err(&e));
                            self.logins.clear();
                        }
                    }
                }
            }
            Job::Totp(res) => match res {
                Ok(code) if !code.is_empty() => self.clipboard = Some(code),
                Ok(_) => self.last_error = Some("Este elemento no tiene TOTP".into()),
                Err(e) => self.last_error = Some(clean_err(&e)),
            },
            Job::Passkeys { consent, result } => {
                if let Some(c) = self.consents.iter_mut().find(|c| c.id == consent) {
                    c.loading = false;
                    match result {
                        Ok(v) => {
                            c.keys.retain(|k| k.source != PasskeySource::Bitwarden);
                            c.keys.extend(v);
                        }
                        Err(e) => c.error = Some(format!("Bitwarden: {}", clean_err(&e))),
                    }
                }
            }
            Job::PasskeySaved { consent, result } => {
                let Some(pos) = self.saves.iter().position(|(id, _)| *id == consent) else {
                    return;
                };
                let (_, save) = self.saves.remove(pos);
                if let Err(e) = result {
                    // Bitwarden refused (old CLI, offline...): keep the passkey locally so the
                    // registration the site just accepted is not lost.
                    log::warn!("saving passkey to Bitwarden failed: {e}");
                    match self.local.as_mut().map(|s| s.add(save.key)) {
                        Some(Ok(())) => self.last_info = Some(
                            "No se pudo guardar en Bitwarden; la llave se guardó en este navegador"
                                .into(),
                        ),
                        _ => {
                            let _ = save.reply.send(Err(
                                "NotAllowedError: no se pudo guardar la llave de acceso".into(),
                            ));
                            return;
                        }
                    }
                }
                let _ = save.reply.send(Ok(save.response));
            }
        }
    }

    fn sync_after_unlock(&mut self) {
        // A silent sync keeps the vault fresh; failures are surfaced by `Synced`.
        self.sync();
    }

    fn lookup_bw_passkeys(&mut self, consent_id: u64) {
        let (Some(bw), Some(s)) = (self.bw.clone(), self.session.clone()) else {
            return;
        };
        let Some(c) = self.consents.iter_mut().find(|c| c.id == consent_id) else {
            return;
        };
        c.loading = true;
        let rp = c.rp_id.clone();
        self.spawn(move || Job::Passkeys {
            consent: consent_id,
            result: bw.passkeys_for_rp(&s, &rp).map_err(|e| format!("{e:#}")),
        });
    }

    // ---- passkey consent ---------------------------------------------------------------

    /// Handle a `webauthn` bridge call. Always answers later, after the user decided.
    pub fn handle_bridge(&mut self, payload: Value, origin: &str, tab: TabId) -> Reply {
        match self.begin_consent(&payload, origin, tab) {
            Ok(rx) => Reply::Later(rx),
            Err(e) => Reply::Now(json!({ "__error": format!("NotAllowedError: {e}") })),
        }
    }

    fn begin_consent(
        &mut self,
        p: &Value,
        origin: &str,
        tab: TabId,
    ) -> Result<Receiver<Result<Value, String>>, String> {
        // Origin = the tab's URL origin, never anything the page says.
        let origin = url::Url::parse(origin)
            .map_err(|_| "origen no válido".to_string())?
            .origin()
            .ascii_serialization();
        if self.consents.iter().any(|c| c.tab == tab) {
            return Err("ya hay una solicitud de llave de acceso pendiente".into());
        }
        if self.consents.len() >= 8 {
            return Err("demasiadas solicitudes pendientes".into());
        }
        let op = p["op"].as_str().unwrap_or("");
        let cdj = b64::dec(p["clientDataJSON"].as_str().ok_or("falta clientDataJSON")?)
            .map_err(|e| e.to_string())?;
        if cdj.len() > 4096 {
            return Err("clientDataJSON demasiado grande".into());
        }
        let ids = |v: &Value| -> Vec<Vec<u8>> {
            v.as_array()
                .into_iter()
                .flatten()
                .take(64)
                .filter_map(|d| d["id"].as_str().and_then(|s| b64::dec(s).ok()))
                .collect()
        };
        let (rp_id, kind) = match op {
            "create" => {
                webauthn::validate_client_data(&cdj, "webauthn.create", &origin)
                    .map_err(|e| e.to_string())?;
                let rp_id = p["rp"]["id"].as_str().unwrap_or("").to_string();
                let user_id =
                    b64::dec(p["user"]["id"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
                let sel = &p["authenticatorSelection"];
                let discoverable =
                    matches!(sel["residentKey"].as_str(), Some("required" | "preferred"))
                        || sel["requireResidentKey"] == true;
                let user = UserEntity {
                    id: user_id,
                    name: cap(p["user"]["name"].as_str().unwrap_or(""), 256),
                    display_name: cap(p["user"]["displayName"].as_str().unwrap_or(""), 256),
                };
                (
                    rp_id,
                    ConsentKind::Create {
                        rp_name: cap(p["rp"]["name"].as_str().unwrap_or(""), 256),
                        user,
                        exclude: ids(&p["excludeCredentials"]),
                        discoverable,
                    },
                )
            }
            "get" => {
                webauthn::validate_client_data(&cdj, "webauthn.get", &origin)
                    .map_err(|e| e.to_string())?;
                (
                    p["rpId"].as_str().unwrap_or("").to_string(),
                    ConsentKind::Get {
                        allow: ids(&p["allowCredentials"]),
                    },
                )
            }
            _ => return Err("operación desconocida".into()),
        };
        let rp_id = rp_id.to_ascii_lowercase();
        webauthn::validate_rp_id(&rp_id, &origin).map_err(|e| e.to_string())?;

        let (tx, rx) = mpsc::channel();
        let id = self.next_id;
        self.next_id += 1;
        let keys = self
            .local
            .as_ref()
            .map(|s| s.for_rp(&rp_id))
            .unwrap_or_default();
        let save_to =
            if self.is_unlocked() || self.state == BwUi::Locked || self.state == BwUi::Unknown {
                self.config.default_store
            } else {
                PasskeyStorage::Local
            };
        self.consents.push(PendingConsent {
            id,
            tab,
            origin,
            rp_id,
            kind,
            keys,
            selected: 0,
            save_to,
            loading: false,
            error: None,
            unlock_input: String::new(),
            client_data_hash: webauthn::client_data_hash(&cdj),
            reply: tx,
        });
        if self.is_unlocked() {
            self.lookup_bw_passkeys(id);
        }
        (self.wake)();
        Ok(rx)
    }

    /// The user pressed "Permitir" on consent `id`.
    pub fn approve_consent(&mut self, id: u64) {
        let Some(pos) = self.consents.iter().position(|c| c.id == id) else {
            return;
        };
        let c = self.consents.remove(pos);
        let result = match &c.kind {
            ConsentKind::Create { .. } => self.finish_create(&c, id),
            ConsentKind::Get { .. } => self.finish_get(&c).map(Some),
        };
        match result {
            Ok(Some(v)) => {
                let _ = c.reply.send(Ok(v));
            }
            Ok(None) => {} // answer pending on Bitwarden (see `saves`)
            Err(e) => {
                let _ = c.reply.send(Err(format!("NotAllowedError: {e}")));
            }
        }
        (self.wake)();
    }

    pub fn cancel_consent(&mut self, id: u64) {
        if let Some(pos) = self.consents.iter().position(|c| c.id == id) {
            let c = self.consents.remove(pos);
            let _ = c.reply.send(Err(
                "NotAllowedError: The operation either timed out or was not allowed.".into(),
            ));
        }
    }

    /// Cancel everything a tab asked for (tab closed or navigated away).
    pub fn cancel_for_tab(&mut self, tab: TabId) {
        let ids: Vec<u64> = self
            .consents
            .iter()
            .filter(|c| c.tab == tab)
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.cancel_consent(id);
        }
    }

    fn finish_get(&mut self, c: &PendingConsent) -> Result<Value, String> {
        let ConsentKind::Get { allow } = &c.kind else {
            unreachable!()
        };
        let key = webauthn::matching_keys(&c.rp_id, allow, &c.keys)
            .get(c.selected)
            .map(|k| (*k).clone())
            .ok_or("no hay una llave de acceso seleccionada")?;
        let local = key.source == PasskeySource::Local;
        let mut signing = key.clone();
        if local {
            signing.counter = key.counter.saturating_add(1);
        }
        let a = webauthn::get_assertion(
            &c.rp_id,
            &c.origin,
            &c.client_data_hash,
            allow,
            &signing,
            !local,
        )
        .map_err(|e| e.to_string())?;
        if local && let Some(s) = &mut self.local {
            let _ = s.set_counter(&key.credential_id, signing.counter);
        }
        Ok(json!({
            "credentialId": b64::enc(&a.credential_id),
            "authenticatorData": b64::enc(&a.authenticator_data),
            "signature": b64::enc(&a.signature),
            "userHandle": if a.user_handle.is_empty() { Value::Null } else { Value::String(b64::enc(&a.user_handle)) },
        }))
    }

    /// `Ok(None)` when the reply must wait for a Bitwarden save.
    fn finish_create(&mut self, c: &PendingConsent, id: u64) -> Result<Option<Value>, String> {
        let ConsentKind::Create {
            rp_name,
            user,
            exclude,
            discoverable,
        } = &c.kind
        else {
            unreachable!()
        };
        let to_bw =
            c.save_to == PasskeyStorage::Bitwarden && self.is_unlocked() && self.bw.is_some();
        let source = if to_bw {
            PasskeySource::Bitwarden
        } else {
            PasskeySource::Local
        };
        let r = webauthn::make_credential(&MakeCredential {
            rp_id: &c.rp_id,
            rp_name,
            origin: &c.origin,
            client_data_hash: &c.client_data_hash,
            user,
            exclude_list: exclude,
            existing: &c.keys,
            discoverable: *discoverable,
            backed_up: to_bw,
            source,
        })
        .map_err(|e| e.to_string())?;
        let response = json!({
            "credentialId": b64::enc(&r.credential_id),
            "attestationObject": b64::enc(&r.attestation_object),
            "authData": b64::enc(&r.auth_data),
            "publicKey": b64::enc(&r.public_key_spki),
            "publicKeyAlgorithm": webauthn::COSE_ES256,
            "transports": ["internal"],
        });
        if to_bw {
            let (bw, s) = (self.bw.clone().unwrap(), self.session.clone().unwrap());
            // Add to an existing login of the same site/user when one is known, else create one.
            let key = r.passkey;
            self.saves.push((
                id,
                PendingSave {
                    reply: c.reply.clone(),
                    response,
                    key: key.clone(),
                },
            ));
            self.spawn(move || Job::PasskeySaved {
                consent: id,
                result: bw
                    .save_passkey(&s, &key, None)
                    .map_err(|e| format!("{e:#}")),
            });
            Ok(None)
        } else {
            let store = self
                .local
                .as_mut()
                .ok_or("el almacén local de llaves no está disponible")?;
            store
                .add(r.passkey)
                .map_err(|e| format!("no se pudo guardar la llave: {e}"))?;
            Ok(Some(response))
        }
    }
}

fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `bw` errors are noisy; keep the first line.
fn clean_err(e: &str) -> String {
    e.lines()
        .next()
        .unwrap_or(e)
        .trim()
        .chars()
        .take(200)
        .collect()
}

/// JS that fills the login form of the page: the focused field's form when focus is in a
/// form, otherwise the first visible password field. Sets values through the native setter
/// and fires `input`/`change`. Never submits. Returns `"ok"`, `"user-only"` or `"no-form"`.
pub fn fill_script(username: &str, password: &str) -> String {
    // serde_json escapes quotes/backslashes/control chars; U+2028/2029 are valid in JSON
    // but not in older JS string literals, so escape them too.
    let lit = |s: &str| {
        serde_json::to_string(s)
            .unwrap_or_else(|_| "\"\"".into())
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    };
    format!(
        r#"(function(u,p){{
  var setter=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
  function vis(e){{var s=getComputedStyle(e);return !e.disabled&&!e.readOnly&&s.visibility!=='hidden'&&s.display!=='none'&&(e.offsetWidth||e.offsetHeight||e.getClientRects().length);}}
  function fill(e,v){{e.focus();setter.call(e,v);e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));}}
  var act=document.activeElement, root=(act&&act.form)||document;
  var pws=[].filter.call(root.querySelectorAll('input[type=password]'),vis);
  if(!pws.length&&root!==document) pws=[].filter.call(document.querySelectorAll('input[type=password]'),vis);
  var pw=(act&&pws.indexOf(act)>=0)?act:pws[0];
  var scope=(pw&&pw.form)||root;
  var cand=[].filter.call(scope.querySelectorAll('input'),function(e){{
    var t=(e.getAttribute('type')||'text').toLowerCase();
    return ['text','email','tel','url',''].indexOf(t)>=0&&vis(e);}});
  var user=null;
  if(pw){{for(var i=0;i<cand.length;i++){{if(cand[i].compareDocumentPosition(pw)&Node.DOCUMENT_POSITION_FOLLOWING)user=cand[i];}}}}
  else{{user=(act&&cand.indexOf(act)>=0)?act:cand.filter(function(e){{return /user|email|login/i.test((e.autocomplete||'')+e.name+e.id);}})[0]||cand[0]||null;}}
  if(user&&u)fill(user,u);
  if(pw){{fill(pw,p);return 'ok';}}
  return user?'user-only':'no-form';
}})({},{})"#,
        lit(username),
        lit(password)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> (PasswordsHost, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut h = PasswordsHost::new(dir.path().to_path_buf(), std::sync::Arc::new(|| {}));
        h.state = BwUi::NotInstalled;
        h.bw = None;
        (h, dir)
    }

    fn client_data(ty: &str, origin: &str) -> String {
        b64::enc(
            json!({"type": ty, "challenge": "abc", "origin": origin})
                .to_string()
                .as_bytes(),
        )
    }

    #[test]
    fn rejects_bad_origin_and_rp() {
        let (mut h, _d) = host();
        let p = json!({"op":"get","clientDataJSON":client_data("webauthn.get","https://evil.com"),"rpId":"example.com"});
        assert!(
            matches!(h.handle_bridge(p, "https://evil.com", 1), Reply::Now(v) if v["__error"].is_string())
        );
        let p = json!({"op":"get","clientDataJSON":client_data("webauthn.get","https://a.com"),"rpId":"b.com"});
        assert!(matches!(
            h.handle_bridge(p, "https://a.com", 1),
            Reply::Now(_)
        ));
        // page claims another origin in clientDataJSON
        let p = json!({"op":"get","clientDataJSON":client_data("webauthn.get","https://example.com"),"rpId":"a.com"});
        assert!(matches!(
            h.handle_bridge(p, "https://a.com", 1),
            Reply::Now(_)
        ));
        assert!(h.consents.is_empty());
    }

    #[test]
    fn create_then_get_locally() {
        let (mut h, _d) = host();
        h.config.default_store = PasskeyStorage::Local;
        let create = json!({"op":"create","clientDataJSON":client_data("webauthn.create","https://example.com"),
            "rp":{"id":"example.com","name":"Ex"},"user":{"id":"AQIDBA","name":"ana","displayName":"Ana"},
            "excludeCredentials":[],"authenticatorSelection":{"residentKey":"required"}});
        let Reply::Later(rx) = h.handle_bridge(create, "https://example.com/login", 7) else {
            panic!()
        };
        assert_eq!(h.consents.len(), 1);
        // a second request from the same tab is refused while the first is pending
        assert!(matches!(
            h.handle_bridge(json!({"op":"get"}), "https://example.com", 7),
            Reply::Now(_)
        ));
        let id = h.consents[0].id;
        h.approve_consent(id);
        let v = rx.try_recv().unwrap().unwrap();
        assert!(v["attestationObject"].is_string());
        assert_eq!(h.local_passkeys().len(), 1);

        let get = json!({"op":"get","clientDataJSON":client_data("webauthn.get","https://example.com"),"rpId":"example.com","allowCredentials":[]});
        let Reply::Later(rx) = h.handle_bridge(get, "https://example.com", 7) else {
            panic!()
        };
        assert_eq!(h.consents[0].candidates().len(), 1);
        let id = h.consents[0].id;
        h.approve_consent(id);
        let v = rx.try_recv().unwrap().unwrap();
        assert!(v["signature"].is_string());
        assert_eq!(h.local_passkeys()[0].counter, 1);

        // cancel path
        let get = json!({"op":"get","clientDataJSON":client_data("webauthn.get","https://example.com"),"rpId":"example.com"});
        let Reply::Later(rx) = h.handle_bridge(get, "https://example.com", 7) else {
            panic!()
        };
        h.cancel_for_tab(7);
        assert!(
            rx.try_recv()
                .unwrap()
                .unwrap_err()
                .starts_with("NotAllowedError")
        );
    }

    #[test]
    fn fill_script_escapes() {
        let s = fill_script("a\"</script>'\n", "p\u{2028}w\\");
        assert!(s.contains(r#""a\"</script>'\n""#));
        assert!(s.contains("\\u2028"));
    }
}
