//! Navigation toolbar, address bar, menus and bookmarks bar.

use egui::{Color32, Rect, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::BookmarkNode;

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
        } else if widgets::icon_button(ui, icon::ARROW_CLOCKWISE, "Recargar (Ctrl+R)", has_tab).clicked() {
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
                && widgets::badge_button(ui, icon::SPARKLE, "Asistente IA (Ctrl+J)", None, w.chrome.assistant_open).clicked()
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
    let glyph = if enabled { icon::SHIELD_CHECK } else { icon::SHIELD_SLASH };
    let badge = (enabled && blocked > 0).then(|| if blocked > 99 { "99+".to_string() } else { blocked.to_string() });
    let resp = widgets::badge_button(ui, glyph, "Protección contra rastreadores", badge, false);
    egui::Popup::from_toggle_button_response(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(260.0);
            ui.label(RichText::new(format!("{}  Protección contra rastreadores", icon::SHIELD_CHECK)).strong());
            ui.add_space(4.0);
            ui.label(format!("Bloqueados en esta página: {blocked}"));
            ui.label(RichText::new(format!("Total en esta sesión: {}", env.shared.blocked_total)).small());
            ui.add_space(6.0);
            let mut on = enabled;
            if ui.checkbox(&mut on, "Bloquear anuncios y rastreadores").changed() {
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
        ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, pal.accent)));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
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
    widgets::paint_letter_avatar(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(22.0)), &env.profile.profile.initial(), color);
    let resp = resp.on_hover_text(format!("Perfil: {}", env.profile.profile.name));
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(240.0);
        ui.horizontal(|ui| {
            widgets::letter_avatar(ui, &env.profile.profile.initial(), color, 32.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&env.profile.profile.name).strong());
                ui.label(RichText::new(if w.guest { "Sesión de invitado" } else { "Perfil activo" }).small().color(pal.text_dim));
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
        if ui.button(format!("{}  Abrir ventana de invitado", icon::USER_CIRCLE_DASHED)).clicked() {
            cmds.push(Cmd::App(AppCmd::NewGuestWindow));
        }
        if ui.button(format!("{}  Gestionar perfiles", icon::USERS_THREE)).clicked() {
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
                if ui.button(icon::CORNERS_OUT).on_hover_text("Pantalla completa (F11)").clicked() {
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
        item(ui, icon::SPARKLE, Action::ToggleAssistant, cmds);
        item(ui, icon::CAMERA, Action::Screenshot, cmds);
        ui.menu_button(format!("{}  Más herramientas", icon::WRENCH), |ui| {
            if menu_item(ui, icon::CODE, &Action::ViewSource.label(), km.shortcut_for(Action::ViewSource)) {
                cmds.push(Cmd::Action(Action::ViewSource));
            }
            if menu_item(ui, icon::BUG, &Action::DevTools.label(), km.shortcut_for(Action::DevTools)) {
                cmds.push(Cmd::Action(Action::DevTools));
            }
            if menu_item(ui, icon::SIDEBAR, &Action::ToggleVerticalTabs.label(), km.shortcut_for(Action::ToggleVerticalTabs)) {
                cmds.push(Cmd::Action(Action::ToggleVerticalTabs));
            }
            if menu_item(ui, icon::BOOKMARK_SIMPLE, &Action::ToggleBookmarksBar.label(), km.shortcut_for(Action::ToggleBookmarksBar)) {
                cmds.push(Cmd::Action(Action::ToggleBookmarksBar));
            }
            if menu_item(ui, icon::BROOM, &Action::ClearBrowsingData.label(), km.shortcut_for(Action::ClearBrowsingData)) {
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
pub fn bookmarks_bar(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let children: Vec<BookmarkNode> = env.profile.bookmarks.toolbar().children().to_vec();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.set_height(24.0);
        if children.is_empty() {
            ui.label(
                RichText::new(format!("{}  Pulsa Ctrl+D o la estrella para guardar páginas aquí", icon::STAR))
                    .small()
                    .color(pal.text_dim),
            );
        }
        for node in &children {
            bookmark_bar_item(ui, node, cmds);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let other = env.profile.bookmarks.other().children().to_vec();
            if !other.is_empty() {
                let resp = ui.add(egui::Button::new(RichText::new(format!("{}  Otros marcadores", icon::FOLDER)).small()).fill(Color32::TRANSPARENT));
                egui::Popup::menu(&resp).show(|ui| {
                    egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                        for n in &other {
                            folder_menu_item(ui, n, cmds);
                        }
                    });
                });
            }
        });
    });
    let _ = w;
}

fn bookmark_bar_item(ui: &mut Ui, node: &BookmarkNode, cmds: &mut Vec<Cmd>) {
    match node {
        BookmarkNode::Bookmark(b) => {
            let title: String = b.title.chars().take(24).collect();
            let (rect, resp) = {
                let galley_w = ui.fonts_mut(|f| f.layout_no_wrap(title.clone(), egui::FontId::proportional(12.0), Color32::WHITE).size().x);
                ui.allocate_exact_size(Vec2::new(galley_w + 30.0, 24.0), Sense::click())
            };
            let pal = Palette::get(ui.ctx());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 6.0, pal.tab_hover);
            }
            let icon_rect = Rect::from_center_size(rect.left_center() + Vec2::new(12.0, 0.0), Vec2::splat(13.0));
            widgets::paint_site_icon(ui.painter(), icon_rect, None, &b.title, &b.url);
            ui.painter().text(
                rect.left_center() + Vec2::new(24.0, 0.0),
                egui::Align2::LEFT_CENTER,
                &title,
                egui::FontId::proportional(12.0),
                pal.text,
            );
            let url = b.url.clone();
            if resp.clicked() {
                let mods = ui.input(|i| i.modifiers);
                if let Ok(u) = url::Url::parse(&url) {
                    cmds.push(Cmd::OpenUrl {
                        url: u,
                        disposition: if mods.command { Disposition::NewBackgroundTab } else { Disposition::CurrentTab },
                    });
                }
            }
            if resp.middle_clicked() {
                if let Ok(u) = url::Url::parse(&url) {
                    cmds.push(Cmd::OpenUrl { url: u, disposition: Disposition::NewBackgroundTab });
                }
            }
            resp.on_hover_text(format!("{}\n{}", b.title, b.url)).context_menu(|ui| {
                if ui.button("Abrir en una pestaña nueva").clicked() {
                    if let Ok(u) = url::Url::parse(&url) {
                        cmds.push(Cmd::OpenUrl { url: u, disposition: Disposition::NewForegroundTab });
                    }
                }
                if ui.button("Copiar enlace").clicked() {
                    ui.ctx().copy_text(url.clone());
                }
                if ui.button("Gestionar marcadores…").clicked() {
                    cmds.push(Cmd::Action(Action::ShowBookmarks));
                }
            });
        },
        BookmarkNode::Folder { title, children, .. } => {
            let resp = ui.add(egui::Button::new(RichText::new(format!("{}  {}", icon::FOLDER, title)).size(12.0)).fill(Color32::TRANSPARENT));
            egui::Popup::menu(&resp).show(|ui| {
                egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
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
                                    cmds.push(Cmd::OpenUrl { url: u, disposition: Disposition::NewBackgroundTab });
                                }
                            }
                        }
                    }
                });
            });
        },
    }
}

fn folder_menu_item(ui: &mut Ui, node: &BookmarkNode, cmds: &mut Vec<Cmd>) {
    match node {
        BookmarkNode::Bookmark(b) => {
            let title: String = b.title.chars().take(60).collect();
            if ui.button(format!("{}  {}", icon::GLOBE_SIMPLE, title)).on_hover_text(&b.url).clicked() {
                if let Ok(u) = url::Url::parse(&b.url) {
                    cmds.push(Cmd::OpenUrl { url: u, disposition: Disposition::CurrentTab });
                }
            }
        },
        BookmarkNode::Folder { title, children, .. } => {
            ui.menu_button(format!("{}  {}", icon::FOLDER, title), |ui| {
                for c in children {
                    folder_menu_item(ui, c, cmds);
                }
            });
        },
    }
}
