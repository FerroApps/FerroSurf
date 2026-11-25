//! The plugin ABI, version 1: hook and host-call ids, error codes and the
//! MessagePack payloads exchanged over linear memory.
//!
//! Every payload is MessagePack with structs encoded as **maps keyed by
//! field name** (`rmp_serde::to_vec_named`), so fields can be added in a
//! later minor revision without breaking older plugins: unknown keys are
//! ignored and missing keys default. Enums are encoded as lowercase strings
//! (or maps with a `"type"` key when they carry data).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// ABI revision implemented by this crate. A plugin's manifest names the
/// version it was built for (`api_version`) and its `fs_abi_version`
/// export must return the same number.
pub const ABI_VERSION: u32 = 1;

/// Name of the import module every host function lives in.
pub const IMPORT_MODULE: &str = "ferrosurf";

/// Hook ids: the first argument of the guest's `fs_call` export.
pub mod hook {
    /// Input [`super::LoadInfo`]; no output.
    pub const LOAD: i32 = 1;
    /// No input, no output. The instance is dropped afterwards.
    pub const UNLOAD: i32 = 2;
    /// Input [`super::SettingChanged`]; no output.
    pub const SETTING_CHANGED: i32 = 3;
    /// Input [`super::HttpResponse`]; no output.
    pub const HTTP_RESPONSE: i32 = 4;
    /// Input [`super::RequestInput`]; output [`super::RequestOutput`].
    /// Synchronous, called only for requests matching `request_filters`.
    pub const REQUEST: i32 = 5;
    /// Input [`super::NavigationEvent`]; no output. Asynchronous.
    pub const NAVIGATION: i32 = 6;
    /// Input [`super::CommandInput`]; no output. A user action.
    pub const COMMAND: i32 = 7;
    /// Input [`super::OmniboxInput`]; output [`super::OmniboxOutput`].
    pub const OMNIBOX: i32 = 8;
    /// Input [`super::McpToolInput`]; output [`super::McpToolOutput`].
    pub const MCP_TOOL: i32 = 9;
    /// Input [`super::PageMessageInput`]; output [`super::PageMessageOutput`].
    pub const PAGE_MESSAGE: i32 = 10;
}

/// Host-call operation ids: the first argument of the `host_call` import.
pub mod op {
    /// [`super::LogReq`] -> nothing.
    pub const LOG: i32 = 1;
    /// [`super::TextReq`] -> nothing. One per hook call.
    pub const TOAST: i32 = 2;
    /// [`super::KeyReq`] -> `Option<`[`super::SettingValue`]`>`.
    pub const GET_SETTING: i32 = 3;
    /// [`super::SetSettingReq`] -> nothing.
    pub const SET_SETTING: i32 = 4;
    /// [`super::KeyReq`] -> `Option<bin>`. Permission `storage`.
    pub const KV_GET: i32 = 5;
    /// [`super::KvSetReq`] -> nothing. Permission `storage`.
    pub const KV_SET: i32 = 6;
    /// [`super::KeyReq`] -> nothing. Permission `storage`.
    pub const KV_DELETE: i32 = 7;
    /// [`super::HttpRequest`] -> request id (`u64`). Permission `network`.
    pub const HTTP_FETCH: i32 = 8;
    /// [`super::OpenUrlReq`] -> nothing. Permission `tabs`, `command` only.
    pub const OPEN_URL: i32 = 9;
    /// [`super::InjectJsReq`] -> nothing. Permission `page_scripting`,
    /// during `command` / `navigation` only, for the hook's tab.
    pub const INJECT_JS: i32 = 10;
    /// [`super::TextReq`] -> nothing. Permission `clipboard`, `command` only.
    pub const CLIPBOARD_WRITE: i32 = 11;
    /// No input -> `u64` Unix time in milliseconds.
    pub const NOW_MS: i32 = 12;
    /// [`super::NotifyReq`] -> nothing. Permission `notifications`.
    pub const NOTIFY: i32 = 13;
    /// No input -> `Option<`[`super::ActiveTab`]`>`. Permission `tabs`.
    pub const GET_ACTIVE_TAB: i32 = 14;
}

/// Negative results of `host_call`.
pub mod err {
    /// The plugin lacks the permission this call needs (or the op is not
    /// allowed in the current hook).
    pub const DENIED: i32 = -1;
    /// The input did not decode or failed validation.
    pub const BAD_INPUT: i32 = -2;
    /// Only allowed while handling a user action (a palette command).
    pub const NOT_USER_ACTION: i32 = -3;
    /// A size, rate or quota limit was hit.
    pub const LIMIT: i32 = -4;
    /// Unknown operation id.
    pub const UNKNOWN_OP: i32 = -5;
    /// `http_fetch` to a host the manifest does not list, or not HTTPS.
    pub const HOST_NOT_ALLOWED: i32 = -6;
    /// The host could not do it right now (storage failure, ...).
    pub const UNAVAILABLE: i32 = -7;
}

/// A plugin setting value. Encoded untagged: a MessagePack bool, integer
/// or string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl SettingValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// [`hook::LOAD`] input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadInfo {
    pub api_version: u32,
    pub plugin_id: String,
    /// Current values of every setting declared in the manifest.
    pub settings: BTreeMap<String, SettingValue>,
}

/// [`hook::SETTING_CHANGED`] input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChanged {
    pub key: String,
    pub value: SettingValue,
}

/// [`hook::REQUEST`] input: one network request that matched the plugin's
/// `request_filters`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestInput {
    pub url: String,
    /// `main_frame`, `sub_frame`, `script`, `stylesheet`, `image`, `font`,
    /// `xhr`, `media`, `websocket`, `ping` or `other`.
    pub resource_type: String,
    /// URL of the page that caused the request, if known.
    pub initiator: Option<String>,
    pub main_frame: bool,
}

/// What a request hook decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestAction {
    #[default]
    Allow,
    Block,
    Redirect,
}

/// [`hook::REQUEST`] output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestOutput {
    pub action: RequestAction,
    /// Required with [`RequestAction::Redirect`]; `http(s)` only.
    pub redirect_url: Option<String>,
}

impl RequestOutput {
    pub fn allow() -> Self {
        Self::default()
    }

    pub fn block() -> Self {
        Self {
            action: RequestAction::Block,
            redirect_url: None,
        }
    }

    pub fn redirect(url: impl Into<String>) -> Self {
        Self {
            action: RequestAction::Redirect,
            redirect_url: Some(url.into()),
        }
    }
}

/// [`hook::NAVIGATION`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NavigationEvent {
    pub tab_id: u64,
    pub url: String,
    pub title: String,
    /// `commit` (the document started) or `complete` (the load finished).
    pub phase: String,
}

/// [`hook::COMMAND`] input: the user ran a palette command (or its shortcut).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandInput {
    /// The `id` of the manifest `[[commands]]` entry.
    pub command: String,
    /// The active tab (0 when there is none).
    pub tab_id: u64,
    pub url: String,
}

/// [`hook::OMNIBOX`] input: the text after the plugin's keyword.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OmniboxInput {
    pub text: String,
}

/// One omnibox suggestion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Suggestion {
    pub title: String,
    pub url: String,
}

/// [`hook::OMNIBOX`] output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OmniboxOutput {
    pub suggestions: Vec<Suggestion>,
    /// Where Enter goes instead of the first suggestion.
    pub navigate: Option<String>,
}

/// [`hook::MCP_TOOL`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpToolInput {
    /// The `name` of the manifest `[[tools]]` entry.
    pub name: String,
    /// Arguments as a JSON object string.
    pub args_json: String,
}

/// [`hook::MCP_TOOL`] output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpToolOutput {
    pub content_text: String,
    pub is_error: bool,
}

impl McpToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content_text: s.into(),
            is_error: false,
        }
    }

    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content_text: s.into(),
            is_error: true,
        }
    }
}

/// [`hook::PAGE_MESSAGE`] input: `window.__ferrosurf.call("plugin:<id>", payload)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageMessageInput {
    /// Origin of the calling page (`https://example.com`).
    pub origin: String,
    pub tab_id: u64,
    /// The payload as a JSON string.
    pub payload_json: String,
}

/// [`hook::PAGE_MESSAGE`] output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageMessageOutput {
    /// The reply as a JSON string (empty: `null`).
    pub reply_json: String,
}

/// [`op::GET_ACTIVE_TAB`] result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActiveTab {
    pub tab_id: u64,
    pub url: String,
    pub title: String,
}

/// [`op::HTTP_FETCH`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: String,
    /// `https://` URL whose host is in the manifest's `network_hosts`.
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
}

/// [`hook::HTTP_RESPONSE`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpResponse {
    pub request_id: u64,
    /// HTTP status, or 0 when the request failed (see `error`).
    pub status: u16,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
    pub error: Option<String>,
}

/// [`op::LOG`] input. Levels: 0 error, 1 warn, 2 info, 3 debug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogReq {
    pub level: u8,
    pub message: String,
}

/// Input of the calls that take one text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextReq {
    pub text: String,
}

/// [`op::TOAST`] input.
pub type ToastReq = TextReq;

/// Input of the calls that take one key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyReq {
    pub key: String,
}

/// [`op::SET_SETTING`] input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSettingReq {
    pub key: String,
    pub value: SettingValue,
}

/// [`op::KV_SET`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KvSetReq {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

/// [`op::OPEN_URL`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenUrlReq {
    pub url: String,
    /// Open in a background tab instead of switching to it.
    pub background: bool,
}

/// [`op::INJECT_JS`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectJsReq {
    pub tab_id: u64,
    pub code: String,
}

/// [`op::NOTIFY`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyReq {
    pub title: String,
    pub body: String,
}

/// Encodes `v` the way the ABI requires (structs as maps).
pub fn encode<T: Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(v)
}

/// Decodes an ABI payload.
pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(b)
}

/// Packs a guest pointer and length into `fs_call`'s `i64` result.
pub fn pack(ptr: u32, len: u32) -> i64 {
    ((u64::from(ptr) << 32) | u64::from(len)) as i64
}

/// Inverse of [`pack`].
pub fn unpack(v: i64) -> (u32, u32) {
    let v = v as u64;
    ((v >> 32) as u32, v as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_output_is_a_map_with_lowercase_action() {
        let b = encode(&RequestOutput::block()).unwrap();
        // fixmap(2), "action", "block", "redirect_url", nil
        assert_eq!(b[0], 0x82);
        let back: RequestOutput = decode(&b).unwrap();
        assert_eq!(back.action, RequestAction::Block);
        let r: RequestOutput =
            decode(&encode(&RequestOutput::redirect("https://a.b/")).unwrap()).unwrap();
        assert_eq!(r.redirect_url.as_deref(), Some("https://a.b/"));
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        #[derive(Serialize)]
        struct Newer<'a> {
            text: &'a str,
            extra: u32,
        }
        let b = encode(&Newer {
            text: "hi",
            extra: 3,
        })
        .unwrap();
        let t: TextReq = decode(&b).unwrap();
        assert_eq!(t.text, "hi");
        let empty = encode(&BTreeMap::<String, u8>::new()).unwrap();
        let m: NavigationEvent = decode(&empty).unwrap();
        assert_eq!(m, NavigationEvent::default());
    }

    #[test]
    fn setting_values_are_untagged() {
        for v in [
            SettingValue::Bool(true),
            SettingValue::Int(-4),
            SettingValue::Str("x".into()),
        ] {
            let b = encode(&v).unwrap();
            assert_eq!(decode::<SettingValue>(&b).unwrap(), v);
        }
        assert_eq!(encode(&SettingValue::Bool(true)).unwrap(), [0xc3]);
    }

    #[test]
    fn bytes_are_bin() {
        let r = KvSetReq {
            key: "k".into(),
            value: vec![1, 2, 3],
        };
        let b = encode(&r).unwrap();
        assert!(b.ends_with(&[0xc4, 3, 1, 2, 3]));
    }

    #[test]
    fn pack_roundtrip() {
        assert_eq!(unpack(pack(0x1234, 0xffff_ffff)), (0x1234, 0xffff_ffff));
        assert_eq!(unpack(pack(u32::MAX, 1)), (u32::MAX, 1));
    }
}
