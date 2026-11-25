//! TEMPORARY STUB — replaced by the full settings window implementation.
#[derive(Default)]
pub struct SettingsState {}

pub fn window(ctx: &egui::Context, w: &mut crate::window::BrowserWindow, _env: &mut crate::window::Env, _cmds: &mut Vec<crate::window::Cmd>) {
    let mut open = true;
    egui::Window::new("Ajustes").open(&mut open).show(ctx, |ui| {
        ui.label("…");
    });
    if !open {
        w.chrome.settings = None;
    }
}
