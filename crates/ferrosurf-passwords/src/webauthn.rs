//! Software WebAuthn authenticator ("platform authenticator").
//!
//! ES256 only, `none` attestation. User presence/verification are reported as true: the
//! browser must have asked the user (consent UI) before calling into this module.

use anyhow::{Result, bail};
use ciborium::value::Value as Cbor;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use sha2::{Digest, Sha256};
use url::Url;

use crate::passkey::{PasskeySource, StoredPasskey};

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
pub const FLAG_BE: u8 = 0x08;
pub const FLAG_BS: u8 = 0x10;
pub const FLAG_AT: u8 = 0x40;

/// COSE algorithm identifier for ES256.
pub const COSE_ES256: i64 = -7;

#[derive(Clone, Debug, Default)]
pub struct UserEntity {
    pub id: Vec<u8>,
    pub name: String,
    pub display_name: String,
}

pub struct AttestationResult {
    pub credential_id: Vec<u8>,
    /// CBOR `{fmt:"none", attStmt:{}, authData}`.
    pub attestation_object: Vec<u8>,
    pub auth_data: Vec<u8>,
    /// COSE_Key (EC2, P-256).
    pub public_key_cose: Vec<u8>,
    /// SubjectPublicKeyInfo DER (what `getPublicKey()` returns).
    pub public_key_spki: Vec<u8>,
    /// The new credential, to be persisted by the caller.
    pub passkey: StoredPasskey,
}

pub struct AssertionResult {
    pub credential_id: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    /// ASN.1 DER ECDSA signature.
    pub signature: Vec<u8>,
    pub user_handle: Vec<u8>,
}

/// Request for [`make_credential`].
pub struct MakeCredential<'a> {
    pub rp_id: &'a str,
    pub rp_name: &'a str,
    pub origin: &'a str,
    pub client_data_hash: &'a [u8],
    pub user: &'a UserEntity,
    pub exclude_list: &'a [Vec<u8>],
    /// Credentials already known for this RP (to honour `exclude_list`).
    pub existing: &'a [StoredPasskey],
    pub discoverable: bool,
    /// Report the credential as backed up (synced), e.g. stored in Bitwarden.
    pub backed_up: bool,
    pub source: PasskeySource,
}

/// Validate that `rp_id` may be used from `origin`: https (or http on localhost), and the
/// RP ID is the origin's host or a dot-suffix of it. Public suffixes are approximated by
/// requiring a dot in `rp_id` unless it equals the host.
pub fn validate_rp_id(rp_id: &str, origin: &str) -> Result<()> {
    let url = Url::parse(origin).map_err(|_| anyhow::anyhow!("origin is not a URL"))?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        bail!("origin has no host");
    }
    let local = host == "localhost" || host.ends_with(".localhost");
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => bail!("WebAuthn requires a secure origin"),
    }
    let rp = rp_id.to_ascii_lowercase();
    if rp.is_empty()
        || rp.contains('/')
        || rp.contains(':')
        || rp.starts_with('.')
        || rp.ends_with('.')
    {
        bail!("invalid RP ID");
    }
    let is_ip = matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)));
    if is_ip && rp != host {
        bail!("RP ID does not match origin");
    }
    if rp == host {
        return Ok(());
    }
    if !rp.contains('.') {
        bail!("RP ID is a public suffix");
    }
    if host.ends_with(&format!(".{rp}")) {
        Ok(())
    } else {
        bail!("RP ID is not a registrable suffix of the origin")
    }
}

pub fn client_data_hash(client_data_json: &[u8]) -> [u8; 32] {
    Sha256::digest(client_data_json).into()
}

/// Check a (page-built, untrusted) clientDataJSON against what the browser knows.
pub fn validate_client_data(json: &[u8], expected_type: &str, origin: &str) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(json)?;
    if v["type"] != expected_type {
        bail!("clientDataJSON type mismatch");
    }
    if v["origin"].as_str() != Some(origin) {
        bail!("clientDataJSON origin mismatch");
    }
    if v["challenge"].as_str().is_none_or(str::is_empty) {
        bail!("clientDataJSON has no challenge");
    }
    if v.get("crossOrigin").is_some_and(|c| c != false) {
        bail!("cross-origin requests are not supported");
    }
    Ok(())
}

fn rp_id_hash(rp_id: &str) -> [u8; 32] {
    Sha256::digest(rp_id.as_bytes()).into()
}

fn cbor_bytes(v: &Cbor) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(v, &mut out).expect("CBOR encode to Vec cannot fail");
    out
}

fn cbor_int(i: i64) -> Cbor {
    Cbor::Integer(i.into())
}

/// COSE_Key for an ES256 public key, keys in CTAP2 canonical order.
pub fn cose_public_key(vk: &VerifyingKey) -> Vec<u8> {
    let point = vk.to_sec1_point(false);
    let x = point.x().expect("uncompressed point").to_vec();
    let y = point.y().expect("uncompressed point").to_vec();
    cbor_bytes(&Cbor::Map(vec![
        (cbor_int(1), cbor_int(2)),
        (cbor_int(3), cbor_int(COSE_ES256)),
        (cbor_int(-1), cbor_int(1)),
        (cbor_int(-2), Cbor::Bytes(x)),
        (cbor_int(-3), Cbor::Bytes(y)),
    ]))
}

/// Attestation object with `none` format.
pub fn attestation_object_none(auth_data: &[u8]) -> Vec<u8> {
    cbor_bytes(&Cbor::Map(vec![
        (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
        (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
        (
            Cbor::Text("authData".into()),
            Cbor::Bytes(auth_data.to_vec()),
        ),
    ]))
}

fn flags(extra: u8, backed_up: bool) -> u8 {
    let mut f = FLAG_UP | FLAG_UV | extra;
    if backed_up {
        f |= FLAG_BE | FLAG_BS;
    }
    f
}

/// Create a new credential. The returned `passkey` must be persisted by the caller.
pub fn make_credential(req: &MakeCredential<'_>) -> Result<AttestationResult> {
    validate_rp_id(req.rp_id, req.origin)?;
    if req.client_data_hash.len() != 32 {
        bail!("client data hash must be 32 bytes");
    }
    if req.user.id.is_empty() || req.user.id.len() > 64 {
        bail!("invalid user handle");
    }
    if req.exclude_list.iter().any(|id| {
        req.existing
            .iter()
            .any(|k| k.rp_id == req.rp_id && &k.credential_id == id)
    }) {
        bail!("credential excluded");
    }

    let sk = generate_signing_key()?;
    let vk = *sk.verifying_key();
    let mut credential_id = [0u8; 16];
    crate::random_bytes(&mut credential_id)?;
    // UUID v4 shaped, so Bitwarden stores it as a GUID like its own passkeys.
    credential_id[6] = (credential_id[6] & 0x0f) | 0x40;
    credential_id[8] = (credential_id[8] & 0x3f) | 0x80;
    let credential_id = credential_id.to_vec();

    let cose = cose_public_key(&vk);
    let mut auth_data = Vec::with_capacity(37 + 18 + credential_id.len() + cose.len());
    auth_data.extend_from_slice(&rp_id_hash(req.rp_id));
    auth_data.push(flags(FLAG_AT, req.backed_up));
    auth_data.extend_from_slice(&0u32.to_be_bytes());
    auth_data.extend_from_slice(&[0u8; 16]); // AAGUID: none
    auth_data.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
    auth_data.extend_from_slice(&credential_id);
    auth_data.extend_from_slice(&cose);

    let pkcs8 = sk
        .to_pkcs8_der()
        .map_err(|e| anyhow::anyhow!("pkcs8: {e}"))?;
    let spki = vk
        .to_public_key_der()
        .map_err(|e| anyhow::anyhow!("spki: {e}"))?
        .as_bytes()
        .to_vec();
    let passkey = StoredPasskey {
        credential_id: credential_id.clone(),
        rp_id: req.rp_id.to_string(),
        rp_name: req.rp_name.to_string(),
        user_handle: req.user.id.clone(),
        user_name: req.user.name.clone(),
        user_display_name: req.user.display_name.clone(),
        private_key_pkcs8: pkcs8.as_bytes().to_vec(),
        counter: 0,
        discoverable: req.discoverable,
        created: crate::now_secs(),
        source: req.source,
        item_id: None,
    };
    Ok(AttestationResult {
        credential_id,
        attestation_object: attestation_object_none(&auth_data),
        auth_data,
        public_key_cose: cose,
        public_key_spki: spki,
        passkey,
    })
}

fn generate_signing_key() -> Result<SigningKey> {
    for _ in 0..16 {
        let mut b = [0u8; 32];
        crate::random_bytes(&mut b)?;
        if let Ok(k) = SigningKey::from_slice(&b) {
            return Ok(k);
        }
    }
    bail!("could not generate a key")
}

/// Sign an assertion with `key`. `key.counter` is used as the signature counter (callers
/// that track counters increment it before calling; synced passkeys stay at 0).
pub fn get_assertion(
    rp_id: &str,
    origin: &str,
    client_data_hash: &[u8],
    allow_list: &[Vec<u8>],
    key: &StoredPasskey,
    backed_up: bool,
) -> Result<AssertionResult> {
    validate_rp_id(rp_id, origin)?;
    if key.rp_id != rp_id {
        bail!("credential belongs to another RP");
    }
    if !allow_list.is_empty() && !allow_list.contains(&key.credential_id) {
        bail!("credential not in allow list");
    }
    if client_data_hash.len() != 32 {
        bail!("client data hash must be 32 bytes");
    }
    let sk = SigningKey::from_pkcs8_der(&key.private_key_pkcs8)
        .map_err(|e| anyhow::anyhow!("invalid private key: {e}"))?;
    let mut auth_data = Vec::with_capacity(37);
    auth_data.extend_from_slice(&rp_id_hash(rp_id));
    auth_data.push(flags(0, backed_up));
    auth_data.extend_from_slice(&key.counter.to_be_bytes());
    let mut msg = auth_data.clone();
    msg.extend_from_slice(client_data_hash);
    let sig: Signature = sk.sign(&msg);
    Ok(AssertionResult {
        credential_id: key.credential_id.clone(),
        authenticator_data: auth_data,
        signature: sig.to_der().as_bytes().to_vec(),
        user_handle: key.user_handle.clone(),
    })
}

/// Keys usable for an assertion given an allow list (empty list = discoverable only).
pub fn matching_keys<'a>(
    rp_id: &str,
    allow_list: &[Vec<u8>],
    keys: &'a [StoredPasskey],
) -> Vec<&'a StoredPasskey> {
    keys.iter()
        .filter(|k| k.rp_id == rp_id)
        .filter(|k| {
            if allow_list.is_empty() {
                k.discoverable
            } else {
                allow_list.contains(&k.credential_id)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;

    fn user() -> UserEntity {
        UserEntity {
            id: vec![1, 2, 3, 4],
            name: "ana".into(),
            display_name: "Ana".into(),
        }
    }

    fn mk(origin: &str, rp: &str) -> Result<AttestationResult> {
        let u = user();
        make_credential(&MakeCredential {
            rp_id: rp,
            rp_name: "Example",
            origin,
            client_data_hash: &[7u8; 32],
            user: &u,
            exclude_list: &[],
            existing: &[],
            discoverable: true,
            backed_up: true,
            source: PasskeySource::Local,
        })
    }

    #[test]
    fn rp_id_validation() {
        assert!(validate_rp_id("example.com", "https://example.com").is_ok());
        assert!(validate_rp_id("example.com", "https://login.example.com/x").is_ok());
        assert!(validate_rp_id("EXAMPLE.com", "https://login.example.com").is_ok());
        assert!(validate_rp_id("example.com", "http://example.com").is_err());
        assert!(validate_rp_id("localhost", "http://localhost:8080").is_ok());
        assert!(validate_rp_id("example.com", "https://notexample.com").is_err());
        assert!(validate_rp_id("example.com", "https://example.com.evil.org").is_err());
        assert!(validate_rp_id("com", "https://example.com").is_err());
        assert!(validate_rp_id("login.example.com", "https://example.com").is_err());
        assert!(validate_rp_id("1.2.3.4", "https://1.2.3.4").is_ok());
        assert!(validate_rp_id("3.4", "https://1.2.3.4").is_err());
        assert!(validate_rp_id("", "https://example.com").is_err());
        assert!(validate_rp_id("example.com", "ftp://example.com").is_err());
        assert!(validate_rp_id("example.com:443", "https://example.com").is_err());
    }

    #[test]
    fn client_data_checks() {
        let ok = br#"{"type":"webauthn.get","challenge":"abc","origin":"https://a.com","crossOrigin":false}"#;
        assert!(validate_client_data(ok, "webauthn.get", "https://a.com").is_ok());
        assert!(validate_client_data(ok, "webauthn.create", "https://a.com").is_err());
        assert!(validate_client_data(ok, "webauthn.get", "https://b.com").is_err());
    }

    #[test]
    fn attestation_roundtrip() {
        let r = mk("https://example.com", "example.com").unwrap();
        // attestation object decodes and carries authData
        let v: Cbor = ciborium::de::from_reader(&r.attestation_object[..]).unwrap();
        let Cbor::Map(m) = v else { panic!() };
        let get = |k: &str| {
            m.iter()
                .find(|(a, _)| a.as_text() == Some(k))
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(get("fmt").as_text(), Some("none"));
        assert_eq!(get("attStmt"), Cbor::Map(vec![]));
        let Cbor::Bytes(ad) = get("authData") else {
            panic!()
        };
        assert_eq!(ad, r.auth_data);
        // layout
        assert_eq!(&ad[..32], &rp_id_hash("example.com"));
        assert_eq!(ad[32] & FLAG_AT, FLAG_AT);
        assert_eq!(ad[32] & (FLAG_UP | FLAG_UV), FLAG_UP | FLAG_UV);
        assert_eq!(&ad[33..37], &[0, 0, 0, 0]);
        assert_eq!(&ad[37..53], &[0u8; 16]);
        let n = u16::from_be_bytes([ad[53], ad[54]]) as usize;
        assert_eq!(n, 16);
        assert_eq!(&ad[55..55 + n], &r.credential_id[..]);
        assert_eq!(&ad[55 + n..], &r.public_key_cose[..]);
        // COSE key decodes with the right shape
        let Cbor::Map(c) = ciborium::de::from_reader(&r.public_key_cose[..]).unwrap() else {
            panic!()
        };
        assert_eq!(c.len(), 5);
        assert_eq!(c[1].1, cbor_int(-7));
        let Cbor::Bytes(x) = &c[3].1 else { panic!() };
        assert_eq!(x.len(), 32);
    }

    #[test]
    fn assertion_signature_verifies() {
        let r = mk("https://example.com", "example.com").unwrap();
        let hash = client_data_hash(b"{\"type\":\"webauthn.get\"}");
        let a = get_assertion(
            "example.com",
            "https://example.com",
            &hash,
            &[],
            &r.passkey,
            true,
        )
        .unwrap();
        assert_eq!(a.credential_id, r.credential_id);
        assert_eq!(a.user_handle, vec![1, 2, 3, 4]);
        let sk = SigningKey::from_pkcs8_der(&r.passkey.private_key_pkcs8).unwrap();
        let vk = *sk.verifying_key();
        let mut msg = a.authenticator_data.clone();
        msg.extend_from_slice(&hash);
        let sig = Signature::from_der(&a.signature).unwrap();
        vk.verify(&msg, &sig).unwrap();
        // tampered message fails
        msg[0] ^= 1;
        assert!(vk.verify(&msg, &sig).is_err());
        // wrong rp / allow list rejected
        assert!(
            get_assertion(
                "other.com",
                "https://other.com",
                &hash,
                &[],
                &r.passkey,
                true
            )
            .is_err()
        );
        assert!(
            get_assertion(
                "example.com",
                "https://example.com",
                &hash,
                &[vec![9]],
                &r.passkey,
                true
            )
            .is_err()
        );
    }

    #[test]
    fn exclude_list_and_bad_origin() {
        let r = mk("https://example.com", "example.com").unwrap();
        let u = user();
        let ex = vec![r.credential_id.clone()];
        let existing = [r.passkey.clone()];
        let res = make_credential(&MakeCredential {
            rp_id: "example.com",
            rp_name: "",
            origin: "https://example.com",
            client_data_hash: &[0; 32],
            user: &u,
            exclude_list: &ex,
            existing: &existing,
            discoverable: true,
            backed_up: false,
            source: PasskeySource::Local,
        });
        assert!(res.is_err());
        assert!(mk("http://example.com", "example.com").is_err());
    }
}
