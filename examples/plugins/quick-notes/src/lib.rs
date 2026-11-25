//! Example plugin: a palette command that saves the active page into the
//! plugin's private store, and an MCP tool that lets the AI assistant read
//! the notes back.

use ferrosurf_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};

const KEY: &str = "notes";
/// Keeps the stored JSON well under the 64 KiB value limit.
const MAX_NOTES: usize = 100;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Note {
    pub url: String,
    pub title: String,
    pub saved_ms: u64,
}

pub fn parse(bytes: &[u8]) -> Vec<Note> {
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// Adds `note` at the front, replacing an older note of the same URL.
pub fn add(mut notes: Vec<Note>, note: Note) -> Vec<Note> {
    notes.retain(|n| n.url != note.url);
    notes.insert(0, note);
    notes.truncate(MAX_NOTES);
    notes
}

fn load() -> Vec<Note> {
    host::kv_get(KEY)
        .ok()
        .flatten()
        .map(|b| parse(&b))
        .unwrap_or_default()
}

#[derive(Default)]
pub struct QuickNotes;

impl Plugin for QuickNotes {
    fn command(&mut self, input: CommandInput) {
        if input.command != "save" {
            return;
        }
        let Ok(Some(tab)) = host::get_active_tab() else {
            let _ = host::toast("No hay ninguna pestaña activa.");
            return;
        };
        let note = Note {
            title: if tab.title.is_empty() {
                tab.url.clone()
            } else {
                tab.title
            },
            url: tab.url,
            saved_ms: host::now_ms(),
        };
        let notes = add(load(), note);
        let saved = serde_json::to_vec(&notes)
            .ok()
            .map(|b| host::kv_set(KEY, &b));
        let _ = host::toast(if matches!(saved, Some(Ok(()))) {
            "Nota guardada."
        } else {
            "No se pudo guardar la nota."
        });
    }

    fn mcp_tool(&mut self, input: McpToolInput) -> McpToolOutput {
        if input.name != "list_notes" {
            return McpToolOutput::error(format!("unknown tool {}", input.name));
        }
        let args: serde_json::Value = serde_json::from_str(&input.args_json).unwrap_or_default();
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(20, |n| n.clamp(1, 100)) as usize;
        let notes: Vec<Note> = load().into_iter().take(limit).collect();
        match serde_json::to_string(&notes) {
            Ok(json) => McpToolOutput::text(json),
            Err(e) => McpToolOutput::error(e.to_string()),
        }
    }
}

register_plugin!(QuickNotes);

#[cfg(test)]
mod tests {
    use super::*;
    use ferrosurf_plugin_sdk::abi;

    fn note(url: &str) -> Note {
        Note {
            url: url.into(),
            title: url.into(),
            saved_ms: 1,
        }
    }

    #[test]
    fn newest_first_without_duplicates() {
        let n = add(add(vec![], note("a")), note("b"));
        let n = add(n, note("a"));
        let urls: Vec<_> = n.iter().map(|n| n.url.as_str()).collect();
        assert_eq!(urls, ["a", "b"]);
        let mut many = Vec::new();
        for i in 0..150 {
            many = add(many, note(&i.to_string()));
        }
        assert_eq!(many.len(), MAX_NOTES);
        assert_eq!(parse(b"garbage"), vec![]);
    }

    #[test]
    fn tool_lists_stored_notes() {
        let stored = serde_json::to_vec(&vec![note("a"), note("b")]).unwrap();
        let bin = abi::encode(&Some(ferrosurf_plugin_sdk::serde_bytes::ByteBuf::from(stored)))
            .unwrap();
        host::testing::reply(Ok(bin));
        let out = QuickNotes.mcp_tool(McpToolInput {
            name: "list_notes".into(),
            args_json: "{\"limit\":1}".into(),
        });
        assert!(!out.is_error);
        let got: Vec<Note> = serde_json::from_str(&out.content_text).unwrap();
        assert_eq!(got, vec![note("a")]);
        assert!(QuickNotes.mcp_tool(McpToolInput::default()).is_error);
    }
}
