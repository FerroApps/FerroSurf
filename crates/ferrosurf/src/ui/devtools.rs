//! Built-in developer tools panel (F12). TEMPORARY STUB, replaced by the full panel.

use std::sync::Arc;

use crate::window::{BrowserWindow, Cmd, Env};

/// Where an async JS evaluation result lands (polled by the panel each frame).
pub type EvalSlot = Arc<parking_lot::Mutex<Option<Result<serde_json::Value, String>>>>;

#[derive(Default)]
pub struct DevtoolsState {}

pub fn panel(ui: &mut egui::Ui, w: &mut BrowserWindow, _env: &mut Env, _cmds: &mut Vec<Cmd>) {
    ui.label(format!(
        "DevTools — {} mensajes",
        w.active_tab().map(|t| t.console.len()).unwrap_or(0)
    ));
}
