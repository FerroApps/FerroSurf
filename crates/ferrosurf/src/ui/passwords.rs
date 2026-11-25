//! Password manager / passkey UI (egui, Spanish copy).
//!
//! Functions mutate the [`PasswordsHost`] directly for host-only things (consent decisions,
//! unlock, sync) and return [`PasswordUiAction`]s for things that need the browser window.

use egui::{Id, RichText};
use egui_phosphor::regular as icon;

use super::{Palette, widgets};
use crate::passwords_host::{BwUi, PasskeyStorage, PasswordsHost, fill_script};
use crate::tab::TabId;
use ferrosurf_passwords::PasskeySource;

/// Things the app must do on behalf of the passwords UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordUiAction {
    /// Run this script in the active tab (`Cmd::EvalInActive(script)`).
    Fill { script: String },
    /// Open a URL in a new tab (`Cmd::OpenUrl`).
    OpenUrl(String),
}

fn modal_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.toolbar)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(18))
}

fn host_of(origin: &str) -> String {
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

fn source_label(s: PasskeySource) -> &'static str {
    match s {
        PasskeySource::Bitwarden => "Bitwarden",
        PasskeySource::Local => "Este navegador",
    }
}

fn account_name(name: &str, display: &str) -> String {
    match (name.is_empty(), display.is_empty()) {
        (false, false) if name != display => format!("{display} ({name})"),
        (false, _) => name.to_string(),
        (true, false) => display.to_string(),
        _ => "cuenta sin nombre".to_string(),
    }
}

/// Unlock row (password field + button). Returns the typed password when submitted.
fn unlock_row(ui: &mut egui::Ui, input: &mut String, busy: bool) -> Option<String> {
    let mut submit = false;
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(input)
                .password(true)
                .hint_text("Contraseña maestra")
                .desired_width(ui.available_width() - 110.0),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        ui.add_enabled_ui(!busy, |ui| {
            if widgets::accent_button(ui, "Desbloquear").clicked() {
                submit = true;
            }
        });
    });
    (submit && !input.is_empty()).then(|| std::mem::take(input))
}

/// Modal asking the user about the oldest passkey request of `active_tab`.
pub fn consent_modal(
    ctx: &egui::Context,
    host: &mut PasswordsHost,
    active_tab: Option<TabId>,
) -> Vec<PasswordUiAction> {
    let Some(i) = host.consents.iter().position(|c| Some(c.tab) == active_tab) else {
        return Vec::new();
    };
    let pal = Palette::get(ctx);
    let state = host.state.clone();
    let unlocked = host.is_unlocked();
    let busy = host.busy > 0;
    let id = host.consents[i].id;
    let mut approve = false;
    let mut cancel = false;
    let mut unlock_pw: Option<String> = None;
    {
        let c = &mut host.consents[i];
        let site = host_of(&c.origin);
        let mut can_approve = true;
        let modal = egui::Modal::new(Id::new(("passkey-consent", id))).frame(modal_frame(&pal)).show(ctx, |ui| {
            ui.set_width(400.0);
            if c.is_create() {
                ui.label(RichText::new(format!("{}  Crear llave de acceso", icon::FINGERPRINT)).heading());
                let user = match &c.kind {
                    crate::passwords_host::ConsentKind::Create { user, .. } => account_name(&user.name, &user.display_name),
                    _ => String::new(),
                };
                ui.add_space(4.0);
                ui.label(format!("{site} quiere crear una llave de acceso para {user}"));
                let existing = c.candidates().len();
                if existing > 0 {
                    ui.label(
                        RichText::new(format!("Ya tienes {existing} llave(s) de acceso para este sitio; se reemplazará la de la misma cuenta."))
                            .small()
                            .color(pal.text_dim),
                    );
                }
                widgets::section(ui, "Guardar en");
                if ui.radio(c.save_to == PasskeyStorage::Bitwarden && unlocked, "Bitwarden").clicked() && unlocked {
                    c.save_to = PasskeyStorage::Bitwarden;
                }
                if !unlocked {
                    match state {
                        BwUi::Locked => {
                            ui.label(RichText::new("Bitwarden está bloqueado. Desbloquéalo para guardar ahí:").small().color(pal.text_dim));
                            if let Some(pw) = unlock_row(ui, &mut c.unlock_input, busy) {
                                unlock_pw = Some(pw);
                            }
                        },
                        BwUi::NotInstalled => {
                            ui.label(RichText::new("Bitwarden CLI no está instalado.").small().color(pal.text_dim));
                        },
                        BwUi::Unauthenticated => {
                            ui.label(RichText::new("Inicia sesión con `bw login` en una terminal.").small().color(pal.text_dim));
                        },
                        _ => {},
                    }
                }
                if ui.radio(c.save_to == PasskeyStorage::Local || !unlocked, "Este navegador").clicked() {
                    c.save_to = PasskeyStorage::Local;
                }
                if !unlocked {
                    c.save_to = PasskeyStorage::Local;
                }
                can_approve = true;
            } else {
                ui.label(RichText::new(format!("{}  Llave de acceso", icon::FINGERPRINT)).heading());
                ui.add_space(4.0);
                ui.label(format!("Iniciar sesión en {} con una llave de acceso", c.rp_id));
                ui.label(RichText::new(format!("Solicitado por {site}")).small().color(pal.text_dim));
                ui.add_space(6.0);
                let labels: Vec<String> = c
                    .candidates()
                    .iter()
                    .map(|k| format!("{}  ·  {}", account_name(&k.user_name, &k.user_display_name), source_label(k.source)))
                    .collect();
                if c.selected >= labels.len() {
                    c.selected = 0;
                }
                for (n, l) in labels.iter().enumerate() {
                    if ui.selectable_label(c.selected == n, l).clicked() {
                        c.selected = n;
                    }
                }
                if c.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Buscando en Bitwarden…").small().color(pal.text_dim));
                    });
                } else if labels.is_empty() {
                    ui.label(RichText::new("No hay llaves de acceso guardadas para este sitio.").color(pal.text_dim));
                }
                if state == BwUi::Locked {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Desbloquea Bitwarden para ver también sus llaves:").small().color(pal.text_dim));
                    if let Some(pw) = unlock_row(ui, &mut c.unlock_input, busy) {
                        unlock_pw = Some(pw);
                    }
                }
                can_approve = !labels.is_empty();
            }
            if let Some(e) = &c.error {
                ui.colored_label(pal.danger, e);
            }
            if let Some(e) = &host.last_error {
                ui.colored_label(pal.danger, e);
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(can_approve, |ui| {
                    if widgets::accent_button(ui, "Permitir").clicked() {
                        approve = true;
                    }
                });
                if widgets::subtle_button(ui, "Cancelar").clicked() {
                    cancel = true;
                }
            });
        });
        if modal.should_close() {
            cancel = true;
        }
    }
    if let Some(pw) = unlock_pw {
        host.unlock(pw);
    }
    if approve {
        host.approve_consent(id);
    } else if cancel {
        host.cancel_consent(id);
    }
    Vec::new()
}

/// Key button for the toolbar with a popover. `current_url` is the active tab's URL.
pub fn toolbar_button(
    ui: &mut egui::Ui,
    host: &mut PasswordsHost,
    current_url: &str,
) -> Vec<PasswordUiAction> {
    let mut actions = Vec::new();
    if let Some(text) = host.take_clipboard() {
        ui.ctx().copy_text(text);
    }
    host.autofill_candidates(current_url);
    let matching = (host.logins_url == current_url && !host.logins.is_empty())
        .then(|| host.logins.len().to_string());
    let glyph = if host.is_unlocked() {
        icon::KEY
    } else {
        icon::LOCK_KEY
    };
    let resp = widgets::badge_button(
        ui,
        glyph,
        "Contraseñas y llaves de acceso",
        matching,
        host.is_unlocked(),
    );
    let pal = Palette::get(ui.ctx());
    let popup = egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    popup.show(|ui| {
        ui.set_min_width(300.0);
        ui.set_max_width(340.0);
        ui.label(RichText::new(format!("{}  Contraseñas", icon::KEY)).strong());
        ui.add_space(4.0);
        match host.state.clone() {
            BwUi::Unknown => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Comprobando Bitwarden…");
                });
            },
            BwUi::NotInstalled => {
                ui.label("Bitwarden CLI (`bw`) no está instalado.");
                ui.label(
                    RichText::new("Instálalo con `npm i -g @bitwarden/cli`, con Flatpak o descargando el binario, y pulsa Reintentar.")
                        .small()
                        .color(pal.text_dim),
                );
                ui.horizontal(|ui| {
                    if widgets::accent_button(ui, "Cómo instalarlo").clicked() {
                        actions.push(PasswordUiAction::OpenUrl("https://bitwarden.com/help/cli/".into()));
                    }
                    if widgets::subtle_button(ui, "Reintentar").clicked() {
                        host.refresh_status();
                    }
                });
            },
            BwUi::Unauthenticated => {
                ui.label("No has iniciado sesión en Bitwarden.");
                ui.label(RichText::new("Ejecuta `bw login` en una terminal y pulsa Reintentar.").small().color(pal.text_dim));
                if widgets::subtle_button(ui, "Reintentar").clicked() {
                    host.refresh_status();
                }
            },
            BwUi::Locked => {
                if let Some(mail) = host.user_email.clone() {
                    ui.label(RichText::new(mail).small().color(pal.text_dim));
                }
                let mut input = std::mem::take(&mut host.unlock_input);
                let busy = host.busy > 0;
                if let Some(pw) = unlock_row(ui, &mut input, busy) {
                    host.unlock(pw);
                }
                host.unlock_input = input;
                if busy {
                    ui.spinner();
                }
            },
            BwUi::Unlocked => {
                host.touch();
                let logins = host.logins.clone();
                if host.logins_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Buscando cuentas…").small().color(pal.text_dim));
                    });
                } else if logins.is_empty() {
                    ui.label(RichText::new("No hay cuentas guardadas para este sitio.").color(pal.text_dim));
                }
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for l in &logins {
                        ui.separator();
                        ui.label(RichText::new(if l.name.is_empty() { &l.username } else { &l.name }).strong());
                        if !l.username.is_empty() {
                            ui.label(RichText::new(&l.username).small().color(pal.text_dim));
                        }
                        ui.horizontal_wrapped(|ui| {
                            if widgets::accent_button(ui, "Rellenar").clicked() {
                                actions.push(PasswordUiAction::Fill { script: fill_script(&l.username, &l.password) });
                            }
                            if widgets::subtle_button(ui, "Usuario").clicked() {
                                ui.ctx().copy_text(l.username.clone());
                            }
                            if widgets::subtle_button(ui, "Contraseña").clicked() {
                                ui.ctx().copy_text(l.password.clone());
                            }
                            if l.totp.is_some() && widgets::subtle_button(ui, "TOTP").clicked() {
                                host.copy_totp(&l.id);
                            }
                        });
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if widgets::subtle_button(ui, "Sincronizar").clicked() {
                        host.sync();
                    }
                    if widgets::subtle_button(ui, "Bloquear").clicked() {
                        host.lock();
                    }
                    if host.busy > 0 {
                        ui.spinner();
                    }
                });
            },
        }
        if let Some(e) = host.last_error.clone() {
            ui.colored_label(pal.danger, e);
        } else if let Some(i) = host.last_info.clone() {
            ui.label(RichText::new(i).small().color(pal.ok));
        }
    });
    actions
}

/// "Contraseñas y llaves de acceso" block of the Settings window.
pub fn settings_section(ui: &mut egui::Ui, host: &mut PasswordsHost) -> Vec<PasswordUiAction> {
    let mut actions = Vec::new();
    let pal = Palette::get(ui.ctx());
    widgets::section(ui, "Contraseñas y llaves de acceso");

    let status = match host.state {
        BwUi::Unknown => "Comprobando…",
        BwUi::NotInstalled => "No instalado",
        BwUi::Unauthenticated => "Sin sesión",
        BwUi::Locked => "Bloqueado",
        BwUi::Unlocked => "Desbloqueado",
    };
    ui.horizontal(|ui| {
        ui.label("Bitwarden:");
        ui.label(RichText::new(status).strong());
        if widgets::subtle_button(ui, "Actualizar").clicked() {
            host.refresh_status();
        }
        if host.state == BwUi::NotInstalled && widgets::subtle_button(ui, "Cómo instalar").clicked()
        {
            actions.push(PasswordUiAction::OpenUrl(
                "https://bitwarden.com/help/cli/".into(),
            ));
        }
    });
    if let Some(p) = host.bw_binary() {
        ui.label(
            RichText::new(format!("Binario: {p}"))
                .small()
                .color(pal.text_dim),
        );
    }
    if let Some(s) = &host.server_url {
        ui.label(
            RichText::new(format!("Servidor: {s}"))
                .small()
                .color(pal.text_dim),
        );
    }
    if let Some(m) = &host.user_email {
        ui.label(
            RichText::new(format!("Cuenta: {m}"))
                .small()
                .color(pal.text_dim),
        );
    }

    let mut changed = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Ruta de bw:");
        let r = ui.add(
            egui::TextEdit::singleline(&mut host.config.bw_path)
                .hint_text("(detectar automáticamente)")
                .desired_width(260.0),
        );
        if r.lost_focus() && r.changed() {
            changed = true;
            host.refresh_status();
        }
    });
    ui.horizontal(|ui| {
        ui.label("Bloquear tras (min de inactividad, 0 = nunca):");
        changed |= ui
            .add(egui::DragValue::new(&mut host.config.auto_lock_minutes).range(0..=1440))
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label("Guardar llaves de acceso nuevas en:");
        changed |= ui
            .selectable_value(
                &mut host.config.default_store,
                PasskeyStorage::Bitwarden,
                "Bitwarden",
            )
            .changed();
        changed |= ui
            .selectable_value(
                &mut host.config.default_store,
                PasskeyStorage::Local,
                "Este navegador",
            )
            .changed();
    });
    if changed {
        host.save_config();
    }

    ui.add_space(6.0);
    ui.label(RichText::new("Llaves de acceso de este navegador").strong());
    ui.label(
        RichText::new("Sus claves privadas se cifran con una clave guardada en el perfil: solo las protegen los permisos del sistema de archivos. Para mayor seguridad usa Bitwarden.")
            .small()
            .color(pal.text_dim),
    );
    let keys: Vec<_> = host
        .local_passkeys()
        .iter()
        .map(|k| {
            (
                k.credential_id.clone(),
                k.rp_id.clone(),
                account_name(&k.user_name, &k.user_display_name),
            )
        })
        .collect();
    if keys.is_empty() {
        ui.label(RichText::new("No hay llaves de acceso guardadas.").color(pal.text_dim));
    }
    for (id, rp, user) in keys {
        ui.horizontal(|ui| {
            ui.label(RichText::new(rp).strong());
            ui.label(RichText::new(user).color(pal.text_dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::subtle_button(ui, "Eliminar").clicked() {
                    host.delete_local_passkey(&id);
                }
            });
        });
    }
    actions
}

/// Turn UI actions into window commands.
pub fn apply_actions(actions: Vec<PasswordUiAction>, cmds: &mut Vec<crate::window::Cmd>) {
    for a in actions {
        match a {
            PasswordUiAction::Fill { script } => {
                cmds.push(crate::window::Cmd::EvalInActive(script))
            }
            PasswordUiAction::OpenUrl(u) => {
                if let Ok(url) = url::Url::parse(&u) {
                    cmds.push(crate::window::Cmd::OpenUrl {
                        url,
                        disposition: crate::window::Disposition::NewForegroundTab,
                    });
                }
            }
        }
    }
}
