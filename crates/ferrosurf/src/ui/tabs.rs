//! Tab strip (horizontal) and tab sidebar (vertical).

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;

use super::{Palette, widgets};
use crate::actions::Action;
use crate::tab::TabId;
use crate::window::{AppCmd, BrowserWindow, Cmd, Env};

pub struct TabDrag {
    pub id: TabId,
    pub grab_offset: f32,
}

struct TabView {
    id: TabId,
    title: String,
    url: String,
    loading: bool,
    pinned: bool,
    active: bool,
    asleep: bool,
    favicon: Option<egui::TextureHandle>,
    audible: bool,
}

fn snapshot(w: &BrowserWindow) -> Vec<TabView> {
    w.tabs
        .iter()
        .enumerate()
        .map(|(i, t)| TabView {
            id: t.id,
            title: t.display_title(),
            url: t.url_string(),
            loading: t.is_loading(),
            pinned: t.pinned,
            active: i == w.active,
            asleep: t.is_asleep(),
            favicon: t.favicon.clone(),
            audible: t.audible,
        })
        .collect()
}

fn tab_context_menu(ui: &mut Ui, id: TabId, pinned: bool, cmds: &mut Vec<Cmd>) {
    if ui
        .button(format!("{}  Nueva pestaña a la derecha", icon::PLUS))
        .clicked()
    {
        cmds.push(Cmd::ActivateTab(id));
        cmds.push(Cmd::Action(Action::NewTab));
    }
    if ui
        .button(format!("{}  Recargar", icon::ARROW_CLOCKWISE))
        .clicked()
    {
        cmds.push(Cmd::ReloadTab(id));
    }
    if ui.button(format!("{}  Duplicar", icon::COPY)).clicked() {
        cmds.push(Cmd::DuplicateTab(id));
    }
    if ui
        .button(format!(
            "{}  {}",
            icon::PUSH_PIN,
            if pinned { "Desfijar" } else { "Fijar" }
        ))
        .clicked()
    {
        cmds.push(Cmd::TogglePin(id));
    }
    if ui
        .button(format!("{}  Añadir a marcadores", icon::STAR))
        .clicked()
    {
        cmds.push(Cmd::ActivateTab(id));
        cmds.push(Cmd::Action(Action::BookmarkPage));
    }
    ui.separator();
    if ui.button(format!("{}  Cerrar", icon::X)).clicked() {
        cmds.push(Cmd::CloseTab(id));
    }
    if ui.button("      Cerrar las demás pestañas").clicked() {
        cmds.push(Cmd::CloseOtherTabs(id));
    }
    if ui.button("      Cerrar pestañas a la derecha").clicked() {
        cmds.push(Cmd::CloseTabsToRight(id));
    }
    if ui
        .button(format!(
            "{}  Reabrir pestaña cerrada",
            icon::ARROW_COUNTER_CLOCKWISE
        ))
        .clicked()
    {
        cmds.push(Cmd::Action(Action::ReopenClosedTab));
    }
}

fn paint_tab(
    ui: &Ui,
    rect: Rect,
    t: &TabView,
    hovered: bool,
    pal: &Palette,
    compact: bool,
) -> Option<Rect> {
    let painter = ui.painter_at(rect.expand(2.0));
    let radius = egui::CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    if t.active {
        painter.rect_filled(rect, radius, pal.tab_active);
    } else if hovered {
        painter.rect_filled(rect.shrink2(Vec2::new(1.0, 2.0)), 8.0, pal.tab_hover);
    }
    let icon_rect = Rect::from_center_size(
        Pos2::new(
            if compact {
                rect.center().x
            } else {
                rect.left() + 18.0
            },
            rect.center().y,
        ),
        Vec2::splat(16.0),
    );
    if t.loading {
        let time = ui.input(|i| i.time);
        widgets::paint_spinner(&painter, icon_rect.center(), 6.5, pal.accent, time);
    } else {
        widgets::paint_site_icon(&painter, icon_rect, t.favicon.as_ref(), &t.title, &t.url);
    }
    if t.asleep && !t.active {
        painter.circle_filled(icon_rect.right_bottom(), 3.0, pal.text_dim);
    }
    if compact {
        return None;
    }
    let close_visible = t.active || hovered;
    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 16.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let mut text_right = if close_visible {
        close_rect.left() - 4.0
    } else {
        rect.right() - 10.0
    };
    if t.audible {
        painter.text(
            Pos2::new(text_right - 8.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon::SPEAKER_HIGH,
            egui::FontId::proportional(13.0),
            pal.text_dim,
        );
        text_right -= 18.0;
    }
    let text_rect = Rect::from_min_max(
        Pos2::new(icon_rect.right() + 8.0, rect.top()),
        Pos2::new(text_right, rect.bottom()),
    );
    let color = if t.active { pal.text } else { pal.text_dim };
    let galley = painter.layout_no_wrap(t.title.clone(), egui::FontId::proportional(12.5), color);
    let text_pos = Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0);
    let clip = painter.with_clip_rect(text_rect);
    clip.galley(text_pos, galley.clone(), color);
    // Fade-out instead of hard clipping.
    if galley.size().x > text_rect.width() {
        let bg = if t.active {
            pal.tab_active
        } else if hovered {
            pal.tab_hover
        } else {
            pal.bg
        };
        let fade = Rect::from_min_max(
            Pos2::new(text_rect.right() - 24.0, rect.top() + 2.0),
            Pos2::new(text_rect.right(), rect.bottom() - 2.0),
        );
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(fade.left_top(), Color32::TRANSPARENT);
        mesh.colored_vertex(fade.right_top(), bg);
        mesh.colored_vertex(fade.right_bottom(), bg);
        mesh.colored_vertex(fade.left_bottom(), Color32::TRANSPARENT);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(mesh);
    }
    if close_visible {
        Some(close_rect)
    } else {
        None
    }
}

/// Horizontal tab strip with a new-tab button.
pub fn tab_strip(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let tabs = snapshot(w);
    let height = 34.0;
    let avail = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // Profile badge on the left for non-default profiles / guests.
        if w.guest || env.profile.profiles.borrow().profiles().len() > 1 {
            let color = super::theme::parse_hex(&env.profile.profile.color).unwrap_or(pal.accent);
            let r = widgets::letter_avatar(ui, &env.profile.profile.initial(), color, 22.0)
                .on_hover_text(format!("Perfil: {}", env.profile.profile.name));
            if r.clicked() {
                cmds.push(Cmd::Action(Action::ProfileManager));
            }
            ui.add_space(6.0);
        }
        let pinned_w = 40.0;
        let n_pinned = tabs.iter().filter(|t| t.pinned).count() as f32;
        let n_normal = tabs.iter().filter(|t| !t.pinned).count().max(1) as f32;
        let free = (avail - 48.0 - n_pinned * (pinned_w + 2.0) - 40.0).max(60.0);
        let tab_w = (free / n_normal - 2.0).clamp(48.0, 230.0);

        let mut rects = Vec::with_capacity(tabs.len());
        let drag_id = w.chrome.tab_drag.as_ref().map(|d| d.id);
        for t in &tabs {
            let width = if t.pinned { pinned_w } else { tab_w };
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
            rects.push(rect);
            let hovered = resp.hovered();
            let compact = t.pinned || width < 70.0;
            let mut paint_rect = rect;
            if drag_id == Some(t.id)
                && let Some(p) = ui.ctx().pointer_interact_pos()
            {
                let off = w
                    .chrome
                    .tab_drag
                    .as_ref()
                    .map(|d| d.grab_offset)
                    .unwrap_or(0.0);
                paint_rect = rect.translate(Vec2::new(p.x - off - rect.left(), 0.0));
            }
            let close_rect = paint_tab(
                ui,
                paint_rect,
                t,
                hovered,
                &pal,
                compact && !(t.active && !t.pinned && width >= 48.0),
            );
            let close_hover = close_rect.is_some_and(|r| ui.rect_contains_pointer(r));
            if let Some(cr) = close_rect {
                if close_hover {
                    ui.painter().rect_filled(cr, 9.0, pal.field);
                }
                ui.painter().text(
                    cr.center(),
                    egui::Align2::CENTER_CENTER,
                    icon::X,
                    egui::FontId::proportional(12.0),
                    pal.text_dim,
                );
            }
            if resp.clicked() {
                if close_hover {
                    cmds.push(Cmd::CloseTab(t.id));
                } else {
                    cmds.push(Cmd::ActivateTab(t.id));
                }
            }
            if resp.middle_clicked() {
                cmds.push(Cmd::CloseTab(t.id));
            }
            if resp.drag_started() {
                let grab = ui
                    .ctx()
                    .pointer_interact_pos()
                    .map(|p| p.x - rect.left())
                    .unwrap_or(0.0);
                w.chrome.tab_drag = Some(TabDrag {
                    id: t.id,
                    grab_offset: grab,
                });
                cmds.push(Cmd::ActivateTab(t.id));
            }
            let title = t.title.clone();
            let url = t.url.clone();
            let resp = resp.on_hover_ui_at_pointer(|ui| {
                ui.label(egui::RichText::new(&title).strong());
                ui.label(egui::RichText::new(ferrosurf_core::util::pretty_url(&url)).small());
            });
            resp.context_menu(|ui| tab_context_menu(ui, t.id, t.pinned, cmds));
        }
        // Drag reorder: drop position by pointer x.
        if let Some(drag) = &w.chrome.tab_drag {
            let released = ui.input(|i| i.pointer.any_released());
            if let Some(p) = ui.ctx().pointer_interact_pos()
                && let Some(from) = tabs.iter().position(|t| t.id == drag.id)
            {
                let mut to = rects
                    .iter()
                    .position(|r| p.x < r.center().x)
                    .unwrap_or(rects.len());
                if to > from {
                    to -= 1;
                }
                if to != from
                    && tabs[from].pinned == tabs.get(to).map(|t| t.pinned).unwrap_or(false)
                {
                    cmds.push(Cmd::MoveTab { from, to });
                }
            }
            if released {
                w.chrome.tab_drag = None;
            }
            ui.ctx().request_repaint();
        }
        ui.add_space(4.0);
        if widgets::icon_button(ui, icon::PLUS, "Nueva pestaña (Ctrl+T)", true).clicked() {
            cmds.push(Cmd::Action(Action::NewTab));
        }
        // Double click on empty strip opens a new tab.
        let rest = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rest, Sense::click());
        if resp.double_clicked() {
            cmds.push(Cmd::Action(Action::NewTab));
        }
        resp.context_menu(|ui| {
            if ui
                .button(format!("{}  Nueva pestaña", icon::PLUS))
                .clicked()
            {
                cmds.push(Cmd::Action(Action::NewTab));
            }
            if ui
                .button(format!(
                    "{}  Reabrir pestaña cerrada",
                    icon::ARROW_COUNTER_CLOCKWISE
                ))
                .clicked()
            {
                cmds.push(Cmd::Action(Action::ReopenClosedTab));
            }
            if ui
                .button(format!("{}  Pestañas verticales", icon::SIDEBAR))
                .clicked()
            {
                cmds.push(Cmd::Action(Action::ToggleVerticalTabs));
            }
        });
    });
    // Animate spinners.
    if tabs.iter().any(|t| t.loading) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Vertical tab sidebar.
pub fn vertical_tabs(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let tabs = snapshot(w);
    let narrow = ui.available_width() < 120.0;
    ui.horizontal(|ui| {
        if !narrow {
            let color = super::theme::parse_hex(&env.profile.profile.color).unwrap_or(pal.accent);
            widgets::letter_avatar(ui, &env.profile.profile.initial(), color, 22.0);
            ui.label(egui::RichText::new(&env.profile.profile.name).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, icon::ROWS, "Pestañas horizontales", true).clicked() {
                    cmds.push(Cmd::Action(Action::ToggleVerticalTabs));
                }
            });
        }
    });
    ui.add_space(4.0);
    // Pinned tabs as a grid of icons.
    let pinned: Vec<&TabView> = tabs.iter().filter(|t| t.pinned).collect();
    if !pinned.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for t in pinned {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, 34.0), Sense::click());
                paint_tab(ui, rect, t, resp.hovered(), &pal, true);
                if t.active {
                    ui.painter().rect_stroke(
                        rect,
                        8.0,
                        Stroke::new(1.5, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                if resp.clicked() {
                    cmds.push(Cmd::ActivateTab(t.id));
                }
                if resp.middle_clicked() {
                    cmds.push(Cmd::CloseTab(t.id));
                }
                resp.on_hover_text(&t.title)
                    .context_menu(|ui| tab_context_menu(ui, t.id, true, cmds));
            }
        });
        ui.separator();
    }
    let new_tab = ui.add_sized(
        [ui.available_width(), 30.0],
        egui::Button::new(if narrow {
            icon::PLUS.to_string()
        } else {
            format!("{}  Nueva pestaña", icon::PLUS)
        })
        .fill(Color32::TRANSPARENT),
    );
    if new_tab.clicked() {
        cmds.push(Cmd::Action(Action::NewTab));
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, t) in tabs.iter().enumerate().filter(|(_, t)| !t.pinned) {
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 32.0),
                    Sense::click_and_drag(),
                );
                let hovered = resp.hovered();
                let painter = ui.painter();
                if t.active {
                    painter.rect_filled(rect, 8.0, pal.toolbar);
                    painter.rect_filled(
                        Rect::from_min_size(
                            rect.left_top() + Vec2::new(0.0, 8.0),
                            Vec2::new(3.0, 16.0),
                        ),
                        2.0,
                        pal.accent,
                    );
                } else if hovered {
                    painter.rect_filled(rect, 8.0, pal.tab_hover);
                }
                let icon_rect = Rect::from_center_size(
                    Pos2::new(rect.left() + 18.0, rect.center().y),
                    Vec2::splat(16.0),
                );
                if t.loading {
                    widgets::paint_spinner(
                        painter,
                        icon_rect.center(),
                        6.5,
                        pal.accent,
                        ui.input(|i| i.time),
                    );
                } else {
                    widgets::paint_site_icon(
                        painter,
                        icon_rect,
                        t.favicon.as_ref(),
                        &t.title,
                        &t.url,
                    );
                }
                let close_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 16.0, rect.center().y),
                    Vec2::splat(18.0),
                );
                let close_hover = ui.rect_contains_pointer(close_rect) && (hovered || t.active);
                if !narrow {
                    let text_rect = Rect::from_min_max(
                        Pos2::new(icon_rect.right() + 8.0, rect.top()),
                        Pos2::new(close_rect.left() - 4.0, rect.bottom()),
                    );
                    let color = if t.active { pal.text } else { pal.text_dim };
                    let galley = painter.layout_no_wrap(
                        t.title.clone(),
                        egui::FontId::proportional(12.5),
                        color,
                    );
                    painter.with_clip_rect(text_rect).galley(
                        Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0),
                        galley,
                        color,
                    );
                    if hovered || t.active {
                        if close_hover {
                            painter.rect_filled(close_rect, 9.0, pal.field);
                        }
                        painter.text(
                            close_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            icon::X,
                            egui::FontId::proportional(12.0),
                            pal.text_dim,
                        );
                    }
                }
                if resp.clicked() {
                    cmds.push(if close_hover && !narrow {
                        Cmd::CloseTab(t.id)
                    } else {
                        Cmd::ActivateTab(t.id)
                    });
                }
                if resp.middle_clicked() {
                    cmds.push(Cmd::CloseTab(t.id));
                }
                if resp.dragged()
                    && let Some(p) = ui.ctx().pointer_interact_pos()
                {
                    if p.y < rect.top() - 4.0 && i > 0 {
                        cmds.push(Cmd::MoveTab { from: i, to: i - 1 });
                    } else if p.y > rect.bottom() + 4.0 && i + 1 < tabs.len() {
                        cmds.push(Cmd::MoveTab { from: i, to: i + 1 });
                    }
                }
                let title = t.title.clone();
                resp.on_hover_text(title)
                    .context_menu(|ui| tab_context_menu(ui, t.id, false, cmds));
            }
        });
    if tabs.iter().any(|t| t.loading) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
    let _ = AppCmd::Quit; // keep import used in all cfgs
}
