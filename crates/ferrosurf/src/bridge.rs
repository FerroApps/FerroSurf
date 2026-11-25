//! Page ↔ browser bridge.
//!
//! Servo has no embedder messaging API for page scripts, so injected scripts talk to the
//! browser through `prompt()`: a prompt whose message starts with [`PREFIX`] never reaches
//! the user. The browser parses `{channel, payload}`, answers by setting the prompt value
//! to a JSON reply and confirming it. The call is synchronous for the page script.
//!
//! **Pages can call this too** (injected scripts share the page's JS realm), so every
//! channel must treat requests as untrusted: the origin comes from the tab URL, never
//! from the payload, and anything sensitive needs explicit user consent in browser UI.

use serde::Deserialize;
use serde_json::Value;

/// Prefix of bridge prompts (starts with a control character no real page uses).
pub const PREFIX: &str = "\u{1}ferrosurf-bridge:";

/// JS helper injected before scripts that use the bridge:
/// `window.__ferrosurf.call(channel, payload)` returns the parsed reply (or throws).
pub const CLIENT_JS: &str = r#"(function(){
  if (window.__ferrosurf && window.__ferrosurf.call) return;
  const PREFIX = "\u0001ferrosurf-bridge:";
  const nativePrompt = window.prompt.bind(window);
  Object.defineProperty(window, "__ferrosurf", { configurable: false, enumerable: false, value: Object.freeze({
    call(channel, payload) {
      const raw = nativePrompt(PREFIX + JSON.stringify({ channel: String(channel), payload: payload === undefined ? null : payload }));
      if (raw === null || raw === undefined) throw new Error("FerroSurf bridge: no reply");
      const reply = JSON.parse(raw);
      if (reply && reply.__error) throw new Error(reply.__error);
      return reply;
    }
  })});
})();"#;

#[derive(Debug, Deserialize)]
pub struct BridgeCall {
    pub channel: String,
    #[serde(default)]
    pub payload: Value,
}

/// Parse a prompt message; `None` if it isn't a bridge call.
pub fn parse(message: &str) -> Option<Result<BridgeCall, String>> {
    let json = message.strip_prefix(PREFIX)?;
    Some(serde_json::from_str(json).map_err(|e| format!("bad bridge call: {e}")))
}

/// Encode an error reply.
pub fn error(msg: impl std::fmt::Display) -> String {
    serde_json::json!({ "__error": msg.to_string() }).to_string()
}

/// Outcome of handling a call.
pub enum Reply {
    /// Answer right away with this JSON.
    Now(Value),
    /// The handler will answer later through the given receiver (user consent, I/O, …).
    Later(std::sync::mpsc::Receiver<Result<Value, String>>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calls() {
        let msg = format!("{PREFIX}{}", r#"{"channel":"ping","payload":{"a":1}}"#);
        let call = parse(&msg).unwrap().unwrap();
        assert_eq!(call.channel, "ping");
        assert_eq!(call.payload["a"], 1);
        assert!(parse("Hello").is_none());
        assert!(parse(&format!("{PREFIX}not json")).unwrap().is_err());
        assert!(error("x").contains("__error"));
    }
}
