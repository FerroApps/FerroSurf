use serde::{Deserialize, Serialize};

use crate::b64::serde_bytes_b64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PasskeySource {
    #[default]
    Local,
    Bitwarden,
}

/// A passkey with its private key (PKCS#8 DER, P-256).
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredPasskey {
    #[serde(with = "serde_bytes_b64")]
    pub credential_id: Vec<u8>,
    pub rp_id: String,
    #[serde(default)]
    pub rp_name: String,
    #[serde(with = "serde_bytes_b64")]
    pub user_handle: Vec<u8>,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub user_display_name: String,
    /// PKCS#8 private key. Never serialized by [`crate::store`] in clear text.
    #[serde(skip)]
    pub private_key_pkcs8: Vec<u8>,
    #[serde(default)]
    pub counter: u32,
    #[serde(default = "yes")]
    pub discoverable: bool,
    #[serde(default)]
    pub created: i64,
    #[serde(skip)]
    pub source: PasskeySource,
    /// Bitwarden vault item holding this credential.
    #[serde(skip)]
    pub item_id: Option<String>,
}

fn yes() -> bool {
    true
}

impl std::fmt::Debug for StoredPasskey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredPasskey")
            .field("rp_id", &self.rp_id)
            .field("user_name", &self.user_name)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}
