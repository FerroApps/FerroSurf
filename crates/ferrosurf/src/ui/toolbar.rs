//! Navigation toolbar, address bar, menus and bookmarks bar.

use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::{BookmarkId, BookmarkNode};

use super::library::BmDrag;

use super::{LibraryTab, Palette, SettingsPage, widgets};
use crate::actions::Action;
use crate::window::{AppCmd, BrowserWindow, Cmd, Disposition, Env};

pub fn toolbar(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let (can_back, can_fwd, loading, has_tab) = w
        .active_tab()
        .map(|t| (t.can_go_back, t.can_go_forward, t.is_loading(), true))
        .unwrap_or((false, false, false, false));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if widgets::icon_button(ui, icon::ARROW_LEFT, "Atrás (Alt+←)", can_back).clicked() {
            cmds.push(Cmd::Action(Action::Back));
        }
        if widgets::icon_button(ui, icon::ARROW_RIGHT, "Adelante (Alt+→)", can_fwd).clicked() {
            cmds.push(Cmd::Action(Action::Forward));
        }
        if loading {
            if widgets::icon_button(ui, icon::X, "Detener (Esc)", true).clicked() {
                cmds.push(Cmd::Action(Action::Stop));
            }
        } else if widgets::icon_button(ui, icon::ARROW_CLOCKWISE, "Recargar (Ctrl+R)", has_tab)
            .clicked()
        {
            cmds.push(Cmd::Action(Action::Reload));
        }
        if env.profile.settings.appearance.show_home_button
            && widgets::icon_button(ui, icon::HOUSE, "Página de inicio", true).clicked()
        {
            cmds.push(Cmd::Action(Action::Home));
        }
        ui.add_space(6.0);

        // Right-side buttons are laid out first (right-to-left) so the omnibox fills the rest.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            main_menu(ui, w, env, cmds);
            profile_button(ui, w, env, cmds);
            if env.profile.settings.assistant.enabled
                && widgets::badge_button(
                    ui,
                    icon::SPARKLE,
                    "Asistente IA (Ctrl+J)",
                    None,
                    w.chrome.assistant_open,
                )
                .clicked()
            {
                cmds.push(Cmd::Action(Action::ToggleAssistant));
            }
            downloads_button(ui, w, env, cmds);
            if !env.profile.extensions.list().is_empty()
                && widgets::icon_button(ui, icon::PUZZLE_PIECE, "Extensiones", true).clicked()
            {
                w.chrome.open_settings(SettingsPage::Extensions);
            }
            shield_button(ui, w, env, cmds);
            {
                let url = w.active_tab().map(|t| t.url_string()).unwrap_or_default();
                let acts = super::passwords::toolbar_button(ui, &mut env.profile.passwords, &url);
                super::passwords::apply_actions(acts, cmds);
            }
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                super::omnibox::omnibox(ui, w, env, cmds);
            });
        });
    });
    let _ = pal;
}

fn shield_button(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let blocked = w.active_tab().map(|t| t.blocked_requests).unwrap_or(0);
    let enabled = env.profile.settings.privacy.block_trackers;
    let glyph = if enabled {
        icon::SHIELD_CHECK
    } else {
        icon::SHIELD_SLASH
    };
    let badge = (enabled && blocked > 0).then(|| {
        if blocked > 99 {
            "99+".to_string()
        } else {
            blocked.to_string()
        }
    });
    let resp = widgets::badge_button(ui, glyph, "Protección contra rastreadores", badge, false);
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(260.0);
            ui.label(
                RichText::new(format!(
                    "{}  Protección contra rastreadores",
                    icon::SHIELD_CHECK
                ))
                .strong(),
            );
            ui.add_space(4.0);
            ui.label(format!("Bloqueados en esta página: {blocked}"));
            ui.label(
                RichText::new(format!(
                    "Total en esta sesión: {}",
                    env.shared.blocked_total
                ))
                .small(),
            );
            ui.add_space(6.0);
            let mut on = enabled;
            if ui
                .checkbox(&mut on, "Bloquear anuncios y rastreadores")
                .changed()
            {
                env.profile.settings.privacy.block_trackers = on;
                env.profile.mark_settings_dirty();
                cmds.push(Cmd::App(AppCmd::SettingsChanged));
                cmds.push(Cmd::Action(Action::Reload));
            }
            if ui.link("Ajustes de privacidad…").clicked() {
                w.chrome.open_settings(SettingsPage::Privacy);
            }
        });
}

fn downloads_button(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let active = env.profile.downloads.active_count();
    if active == 0 && !env.shared.downloads_attention && env.profile.downloads.items().is_empty() {
        return;
    }
    let resp = widgets::badge_button(
        ui,
        icon::DOWNLOAD_SIMPLE,
        "Descargas (Ctrl+Shift+Y)",
        (active > 0).then(|| active.to_string()),
        w.chrome.library == Some(LibraryTab::Downloads),
    );
    if let Some(p) = env.profile.downloads.overall_progress() {
        let c = resp.rect.center();
        let r = resp.rect.width() / 2.0 - 2.0;
        let n = 32;
        let pts: Vec<egui::Pos2> = (0..=((n as f32 * p) as usize))
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 / n as f32 * std::f32::consts::TAU;
                c + r * egui::vec2(a.cos(), a.sin())
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(pts, Stroke::new(2.0, pal.accent)));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
    if resp.clicked() {
        env.shared.downloads_attention = false;
        cmds.push(Cmd::Action(Action::ShowDownloads));
    }
}

fn profile_button(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let color = super::theme::parse_hex(&env.profile.profile.color).unwrap_or(pal.accent);
    let size = ui.spacing().interact_size.y.max(28.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    widgets::paint_letter_avatar(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(22.0)),
        &env.profile.profile.initial(),
        color,
    );
    let resp = resp.on_hover_text(format!("Perfil: {}", env.profile.profile.name));
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(240.0);
        ui.horizontal(|ui| {
            widgets::letter_avatar(ui, &env.profile.profile.initial(), color, 32.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&env.profile.profile.name).strong());
                ui.label(
                    RichText::new(if w.guest {
                        "Sesión de invitado"
                    } else {
                        "Perfil activo"
                    })
                    .small()
                    .color(pal.text_dim),
                );
            });
        });
        ui.separator();
        let current = env.profile.profile.id.clone();
        let others: Vec<_> = env
            .profile
            .profiles
            .borrow()
            .profiles()
            .iter()
            .filter(|p| p.id != current)
            .cloned()
            .collect();
        for p in others {
            let c = super::theme::parse_hex(&p.color).unwrap_or(pal.accent);
            ui.horizontal(|ui| {
                widgets::letter_avatar(ui, &p.initial(), c, 20.0);
                if ui.button(&p.name).clicked() {
                    cmds.push(Cmd::App(AppCmd::OpenProfile(p.id.clone())));
                }
            });
        }
        if ui
            .button(format!(
                "{}  Abrir ventana de invitado",
                icon::USER_CIRCLE_DASHED
            ))
            .clicked()
        {
            cmds.push(Cmd::App(AppCmd::NewGuestWindow));
        }
        if ui
            .button(format!("{}  Gestionar perfiles", icon::USERS_THREE))
            .clicked()
        {
            w.chrome.open_settings(SettingsPage::Profiles);
        }
    });
}

fn menu_item(ui: &mut Ui, glyph: &str, label: &str, shortcut: Option<String>) -> bool {
    let pal = Palette::get(ui.ctx());
    let resp = ui.add(
        egui::Button::new(format!("{glyph}  {label}"))
            .shortcut_text(RichText::new(shortcut.unwrap_or_default()).color(pal.text_dim))
            .min_size(Vec2::new(250.0, 0.0)),
    );
    resp.clicked()
}

fn main_menu(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let resp = widgets::icon_button(ui, icon::LIST, "Menú", true);
    let km = env.keymap;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(270.0);
        let item = |ui: &mut Ui, glyph: &str, action: Action, cmds: &mut Vec<Cmd>| {
            if menu_item(ui, glyph, &action.label(), km.shortcut_for(action)) {
                cmds.push(Cmd::Action(action));
            }
        };
        item(ui, icon::PLUS, Action::NewTab, cmds);
        item(ui, icon::APP_WINDOW, Action::NewWindow, cmds);
        item(ui, icon::USER_CIRCLE_DASHED, Action::NewGuestWindow, cmds);
        ui.separator();
        item(ui, icon::CLOCK_COUNTER_CLOCKWISE, Action::ShowHistory, cmds);
        item(ui, icon::BOOKMARKS_SIMPLE, Action::ShowBookmarks, cmds);
        item(ui, icon::DOWNLOAD_SIMPLE, Action::ShowDownloads, cmds);
        item(ui, icon::PUZZLE_PIECE, Action::ShowExtensions, cmds);
        ui.separator();
        // Zoom row.
        let zoom = w.active_tab().map(|t| t.zoom).unwrap_or(1.0);
        ui.horizontal(|ui| {
            ui.label(format!("{}  Zoom", icon::MAGNIFYING_GLASS));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(icon::CORNERS_OUT)
                    .on_hover_text("Pantalla completa (F11)")
                    .clicked()
                {
                    cmds.push(Cmd::Action(Action::Fullscreen));
                }
                if ui.button(icon::PLUS).clicked() {
                    cmds.push(Cmd::Action(Action::ZoomIn));
                }
                if ui.button(format!("{:.0}%", zoom * 100.0)).clicked() {
                    cmds.push(Cmd::Action(Action::ZoomReset));
                }
                if ui.button(icon::MINUS).clicked() {
                    cmds.push(Cmd::Action(Action::ZoomOut));
                }
            });
        });
        ui.separator();
        item(ui, icon::MAGNIFYING_GLASS, Action::Find, cmds);
        item(ui, icon::COMMAND, Action::CommandPalette, cmds);
        if env.profile.settings.assistant.enabled {
            item(ui, icon::SPARKLE, Action::ToggleAssistant, cmds);
        }
        item(ui, icon::CAMERA, Action::Screenshot, cmds);
        ui.menu_button(format!("{}  Más herramientas", icon::WRENCH), |ui| {
            if menu_item(
                ui,
                icon::CODE,
                &Action::ViewSource.label(),
                km.shortcut_for(Action::ViewSource),
            ) {
                cmds.push(Cmd::Action(Action::ViewSource));
            }
            if menu_item(
                ui,
                icon::BUG,
                &Action::DevTools.label(),
                km.shortcut_for(Action::DevTools),
            ) {
                cmds.push(Cmd::Action(Action::DevTools));
            }
            if menu_item(
                ui,
                icon::SIDEBAR,
                &Action::ToggleVerticalTabs.label(),
                km.shortcut_for(Action::ToggleVerticalTabs),
            ) {
                cmds.push(Cmd::Action(Action::ToggleVerticalTabs));
            }
            if menu_item(
                ui,
                icon::BOOKMARK_SIMPLE,
                &Action::ToggleBookmarksBar.label(),
                km.shortcut_for(Action::ToggleBookmarksBar),
            ) {
                cmds.push(Cmd::Action(Action::ToggleBookmarksBar));
            }
            if menu_item(
                ui,
                icon::BROOM,
                &Action::ClearBrowsingData.label(),
                km.shortcut_for(Action::ClearBrowsingData),
            ) {
                cmds.push(Cmd::Action(Action::ClearBrowsingData));
            }
            if menu_item(ui, icon::PLUGS_CONNECTED, "Conexión MCP", None) {
                w.chrome.open_settings(SettingsPage::Mcp);
            }
        });
        ui.separator();
        item(ui, icon::GEAR_SIX, Action::ShowSettings, cmds);
        item(ui, icon::INFO, Action::About, cmds);
        ui.separator();
        item(ui, icon::SIGN_OUT, Action::Quit, cmds);
    });
}

/// Bookmarks bar under the toolbar.
/// Deferred edits requested from the bookmarks bar (applied after drawing).
enum BarOp {
    Move {
        id: BookmarkId,
        parent: BookmarkId,
        index: Option<usize>,
    },
    NewFolder {
        parent: BookmarkId,
    },
    AddCurrent {
        parent: BookmarkId,
    },
    Remove(BookmarkId),
    Rename(BookmarkId, String),
}

/// Bookmarks bar under the toolbar. Bookmarks and folders can be dragged to reorder,
/// dropped in from the library panel, and folders created from the context menu.
pub fn bookmarks_bar(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let children: Vec<BookmarkNode> = env.profile.bookmarks.toolbar().children().to_vec();
    let dragging = egui::DragAndDrop::payload::<BmDrag>(ui.ctx()).map(|p| p.0);
    let mut ops: Vec<BarOp> = Vec::new();
    let renaming = ui
        .ctx()
        .data(|d| d.get_temp::<(BookmarkId, String)>(egui::Id::new("bar-rename")));

    let bar = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.set_height(24.0);
        if children.is_empty() {
            let hint = if dragging.is_some() {
                format!(
                    "{}  Suelta aquí para añadirlo a la barra",
                    icon::DOWNLOAD_SIMPLE
                )
            } else {
                format!(
                    "{}  Pulsa Ctrl+D o la estrella para guardar páginas aquí",
                    icon::STAR
                )
            };
            ui.label(RichText::new(hint).small().color(pal.text_dim));
        }
        let mut item_rects: Vec<Rect> = Vec::with_capacity(children.len());
        for node in &children {
            let resp = bookmark_bar_item(ui, node, &env.profile.favicons, cmds, &mut ops);
            item_rects.push(resp.rect);
        }
        // Drop target: insertion point by pointer x among the items.
        if let Some(id) = dragging
            && let Some(pos) = ui.ctx().pointer_hover_pos()
            && ui.max_rect().expand(4.0).contains(pos)
        {
            // Dropping on the middle of a folder moves into it.
            let into_folder = children.iter().zip(&item_rects).find_map(|(n, r)| match n {
                BookmarkNode::Folder { id: fid, .. }
                    if *fid != id && r.shrink2(Vec2::new(r.width() * 0.25, 0.0)).contains(pos) =>
                {
                    Some((*fid, *r))
                }
                _ => None,
            });
            if let Some((fid, r)) = into_folder {
                ui.painter().rect_stroke(
                    r,
                    6.0,
                    Stroke::new(1.5, pal.accent),
                    egui::StrokeKind::Inside,
                );
                if ui.input(|i| i.pointer.any_released()) {
                    ops.push(BarOp::Move {
                        id,
                        parent: fid,
                        index: None,
                    });
                }
            } else {
                let index = item_rects
                    .iter()
                    .position(|r| pos.x < r.center().x)
                    .unwrap_or(item_rects.len());
                let x = match (item_rects.get(index), item_rects.last()) {
                    (Some(r), _) => r.left() - 1.0,
                    (None, Some(last)) => last.right() + 1.0,
                    (None, None) => ui.min_rect().left() + 2.0,
                };
                let r = ui.max_rect();
                ui.painter().line_segment(
                    [
                        egui::pos2(x, r.top() + 2.0),
                        egui::pos2(x, r.bottom() - 2.0),
                    ],
                    Stroke::new(2.0, pal.accent),
                );
                if ui.input(|i| i.pointer.any_released()) {
                    ops.push(BarOp::Move {
                        id,
                        parent: ferrosurf_core::bookmarks::TOOLBAR_ID,
                        index: Some(index),
                    });
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let other = env.profile.bookmarks.other().children().to_vec();
            if !other.is_empty() {
                let resp = ui.add(
                    egui::Button::new(
                        RichText::new(format!("{}  Otros marcadores", icon::FOLDER)).small(),
                    )
                    .fill(Color32::TRANSPARENT),
                );
                egui::Popup::menu(&resp).show(|ui| {
                    egui::ScrollArea::vertical()
                        .max_height(480.0)
                        .show(ui, |ui| {
                            for n in &other {
                                folder_menu_item(ui, n, cmds);
                            }
                        });
                });
            }
        });
    });
    // Context menu on the empty part of the bar.
    let current = w
        .active_tab()
        .map(|t| t.url_string())
        .filter(|u| !u.is_empty());
    bar.response.interact(Sense::click()).context_menu(|ui| {
        if ui
            .add_enabled(
                current.is_some(),
                egui::Button::new(format!("{}  Añadir página actual", icon::STAR)),
            )
            .clicked()
        {
            ops.push(BarOp::AddCurrent {
                parent: ferrosurf_core::bookmarks::TOOLBAR_ID,
            });
            ui.close();
        }
        if ui
            .button(format!("{}  Nueva carpeta", icon::FOLDER_PLUS))
            .clicked()
        {
            ops.push(BarOp::NewFolder {
                parent: ferrosurf_core::bookmarks::TOOLBAR_ID,
            });
            ui.close();
        }
        ui.separator();
        if ui
            .button(format!("{}  Gestionar marcadores", icon::BOOKMARKS_SIMPLE))
            .clicked()
        {
            cmds.push(Cmd::Action(Action::ShowBookmarks));
            ui.close();
        }
        if ui
            .button(format!(
                "{}  Ocultar la barra de marcadores",
                icon::EYE_SLASH
            ))
            .clicked()
        {
            cmds.push(Cmd::Action(Action::ToggleBookmarksBar));
            ui.close();
        }
    });

    // Inline rename popup (new folders and "Renombrar").
    if let Some((id, mut text)) = renaming {
        let mut done = None;
        egui::Area::new(egui::Id::new("bar-rename-area"))
            .order(egui::Order::Foreground)
            .fixed_pos(bar.response.rect.left_bottom() + Vec2::new(8.0, 4.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new("Nombre de la carpeta").small());
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(220.0));
                    r.request_focus();
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = Some(true);
                    } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        done = Some(false);
                    }
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, "Guardar").clicked() {
                            done = Some(true);
                        }
                        if widgets::subtle_button(ui, "Cancelar").clicked() {
                            done = Some(false);
                        }
                    });
                });
            });
        match done {
            Some(true) => {
                ops.push(BarOp::Rename(id, text));
                ui.ctx().data_mut(|d| {
                    d.remove::<(BookmarkId, String)>(egui::Id::new("bar-rename"));
                });
            }
            Some(false) => ui.ctx().data_mut(|d| {
                d.remove::<(BookmarkId, String)>(egui::Id::new("bar-rename"));
            }),
            None => ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new("bar-rename"), (id, text));
            }),
        }
    }

    for op in ops {
        let res = match op {
            BarOp::Move { id, parent, index } => {
                // Moving within the same folder: account for the removed slot.
                let mut index = index;
                if let Some(ix) = index
                    && let Some(old) = env
                        .profile
                        .bookmarks
                        .toolbar()
                        .children()
                        .iter()
                        .position(|n| n.id() == id)
                    && parent == ferrosurf_core::bookmarks::TOOLBAR_ID
                    && old < ix
                {
                    index = Some(ix - 1);
                }
                env.profile.bookmarks.move_to(id, parent, index)
            }
            BarOp::NewFolder { parent } => env
                .profile
                .bookmarks
                .add_folder(parent, "Nueva carpeta")
                .map(|id| {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(
                            egui::Id::new("bar-rename"),
                            (id, "Nueva carpeta".to_string()),
                        )
                    });
                }),
            BarOp::AddCurrent { parent } => match w.active_tab() {
                Some(t) => env
                    .profile
                    .bookmarks
                    .add(parent, &t.display_title(), &t.url_string())
                    .map(|_| ()),
                None => Ok(()),
            },
            BarOp::Remove(id) => env.profile.bookmarks.remove(id).map(|_| ()),
            BarOp::Rename(id, title) => env.profile.bookmarks.rename(id, title.trim()),
        };
        if let Err(e) = res {
            env.shared.toast(&format!("Marcadores: {e}"));
        }
    }
}

/// One bookmark or folder in the bar. Returns its response (rect used for drops).
fn bookmark_bar_item(
    ui: &mut Ui,
    node: &BookmarkNode,
    favicons: &crate::favicons::FaviconStore,
    cmds: &mut Vec<Cmd>,
    ops: &mut Vec<BarOp>,
) -> egui::Response {
    let pal = Palette::get(ui.ctx());
    let (glyph_folder, title) = match node {
        BookmarkNode::Bookmark(b) => (false, b.title.chars().take(24).collect::<String>()),
        BookmarkNode::Folder { title, .. } => (true, title.chars().take(24).collect::<String>()),
    };
    let text_w = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            title.clone(),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
        )
        .size()
        .x
    });
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(text_w + 30.0, 24.0), Sense::click_and_drag());
    let being_dragged =
        egui::DragAndDrop::payload::<BmDrag>(ui.ctx()).is_some_and(|p| p.0 == node.id());
    if resp.hovered() || being_dragged {
        ui.painter().rect_filled(rect, 6.0, pal.tab_hover);
    }
    let icon_rect =
        Rect::from_center_size(rect.left_center() + Vec2::new(12.0, 0.0), Vec2::splat(13.0));
    match node {
        BookmarkNode::Bookmark(b) => {
            let tex = crate::favicons::texture(ui.ctx(), favicons, &b.url);
            widgets::paint_site_icon(ui.painter(), icon_rect, tex.as_ref(), &b.title, &b.url)
        }
        BookmarkNode::Folder { .. } => {
            ui.painter().text(
                icon_rect.center(),
                egui::Align2::CENTER_CENTER,
                icon::FOLDER,
                egui::FontId::proportional(13.0),
                pal.text_dim,
            );
        }
    }
    ui.painter().text(
        rect.left_center() + Vec2::new(24.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &title,
        egui::FontId::proportional(12.0),
        if being_dragged {
            pal.text_dim
        } else {
            pal.text
        },
    );
    resp.dnd_set_drag_payload(BmDrag(node.id()));
    let id = node.id();
    match node {
        BookmarkNode::Bookmark(b) => {
            let url = b.url.clone();
            if resp.clicked() {
                let mods = ui.input(|i| i.modifiers);
                if let Ok(u) = url::Url::parse(&url) {
                    cmds.push(Cmd::OpenUrl {
                        url: u,
                        disposition: if mods.command {
                            Disposition::NewBackgroundTab
                        } else {
                            Disposition::CurrentTab
                        },
                    });
                }
            }
            if resp.middle_clicked()
                && let Ok(u) = url::Url::parse(&url)
            {
                cmds.push(Cmd::OpenUrl {
                    url: u,
                    disposition: Disposition::NewBackgroundTab,
                });
            }
            resp.clone()
                .on_hover_text(format!("{}\n{}", b.title, b.url))
                .context_menu(|ui| {
                    if ui
                        .button(format!(
                            "{}  Abrir en una pestaña nueva",
                            icon::ARROW_SQUARE_OUT
                        ))
                        .clicked()
                        && let Ok(u) = url::Url::parse(&url)
                    {
                        cmds.push(Cmd::OpenUrl {
                            url: u,
                            disposition: Disposition::NewForegroundTab,
                        });
                    }
                    if ui
                        .button(format!("{}  Copiar enlace", icon::LINK))
                        .clicked()
                    {
                        ui.ctx().copy_text(url.clone());
                    }
                    if ui
                        .button(format!("{}  Editar…", icon::PENCIL_SIMPLE))
                        .clicked()
                    {
                        cmds.push(Cmd::Action(Action::ShowBookmarks));
                    }
                    ui.separator();
                    if ui
                        .button(format!("{}  Nueva carpeta", icon::FOLDER_PLUS))
                        .clicked()
                    {
                        ops.push(BarOp::NewFolder {
                            parent: ferrosurf_core::bookmarks::TOOLBAR_ID,
                        });
                    }
                    if ui.button(format!("{}  Eliminar", icon::TRASH)).clicked() {
                        ops.push(BarOp::Remove(id));
                    }
                });
        }
        BookmarkNode::Folder {
            title, children, ..
        } => {
            let _ = glyph_folder;
            let popup_id = egui::Id::new(("bar-folder", id));
            if resp.clicked() {
                egui::Popup::toggle_id(ui.ctx(), popup_id);
            }
            egui::Popup::from_response(&resp)
                .id(popup_id)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    egui::ScrollArea::vertical()
                        .max_height(480.0)
                        .show(ui, |ui| {
                            if children.is_empty() {
                                ui.label(RichText::new("(vacía)").weak());
                            }
                            for c in children {
                                folder_menu_item(ui, c, cmds);
                            }
                            let urls: Vec<String> = children
                                .iter()
                                .filter_map(|c| match c {
                                    BookmarkNode::Bookmark(b) => Some(b.url.clone()),
                                    _ => None,
                                })
                                .collect();
                            if urls.len() > 1 {
                                ui.separator();
                                if ui.button(format!("Abrir todos ({})", urls.len())).clicked() {
                                    for u in urls {
                                        if let Ok(u) = url::Url::parse(&u) {
                                            cmds.push(Cmd::OpenUrl {
                                                url: u,
                                                disposition: Disposition::NewBackgroundTab,
                                            });
                                        }
                                    }
                                }
                            }
                        });
                });
            let title = title.clone();
            resp.clone().context_menu(|ui| {
                if ui
                    .button(format!("{}  Nueva carpeta dentro", icon::FOLDER_PLUS))
                    .clicked()
                {
                    ops.push(BarOp::NewFolder { parent: id });
                }
                if ui
                    .button(format!("{}  Añadir página actual aquí", icon::STAR))
                    .clicked()
                {
                    ops.push(BarOp::AddCurrent { parent: id });
                }
                if ui
                    .button(format!("{}  Renombrar", icon::PENCIL_SIMPLE))
                    .clicked()
                {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new("bar-rename"), (id, title.clone()))
                    });
                }
                ui.separator();
                if ui
                    .button(format!("{}  Eliminar carpeta", icon::TRASH))
                    .clicked()
                {
                    ops.push(BarOp::Remove(id));
                }
            });
        }
    }
    resp
}

fn folder_menu_item(ui: &mut Ui, node: &BookmarkNode, cmds: &mut Vec<Cmd>) {
    match node {
        BookmarkNode::Bookmark(b) => {
            let title: String = b.title.chars().take(60).collect();
            if ui
                .button(format!("{}  {}", icon::GLOBE_SIMPLE, title))
                .on_hover_text(&b.url)
                .clicked()
                && let Ok(u) = url::Url::parse(&b.url)
            {
                cmds.push(Cmd::OpenUrl {
                    url: u,
                    disposition: Disposition::CurrentTab,
                });
            }
        }
        BookmarkNode::Folder {
            title, children, ..
        } => {
            ui.menu_button(format!("{}  {}", icon::FOLDER, title), |ui| {
                for c in children {
                    folder_menu_item(ui, c, cmds);
                }
            });
        }
    }
}
