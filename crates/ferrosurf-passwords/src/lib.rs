//! Password manager and passkey support for FerroSurf.
//!
//! * [`bitwarden`]: client for the official `bw` CLI (blocking; call from worker threads).
//! * [`store`]: local passkey store used when Bitwarden is unavailable.
//! * [`webauthn`]: software WebAuthn "platform authenticator" (ES256, `none` attestation).
//! * [`WEBAUTHN_POLYFILL_JS`]: page script that installs `navigator.credentials` on top of
//!   the browser bridge, because Servo has no WebAuthn implementation.
//!
//! This crate has no browser-engine dependencies.

pub mod bitwarden;
pub mod passkey;
pub mod store;
pub mod webauthn;

pub use passkey::{PasskeySource, StoredPasskey};

/// JS installed (as a user script, after the bridge client) in every page.
pub const WEBAUTHN_POLYFILL_JS: &str = include_str!("polyfill.js");

/// base64url (no padding) helpers shared by the modules.
pub mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};

    pub fn enc(data: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(data)
    }

    /// Decode base64 in any of the common flavours (url-safe or standard, padded or not).
    pub fn dec(s: &str) -> anyhow::Result<Vec<u8>> {
        let s = s.trim();
        for e in [&URL_SAFE_NO_PAD, &URL_SAFE, &STANDARD_NO_PAD, &STANDARD] {
            if let Ok(v) = e.decode(s) {
                return Ok(v);
            }
        }
        anyhow::bail!("invalid base64")
    }

    /// serde adapter: `Vec<u8>` as base64url string.
    pub mod serde_bytes_b64 {
        use serde::{Deserialize, Deserializer, Serializer};
        pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&super::enc(v))
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
            let s = String::deserialize(d)?;
            super::dec(&s).map_err(serde::de::Error::custom)
        }
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fill `buf` with OS randomness.
pub(crate) fn random_bytes(buf: &mut [u8]) -> anyhow::Result<()> {
    getrandom::fill(buf).map_err(|e| anyhow::anyhow!("random: {e}"))
}
