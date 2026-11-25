//! Settings page for native (WebAssembly) plugins.

use egui::RichText;
use egui_phosphor::regular as icon;
use ferrosurf_plugins::manifest::SettingKind;
use ferrosurf_plugins::sdk::abi::SettingValue;

use super::{Palette, widgets};
use crate::window::{AppCmd, Cmd, Env};

pub fn settings_page(ui: &mut egui::Ui, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let pal = Palette::get(ui.ctx());
    let host = env.profile.plugins.clone();
    let mut changed = false;

    widgets::section(ui, "Plugins nativos");
    ui.label(
        RichText::new(
            "Los plugins nativos se escriben en Rust y se ejecutan como WebAssembly en un entorno \
             aislado: solo pueden hacer lo que les permitas, no consumen nada mientras están \
             desactivados y son mucho más ligeros que las extensiones de Chrome/Firefox \
             (que siguen funcionando como «legacy»).",
        )
        .color(pal.text_dim)
        .small(),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if widgets::subtle_button(ui, &format!("{}  Instalar carpeta…", icon::FOLDER_OPEN))
            .clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            report(env, host.install_from(&dir));
            changed = true;
        }
        if widgets::subtle_button(
            ui,
            &format!("{}  Instalar .ferroplug…", icon::UPLOAD_SIMPLE),
        )
        .clicked()
            && let Some(file) = rfd::FileDialog::new()
                .add_filter("Plugin de FerroSurf", &["ferroplug", "zip"])
                .pick_file()
        {
            report(env, host.install_from(&file));
            changed = true;
        }
        if widgets::subtle_button(ui, &format!("{}  Recargar", icon::ARROW_CLOCKWISE)).clicked() {
            host.reload_all();
            changed = true;
        }
        if widgets::subtle_button(ui, &format!("{}  Abrir carpeta", icon::FOLDER)).clicked() {
            let _ = std::fs::create_dir_all(host.dir());
            crate::app::open_external(&host.dir().display().to_string());
        }
    });
    ui.add_space(10.0);

    let plugins = host.list();
    if plugins.is_empty() {
        ui.label(
            RichText::new(
                "No hay plugins instalados. Consulta docs/plugins.md para crear uno con el SDK.",
            )
            .color(pal.text_dim),
        );
    }
    for p in plugins {
        let m = &p.manifest;
        egui::Frame::new()
            .fill(pal.field)
            .corner_radius(10)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::CUBE).size(20.0));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{}  {}", m.name, m.version)).strong());
                        if !m.description.is_empty() {
                            ui.label(RichText::new(&m.description).small().color(pal.text_dim));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(icon::TRASH)
                            .on_hover_text("Desinstalar")
                            .clicked()
                        {
                            if let Err(e) = host.uninstall(&m.id) {
                                env.shared.toast(&format!("No se pudo desinstalar: {e}"));
                            }
                            changed = true;
                        }
                        let mut on = p.enabled && !p.needs_reapproval;
                        if ui.checkbox(&mut on, "Activo").changed() {
                            if on {
                                // The permissions are listed right below: turning it on grants them.
                                if let Err(e) = host.enable(&m.id, &m.permissions) {
                                    env.shared.toast(&format!("No se pudo activar: {e}"));
                                }
                            } else {
                                host.disable(&m.id);
                            }
                            changed = true;
                        }
                    });
                });
                if let Some(err) = &p.error {
                    ui.label(RichText::new(format!("⚠ {err}")).color(pal.danger).small());
                }
                if p.needs_reapproval {
                    ui.label(
                        RichText::new(
                            "Una actualización pide permisos nuevos: actívalo de nuevo para concederlos.",
                        )
                        .color(pal.danger)
                        .small(),
                    );
                }
                if !m.permissions.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if p.enabled {
                            "Permisos concedidos:"
                        } else {
                            "Al activarlo podrá:"
                        })
                        .small(),
                    );
                    for perm in &m.permissions {
                        let granted = p.granted.contains(perm);
                        ui.label(
                            RichText::new(format!(
                                "  {} {}",
                                if granted || !p.enabled { "•" } else { "✕" },
                                perm.describe()
                            ))
                            .small()
                            .color(pal.text_dim),
                        );
                    }
                    if !m.network_hosts.is_empty() {
                        ui.label(
                            RichText::new(format!("  Servidores: {}", m.network_hosts.join(", ")))
                                .small()
                                .color(pal.text_dim),
                        );
                    }
                }
                if !p.settings.is_empty() {
                    ui.add_space(6.0);
                    for s in &p.settings {
                        let mut value = s.value.clone();
                        let mut edited = false;
                        ui.horizontal(|ui| {
                            ui.label(&s.spec.label);
                            match (&s.spec.kind, &mut value) {
                                (SettingKind::Bool, SettingValue::Bool(b)) => {
                                    edited = ui.checkbox(b, "").changed();
                                }
                                (SettingKind::Int { min, max }, SettingValue::Int(i)) => {
                                    edited = ui
                                        .add(egui::DragValue::new(i).range(*min..=*max))
                                        .changed();
                                }
                                (SettingKind::Str { .. }, SettingValue::Str(t)) => {
                                    edited = ui.text_edit_singleline(t).lost_focus();
                                }
                                (SettingKind::Choice { choices }, SettingValue::Str(t)) => {
                                    egui::ComboBox::from_id_salt((&m.id, &s.spec.key))
                                        .selected_text(t.clone())
                                        .show_ui(ui, |ui| {
                                            for c in choices {
                                                edited |= ui
                                                    .selectable_value(t, c.clone(), c)
                                                    .changed();
                                            }
                                        });
                                }
                                _ => {}
                            }
                        });
                        if edited
                            && let Err(e) = host.set_setting(&m.id, &s.spec.key, value)
                        {
                            env.shared.toast(&format!("Ajuste no válido: {e}"));
                        }
                    }
                }
            });
        ui.add_space(8.0);
    }
    for b in host.broken() {
        ui.label(
            RichText::new(format!("⚠ {}: {}", b.path.display(), b.error))
                .color(pal.danger)
                .small(),
        );
    }
    if changed {
        cmds.push(Cmd::App(AppCmd::ExtensionsChanged));
    }
    false
}

fn report(env: &mut Env, r: Result<String, String>) {
    match r {
        Ok(id) => env.shared.toast(&format!(
            "Plugin «{id}» instalado. Actívalo para concederle sus permisos."
        )),
        Err(e) => env
            .shared
            .toast(&format!("No se pudo instalar el plugin: {e}")),
    }
}
