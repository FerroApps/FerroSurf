//! Page-initiated UI: alert/confirm/prompt, context menus, <select> popups, color pickers,
//! HTTP auth, permission doorhangers and the bookmark editor.

use std::collections::HashMap;

use egui::{Color32, Id, Pos2, Rect, RichText, Sense, Vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::permissions::{Decision, PermissionKind};
use servo::{
    ContextMenuAction, ContextMenuElementInformationFlags, ContextMenuItem, EmbedderControl,
    PermissionFeature, SelectElementOptionOrOptgroup, SimpleDialog,
};

use super::{Palette, widgets};
use crate::actions::Action;
use crate::window::{AppCmd, BrowserWindow, Cmd, Disposition, Env};

#[derive(Default)]
pub struct DialogState {
    prompt_values: HashMap<String, String>,
}

pub fn permission_kind(feature: PermissionFeature) -> PermissionKind {
    match feature {
        PermissionFeature::Geolocation => PermissionKind::Geolocation,
        PermissionFeature::Notifications | PermissionFeature::Push => PermissionKind::Notifications,
        PermissionFeature::Camera => PermissionKind::Camera,
        PermissionFeature::Microphone => PermissionKind::Microphone,
        PermissionFeature::Midi => PermissionKind::Midi,
        other => PermissionKind::Other(format!("{other:?}").to_lowercase()),
    }
}

fn permission_label(kind: &PermissionKind) -> (&'static str, String) {
    match kind {
        PermissionKind::Geolocation => (icon::MAP_PIN, "conocer tu ubicación".into()),
        PermissionKind::Notifications => (icon::BELL, "mostrar notificaciones".into()),
        PermissionKind::Camera => (icon::VIDEO_CAMERA, "usar tu cámara".into()),
        PermissionKind::Microphone => (icon::MICROPHONE, "usar tu micrófono".into()),
        PermissionKind::Midi => (icon::PIANO_KEYS, "acceder a dispositivos MIDI".into()),
        PermissionKind::Clipboard => (icon::CLIPBOARD, "leer el portapapeles".into()),
        other => (icon::KEY, format!("usar «{other:?}»")),
    }
}

fn modal_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.toolbar)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(18))
}

pub fn show_all(ctx: &egui::Context, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ctx);
    let active = w.active_tab().map(|t| t.id);
    let ppp = ctx.pixels_per_point();
    let content = Rect::from_min_max(
        (w.content_rect_px.min.to_vec2() / ppp).to_pos2(),
        (w.content_rect_px.max.to_vec2() / ppp).to_pos2(),
    );

    // Engine controls for the active tab. Each may be consumed (submit/dismiss take self).
    let mut i = 0;
    while i < w.controls.len() {
        if Some(w.controls[i].0) != active {
            i += 1;
            continue;
        }
        let mut consumed = false;
        let tab_url = w.active_tab().map(|t| t.url_string()).unwrap_or_default();
        let control_key = format!("{:?}", w.controls[i].1.id());
        match &mut w.controls[i].1 {
            EmbedderControl::SimpleDialog(dialog) => {
                let id = control_key;
                let mut outcome: Option<bool> = None;
                let origin = url::Url::parse(&tab_url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default();
                egui::Modal::new(Id::new(("simple-dialog", id.clone())))
                    .frame(modal_frame(&pal))
                    .show(ctx, |ui| {
                        ui.set_width(420.0);
                        ui.label(
                            RichText::new(format!("{}  {origin} dice", icon::GLOBE_SIMPLE))
                                .strong(),
                        );
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                ui.label(dialog.message());
                            });
                        if let SimpleDialog::Prompt(p) = dialog {
                            let value = w
                                .chrome
                                .dialogs
                                .prompt_values
                                .entry(id.clone())
                                .or_insert_with(|| p.current_value().to_string());
                            let r = ui.add(
                                egui::TextEdit::singleline(value).desired_width(f32::INFINITY),
                            );
                            r.request_focus();
                            p.set_current_value(value);
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                outcome = Some(true);
                            }
                        }
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::accent_button(ui, "Aceptar").clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                outcome = Some(true);
                            }
                            if !matches!(dialog, SimpleDialog::Alert(_))
                                && widgets::subtle_button(ui, "Cancelar").clicked()
                            {
                                outcome = Some(false);
                            }
                        });
                    });
                if let Some(ok) = outcome {
                    if let (_, EmbedderControl::SimpleDialog(d)) = w.controls.remove(i) {
                        if ok { d.confirm() } else { d.dismiss() }
                    }
                    w.chrome.dialogs.prompt_values.remove(&id);
                    consumed = true;
                }
            }
            EmbedderControl::ContextMenu(menu) => {
                let pos = content.min
                    + (Vec2::new(menu.position().min.x as f32, menu.position().min.y as f32) / ppp);
                let info = menu.element_info().clone();
                let items: Vec<ContextMenuItem> = menu.items().to_vec();
                let mut choice: Option<ContextMenuAction> = None;
                let mut close = false;
                let area = egui::Area::new(Id::new("page-context-menu"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .constrain(true)
                    .show(ctx, |ui| {
                        egui::Frame::menu(ui.style()).show(ui, |ui| {
                            ui.set_min_width(240.0);
                            let link = info.link_url.clone();
                            let image = info.image_url.clone();
                            if let Some(link) = &link {
                                if ui.button(format!("{}  Abrir enlace en una pestaña nueva", icon::ARROW_SQUARE_OUT)).clicked() {
                                    cmds.push(Cmd::OpenUrl { url: link.clone(), disposition: Disposition::NewBackgroundTab });
                                    close = true;
                                }
                                if ui.button(format!("{}  Abrir enlace en una ventana nueva", icon::APP_WINDOW)).clicked() {
                                    cmds.push(Cmd::App(AppCmd::NewWindow { urls: vec![link.clone()] }));
                                    close = true;
                                }
                                if ui.button(format!("{}  Guardar enlace como…", icon::DOWNLOAD_SIMPLE)).clicked() {
                                    cmds.push(Cmd::Download { url: link.clone() });
                                    close = true;
                                }
                                if ui.button(format!("{}  Copiar dirección del enlace", icon::LINK)).clicked() {
                                    ui.ctx().copy_text(link.to_string());
                                    close = true;
                                }
                                ui.separator();
                            }
                            if let Some(img) = &image {
                                if ui.button(format!("{}  Abrir imagen en una pestaña nueva", icon::IMAGE)).clicked() {
                                    cmds.push(Cmd::OpenUrl { url: img.clone(), disposition: Disposition::NewBackgroundTab });
                                    close = true;
                                }
                                if ui.button(format!("{}  Guardar imagen como…", icon::DOWNLOAD_SIMPLE)).clicked() {
                                    cmds.push(Cmd::Download { url: img.clone() });
                                    close = true;
                                }
                                if ui.button(format!("{}  Copiar dirección de la imagen", icon::LINK)).clicked() {
                                    ui.ctx().copy_text(img.to_string());
                                    close = true;
                                }
                                ui.separator();
                            }
                            let mut last_sep = true;
                            for item in &items {
                                match item {
                                    ContextMenuItem::Separator => {
                                        if !last_sep {
                                            ui.separator();
                                        }
                                        last_sep = true;
                                    },
                                    ContextMenuItem::Item { label, action, enabled } => {
                                        if matches!(action, ContextMenuAction::OpenLinkInNewWebView | ContextMenuAction::CopyLink | ContextMenuAction::CopyImageLink | ContextMenuAction::OpenImageInNewView) {
                                            continue; // replaced by our richer items above
                                        }
                                        let glyph = match action {
                                            ContextMenuAction::GoBack => icon::ARROW_LEFT,
                                            ContextMenuAction::GoForward => icon::ARROW_RIGHT,
                                            ContextMenuAction::Reload => icon::ARROW_CLOCKWISE,
                                            ContextMenuAction::Cut => icon::SCISSORS,
                                            ContextMenuAction::Copy => icon::COPY,
                                            ContextMenuAction::Paste => icon::CLIPBOARD_TEXT,
                                            ContextMenuAction::SelectAll => icon::SELECTION_ALL,
                                            _ => " ",
                                        };
                                        last_sep = false;
                                        let label = spanish_label(action).unwrap_or(label.as_str()).to_string();
                                        if ui.add_enabled(*enabled, egui::Button::new(format!("{glyph}  {label}"))).clicked() {
                                            choice = Some(*action);
                                        }
                                    },
                                }
                            }
                            ui.separator();
                            if info.flags.contains(ContextMenuElementInformationFlags::Selection) {
                                if ui.button(format!("{}  Preguntar al asistente sobre la selección", icon::SPARKLE)).clicked() {
                                    cmds.push(Cmd::EvalInActive(ASK_SELECTION_JS.into()));
                                    w.chrome.assistant_open = true;
                                    cmds.push(Cmd::App(AppCmd::StartAssistant {
                                        text: "Explica el texto que tengo seleccionado en la página (usa browser_eval_js con `String(window.getSelection())` para leerlo).".into(),
                                    }));
                                    close = true;
                                }
                            } else if ui.button(format!("{}  Preguntar al asistente sobre esta página", icon::SPARKLE)).clicked() {
                                w.chrome.assistant_open = true;
                                cmds.push(Cmd::App(AppCmd::StartAssistant { text: "Resume esta página.".into() }));
                                close = true;
                            }
                            if ui.button(format!("{}  Añadir a marcadores", icon::STAR)).clicked() {
                                cmds.push(Cmd::Action(Action::BookmarkPage));
                                close = true;
                            }
                            if ui.button(format!("{}  Ver código fuente", icon::CODE)).clicked() {
                                cmds.push(Cmd::Action(Action::ViewSource));
                                close = true;
                            }
                        });
                    });
                w.chrome.overlay_rects.push(area.response.rect);
                let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
                    && ctx
                        .pointer_interact_pos()
                        .is_some_and(|p| !area.response.rect.contains(p));
                if choice.is_some()
                    || close
                    || clicked_outside
                    || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    if let (_, EmbedderControl::ContextMenu(m)) = w.controls.remove(i) {
                        match choice {
                            Some(a) => m.select(a),
                            None => m.dismiss(),
                        }
                    }
                    consumed = true;
                }
            }
            EmbedderControl::SelectElement(select) => {
                let r = select.position();
                let pos = content.min + Vec2::new(r.min.x as f32, r.max.y as f32) / ppp;
                let width = ((r.max.x - r.min.x) as f32 / ppp).max(160.0);
                let options: Vec<SelectElementOptionOrOptgroup> = select.options().to_vec();
                let selected = select.selected_options();
                let multiple = select.allow_select_multiple();
                let mut picked: Option<usize> = None;
                let area = egui::Area::new(Id::new("select-popup"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .constrain(true)
                    .show(ctx, |ui| {
                        egui::Frame::menu(ui.style()).show(ui, |ui| {
                            ui.set_min_width(width);
                            egui::ScrollArea::vertical()
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    let mut row =
                                        |ui: &mut egui::Ui,
                                         o: &servo::SelectElementOption,
                                         indent: bool| {
                                            let is_sel = selected.contains(&o.id);
                                            let label = if indent {
                                                format!("    {}", o.label)
                                            } else {
                                                o.label.clone()
                                            };
                                            if ui
                                                .add_enabled(
                                                    !o.is_disabled,
                                                    egui::Button::selectable(is_sel, label),
                                                )
                                                .clicked()
                                            {
                                                picked = Some(o.id);
                                            }
                                        };
                                    for opt in &options {
                                        match opt {
                                            SelectElementOptionOrOptgroup::Option(o) => {
                                                row(ui, o, false)
                                            }
                                            SelectElementOptionOrOptgroup::Optgroup {
                                                label,
                                                options,
                                            } => {
                                                ui.label(RichText::new(label).strong().small());
                                                for o in options {
                                                    row(ui, o, true);
                                                }
                                            }
                                        }
                                    }
                                });
                        });
                    });
                w.chrome.overlay_rects.push(area.response.rect);
                let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
                    && ctx
                        .pointer_interact_pos()
                        .is_some_and(|p| !area.response.rect.contains(p));
                if let Some(id) = picked {
                    if multiple {
                        let mut sel = selected.clone();
                        if let Some(p) = sel.iter().position(|s| *s == id) {
                            sel.remove(p);
                        } else {
                            sel.push(id);
                        }
                        select.select(sel);
                    } else {
                        select.select(vec![id]);
                        if let (_, EmbedderControl::SelectElement(s)) = w.controls.remove(i) {
                            s.submit();
                        }
                        consumed = true;
                    }
                }
                if !consumed && (clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)))
                {
                    if let (_, EmbedderControl::SelectElement(s)) = w.controls.remove(i) {
                        s.submit();
                    }
                    consumed = true;
                }
            }
            EmbedderControl::ColorPicker(picker) => {
                let r = picker.position();
                let pos = content.min + Vec2::new(r.min.x as f32, r.max.y as f32) / ppp;
                let current = picker
                    .current_color()
                    .map(|c| Color32::from_rgb(c.red, c.green, c.blue))
                    .unwrap_or(Color32::BLACK);
                let mut color = current;
                let mut done = false;
                let area = egui::Area::new(Id::new("color-picker"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .constrain(true)
                    .show(ctx, |ui| {
                        egui::Frame::menu(ui.style()).show(ui, |ui| {
                            egui::color_picker::color_picker_color32(
                                ui,
                                &mut color,
                                egui::color_picker::Alpha::Opaque,
                            );
                            ui.horizontal(|ui| {
                                if widgets::accent_button(ui, "Aceptar").clicked() {
                                    done = true;
                                }
                            });
                        });
                    });
                w.chrome.overlay_rects.push(area.response.rect);
                if color != current {
                    picker.select(Some(servo::RgbColor {
                        red: color.r(),
                        green: color.g(),
                        blue: color.b(),
                    }));
                }
                if done || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    if let (_, EmbedderControl::ColorPicker(p)) = w.controls.remove(i) {
                        p.submit();
                    }
                    consumed = true;
                }
            }
            _ => {}
        }
        if !consumed {
            i += 1;
        }
    }

    // HTTP authentication.
    if let Some(idx) = w
        .chrome
        .auth_prompts
        .iter()
        .position(|p| Some(p.tab) == active)
    {
        let mut submit: Option<bool> = None;
        let prompt = &mut w.chrome.auth_prompts[idx];
        let host = prompt
            .request
            .as_ref()
            .map(|r| r.url().host_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let proxy = prompt.request.as_ref().is_some_and(|r| r.for_proxy());
        egui::Modal::new(Id::new("auth-dialog"))
            .frame(modal_frame(&pal))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new(format!("{}  Iniciar sesión", icon::KEY)).heading());
                ui.label(if proxy {
                    format!("El proxy {host} requiere usuario y contraseña")
                } else {
                    format!("{host} requiere usuario y contraseña")
                });
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut prompt.user)
                        .hint_text("Usuario")
                        .desired_width(f32::INFINITY),
                );
                let pw = ui.add(
                    egui::TextEdit::singleline(&mut prompt.password)
                        .password(true)
                        .hint_text("Contraseña")
                        .desired_width(f32::INFINITY),
                );
                if pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = Some(true);
                }
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::accent_button(ui, "Iniciar sesión").clicked() {
                        submit = Some(true);
                    }
                    if widgets::subtle_button(ui, "Cancelar").clicked() {
                        submit = Some(false);
                    }
                });
            });
        if let Some(ok) = submit {
            let mut prompt = w.chrome.auth_prompts.remove(idx);
            if let Some(req) = prompt.request.take() {
                if ok {
                    req.authenticate(prompt.user.clone(), prompt.password.clone());
                }
            }
        }
    }

    bookmark_editor(ctx, w, env);
}

fn spanish_label(action: &ContextMenuAction) -> Option<&'static str> {
    Some(match action {
        ContextMenuAction::GoBack => "Atrás",
        ContextMenuAction::GoForward => "Adelante",
        ContextMenuAction::Reload => "Recargar",
        ContextMenuAction::Cut => "Cortar",
        ContextMenuAction::Copy => "Copiar",
        ContextMenuAction::Paste => "Pegar",
        ContextMenuAction::SelectAll => "Seleccionar todo",
        _ => return None,
    })
}

const ASK_SELECTION_JS: &str = "String(window.getSelection())";

/// Permission prompts anchored to the top-left of the content area.
pub fn doorhangers(
    ui: &mut egui::Ui,
    content: Rect,
    w: &mut BrowserWindow,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
) {
    let pal = Palette::get(ui.ctx());
    let active = w.active_tab().map(|t| t.id);
    let Some(idx) = w
        .chrome
        .permission_prompts
        .iter()
        .position(|p| Some(p.tab) == active)
    else {
        return;
    };
    let (glyph, what) = permission_label(&w.chrome.permission_prompts[idx].kind);
    let origin = w.chrome.permission_prompts[idx].origin.clone();
    let mut decision: Option<(Decision, bool)> = None;
    let area = egui::Area::new(Id::new("doorhanger"))
        .order(egui::Order::Foreground)
        .fixed_pos(content.min + Vec2::new(12.0, 8.0))
        .show(ui.ctx(), |ui| {
            modal_frame(&pal)
                .shadow(ui.ctx().global_style().visuals.popup_shadow)
                .show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(glyph).size(22.0).color(pal.accent));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(origin.trim_start_matches("https://")).strong());
                            ui.label(format!("quiere {what}"));
                        });
                    });
                    let mut remember =
                        ui.data_mut(|d| *d.get_temp_mut_or(Id::new("perm-remember"), true));
                    if ui
                        .checkbox(&mut remember, "Recordar esta decisión")
                        .changed()
                    {
                        ui.data_mut(|d| d.insert_temp(Id::new("perm-remember"), remember));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, "Permitir").clicked() {
                            decision = Some((Decision::Allow, remember));
                        }
                        if widgets::subtle_button(ui, "Bloquear").clicked() {
                            decision = Some((Decision::Deny, remember));
                        }
                    });
                });
        });
    w.chrome.overlay_rects.push(area.response.rect);
    if let Some((d, remember)) = decision {
        let mut p = w.chrome.permission_prompts.remove(idx);
        if remember {
            env.profile.permissions.set(&p.origin, p.kind.clone(), d);
        }
        if let Some(req) = p.request.take() {
            match d {
                Decision::Allow => req.allow(),
                _ => req.deny(),
            }
        }
    }
    let _ = cmds;
    let _ = Pos2::ZERO;
    let _ = Sense::hover();
}

fn bookmark_editor(ctx: &egui::Context, w: &mut BrowserWindow, env: &mut Env) {
    let Some(ed) = &mut w.chrome.bookmark_editor else {
        return;
    };
    let pal = Palette::get(ctx);
    let field = w.chrome.omnibox.field_rect.0;
    let mut close = false;
    let mut remove = false;
    let folders = env.profile.bookmarks.folders();
    let area = egui::Area::new(Id::new("bookmark-editor"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(field.right() - 340.0, field.bottom() + 6.0))
        .show(ctx, |ui| {
            modal_frame(&pal)
                .shadow(ctx.global_style().visuals.popup_shadow)
                .show(ui, |ui| {
                    ui.set_width(310.0);
                    ui.label(
                        RichText::new(format!("{}  Marcador añadido", icon::STAR))
                            .strong()
                            .color(pal.accent),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Nombre").small());
                    ui.add(egui::TextEdit::singleline(&mut ed.title).desired_width(f32::INFINITY));
                    ui.label(RichText::new("Carpeta").small());
                    let current = folders
                        .iter()
                        .find(|(id, _)| *id == ed.folder)
                        .map(|f| f.1.clone())
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt("bm-folder")
                        .width(290.0)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (id, path) in &folders {
                                ui.selectable_value(&mut ed.folder, *id, path);
                            }
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, "Listo").clicked() {
                            close = true;
                        }
                        if widgets::subtle_button(ui, "Eliminar").clicked() {
                            remove = true;
                        }
                    });
                });
        });
    w.chrome.overlay_rects.push(area.response.rect);
    if remove {
        let _ = env.profile.bookmarks.remove(ed.id);
        w.chrome.bookmark_editor = None;
    } else if close {
        let (id, title, url, folder) = (ed.id, ed.title.clone(), ed.url.clone(), ed.folder);
        let _ = env.profile.bookmarks.update(id, &title, &url, vec![]);
        let _ = env.profile.bookmarks.move_to(id, folder, None);
        w.chrome.bookmark_editor = None;
    }
}
