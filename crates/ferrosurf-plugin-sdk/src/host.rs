//! Safe wrappers around the host's `ferrosurf.host_call` import.
//!
//! On non-wasm targets the calls go to an in-process recorder instead, so
//! plugin logic can be unit-tested with `cargo test` (see [`testing`]).

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::abi::{
    self, ActiveTab, HttpRequest, InjectJsReq, KeyReq, KvSetReq, LogReq, NotifyReq, OpenUrlReq,
    SetSettingReq, SettingValue, TextReq, err, op,
};

/// Why a host call failed (the negative `host_call` result).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    Denied,
    BadInput,
    NotUserAction,
    Limit,
    UnknownOp,
    HostNotAllowed,
    Unavailable,
    /// The host's answer did not decode.
    Decode,
}

impl HostError {
    fn from_code(c: i32) -> Self {
        match c {
            err::DENIED => Self::Denied,
            err::BAD_INPUT => Self::BadInput,
            err::NOT_USER_ACTION => Self::NotUserAction,
            err::LIMIT => Self::Limit,
            err::HOST_NOT_ALLOWED => Self::HostNotAllowed,
            err::UNAVAILABLE => Self::Unavailable,
            _ => Self::UnknownOp,
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod sys {
    #[link(wasm_import_module = "ferrosurf")]
    unsafe extern "C" {
        pub fn host_call(op: i32, ptr: i32, len: i32) -> i32;
        pub fn host_take(ptr: i32, len: i32) -> i32;
    }

    pub fn call(op: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
        // SAFETY: the host only reads `len` bytes at `ptr`.
        let n = unsafe { host_call(op, input.as_ptr() as i32, input.len() as i32) };
        if n < 0 {
            return Err(n);
        }
        let mut out = vec![0u8; n as usize];
        if n > 0 {
            // SAFETY: the host writes at most `n` bytes into `out`.
            unsafe { host_take(out.as_mut_ptr() as i32, n) };
        }
        Ok(out)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod sys {
    pub fn call(op: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
        super::testing::record(op, input)
    }
}

/// Records host calls on native targets so plugins can be unit-tested.
#[cfg(not(target_arch = "wasm32"))]
pub mod testing {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    thread_local! {
        static CALLS: RefCell<Vec<(i32, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
        static REPLIES: RefCell<VecDeque<Result<Vec<u8>, i32>>> = const { RefCell::new(VecDeque::new()) };
    }

    pub(super) fn record(op: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
        CALLS.with(|c| c.borrow_mut().push((op, input.to_vec())));
        REPLIES.with(|r| r.borrow_mut().pop_front().unwrap_or(Ok(Vec::new())))
    }

    /// Queues the answer of the next host call (default: empty success).
    pub fn reply(r: Result<Vec<u8>, i32>) {
        REPLIES.with(|q| q.borrow_mut().push_back(r));
    }

    /// Every `(op, MessagePack input)` made so far; clears the record.
    pub fn take_calls() -> Vec<(i32, Vec<u8>)> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

fn call<T: Serialize + ?Sized>(op: i32, input: &T) -> Result<Vec<u8>, HostError> {
    let b = abi::encode(input).map_err(|_| HostError::BadInput)?;
    sys::call(op, &b).map_err(HostError::from_code)
}

fn call_decode<T: Serialize + ?Sized, R: DeserializeOwned>(
    op: i32,
    input: &T,
) -> Result<R, HostError> {
    let out = call(op, input)?;
    abi::decode(&out).map_err(|_| HostError::Decode)
}

/// Log levels for [`log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

/// Writes to the app log (prefixed with the plugin id, length-capped).
pub fn log(level: Level, message: &str) {
    let _ = call(
        op::LOG,
        &LogReq {
            level: level as u8,
            message: message.to_owned(),
        },
    );
}

/// Shows a short toast in the browser (one per hook call).
pub fn toast(text: &str) -> Result<(), HostError> {
    call(op::TOAST, &TextReq { text: text.into() }).map(drop)
}

/// The current value of a manifest setting.
pub fn setting(key: &str) -> Option<SettingValue> {
    call_decode(op::GET_SETTING, &KeyReq { key: key.into() })
        .ok()
        .flatten()
}

/// A string setting, or `""`.
pub fn setting_str(key: &str) -> String {
    match setting(key) {
        Some(SettingValue::Str(s)) => s,
        _ => String::new(),
    }
}

/// A bool setting, or `false`.
pub fn setting_bool(key: &str) -> bool {
    setting(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Changes a manifest setting (validated against its schema, persisted).
pub fn set_setting(key: &str, value: SettingValue) -> Result<(), HostError> {
    call(
        op::SET_SETTING,
        &SetSettingReq {
            key: key.into(),
            value,
        },
    )
    .map(drop)
}

/// Reads from the plugin's private key-value store (`storage`).
pub fn kv_get(key: &str) -> Result<Option<Vec<u8>>, HostError> {
    let v: Option<serde_bytes::ByteBuf> = call_decode(op::KV_GET, &KeyReq { key: key.into() })?;
    Ok(v.map(serde_bytes::ByteBuf::into_vec))
}

/// Writes to the plugin's private key-value store (`storage`, quota-limited).
pub fn kv_set(key: &str, value: &[u8]) -> Result<(), HostError> {
    call(
        op::KV_SET,
        &KvSetReq {
            key: key.into(),
            value: value.to_vec(),
        },
    )
    .map(drop)
}

/// Removes a key from the plugin's store (`storage`).
pub fn kv_delete(key: &str) -> Result<(), HostError> {
    call(op::KV_DELETE, &KeyReq { key: key.into() }).map(drop)
}

/// Starts an HTTPS request to a host listed in `network_hosts` (`network`).
/// The answer arrives later in [`crate::guest::Plugin::http_response`]
/// with the returned id.
pub fn http_fetch(req: &HttpRequest) -> Result<u64, HostError> {
    call_decode(op::HTTP_FETCH, req)
}

/// Opens a tab (`tabs`; only while handling a palette command).
pub fn open_url(url: &str, background: bool) -> Result<(), HostError> {
    call(
        op::OPEN_URL,
        &OpenUrlReq {
            url: url.into(),
            background,
        },
    )
    .map(drop)
}

/// Runs JavaScript in a tab (`page_scripting`; during `command` and
/// `navigation`, for that hook's tab only).
pub fn inject_js(tab_id: u64, code: &str) -> Result<(), HostError> {
    call(
        op::INJECT_JS,
        &InjectJsReq {
            tab_id,
            code: code.into(),
        },
    )
    .map(drop)
}

/// Copies text to the clipboard (`clipboard`, palette command only).
pub fn clipboard_write(text: &str) -> Result<(), HostError> {
    call(op::CLIPBOARD_WRITE, &TextReq { text: text.into() }).map(drop)
}

/// Unix time in milliseconds (wasm32-unknown-unknown has no clock).
pub fn now_ms() -> u64 {
    call_decode(op::NOW_MS, &()).unwrap_or(0)
}

/// Shows a desktop notification (`notifications`, one per hook call).
pub fn notify(title: &str, body: &str) -> Result<(), HostError> {
    call(
        op::NOTIFY,
        &NotifyReq {
            title: title.into(),
            body: body.into(),
        },
    )
    .map(drop)
}

/// The active tab (`tabs`).
pub fn get_active_tab() -> Result<Option<ActiveTab>, HostError> {
    call_decode(op::GET_ACTIVE_TAB, &())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_recorded_natively() {
        toast("hi").unwrap();
        testing::reply(Err(err::DENIED));
        assert_eq!(clipboard_write("x"), Err(HostError::Denied));
        let calls = testing::take_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, op::TOAST);
        let t: TextReq = abi::decode(&calls[0].1).unwrap();
        assert_eq!(t.text, "hi");
    }
}
