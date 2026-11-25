//! Tab strip (horizontal) and tab sidebar (vertical).

use egui::{Color32, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};
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

/// Per-frame animated factors of one tab (all 0..1).
#[derive(Clone, Copy)]
struct Fx {
    hover: f32,
    active: f32,
    load: f32,
    close: f32,
    /// Fades the whole tab in while it grows from zero width.
    appear: f32,
}

fn tab_fx(ctx: &egui::Context, t: &TabView, hovered: bool, key: Id, appear: f32) -> Fx {
    Fx {
        hover: widgets::fade(ctx, key.with("hv"), hovered, 0.14),
        active: widgets::fade(ctx, key.with("ac"), t.active, 0.18),
        load: widgets::fade(ctx, key.with("ld"), t.loading, 0.22),
        close: widgets::fade(ctx, key.with("cl"), hovered || t.active, 0.14),
        appear,
    }
}

/// Favicon and spinner, cross-faded according to `fx.load`.
fn paint_tab_icon(
    painter: &egui::Painter,
    icon_rect: Rect,
    t: &TabView,
    fx: &Fx,
    pal: &Palette,
    time: f64,
) {
    if fx.load < 1.0 {
        let mut p = painter.clone();
        p.multiply_opacity(1.0 - fx.load);
        widgets::paint_site_icon(&p, icon_rect, t.favicon.as_ref(), &t.title, &t.url);
    }
    if fx.load > 0.0 {
        widgets::paint_spinner(
            painter,
            icon_rect.center(),
            6.5,
            pal.accent.gamma_multiply(fx.load),
            time,
        );
    }
    if t.asleep && !t.active {
        painter.circle_filled(icon_rect.right_bottom(), 3.0, pal.text_dim);
    }
}

fn fx_bg(pal: &Palette, fx: &Fx) -> Color32 {
    pal.bg
        .lerp_to_gamma(pal.tab_hover, fx.hover)
        .lerp_to_gamma(pal.tab_active, fx.active)
}

fn paint_tab(
    ui: &Ui,
    rect: Rect,
    t: &TabView,
    fx: &Fx,
    pal: &Palette,
    compact: bool,
) -> Option<Rect> {
    let mut painter = ui.painter_at(rect.expand(2.0));
    painter.set_opacity(fx.appear.clamp(0.0, 1.0));
    let radius = egui::CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    if fx.active > 0.0 {
        painter.rect_filled(rect, radius, pal.tab_active.gamma_multiply(fx.active));
    }
    let hover_a = fx.hover * (1.0 - fx.active);
    if hover_a > 0.0 {
        painter.rect_filled(
            rect.shrink2(Vec2::new(1.0, 2.0)),
            8.0,
            pal.tab_hover.gamma_multiply(hover_a),
        );
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
    paint_tab_icon(&painter, icon_rect, t, fx, pal, ui.input(|i| i.time));
    if compact {
        return None;
    }
    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 16.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let mut text_right = widgets::lerp(rect.right() - 10.0, close_rect.left() - 4.0, fx.close);
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
        Pos2::new(text_right.max(icon_rect.right() + 8.0), rect.bottom()),
    );
    let color = pal.text_dim.lerp_to_gamma(pal.text, fx.active);
    let galley = painter.layout_no_wrap(t.title.clone(), egui::FontId::proportional(12.5), color);
    let text_pos = Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0);
    let clip = painter.with_clip_rect(text_rect);
    clip.galley(text_pos, galley.clone(), color);
    // Fade-out instead of hard clipping.
    if galley.size().x > text_rect.width() {
        let bg = fx_bg(pal, fx).gamma_multiply(painter.opacity());
        let fade = Rect::from_min_max(
            Pos2::new(text_rect.right() - 24.0, rect.top() + 2.0),
            Pos2::new(text_rect.right(), rect.bottom() - 2.0),
        );
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(fade.left_top(), bg.gamma_multiply(0.0));
        mesh.colored_vertex(fade.right_top(), bg);
        mesh.colored_vertex(fade.right_bottom(), bg);
        mesh.colored_vertex(fade.left_bottom(), bg.gamma_multiply(0.0));
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(mesh);
    }
    if fx.close > 0.4 {
        Some(close_rect)
    } else {
        None
    }
}

/// Close "x" glyph with a hover pill that fades; `alpha` follows the tab's close fade.
fn paint_close_button(
    ui: &Ui,
    painter: &egui::Painter,
    cr: Rect,
    key: Id,
    alpha: f32,
    pal: &Palette,
) {
    let hot = ui.rect_contains_pointer(cr) && alpha > 0.4;
    let t = widgets::fade(ui.ctx(), key.with("closeh"), hot, 0.12);
    if t > 0.0 {
        painter.rect_filled(cr, 9.0, pal.field.gamma_multiply(t * alpha));
    }
    painter.text(
        cr.center(),
        egui::Align2::CENTER_CENTER,
        icon::X,
        egui::FontId::proportional(12.0),
        pal.text_dim
            .lerp_to_gamma(pal.text, t)
            .gamma_multiply(alpha),
    );
}

/// Horizontal tab strip with a new-tab button.
pub fn tab_strip(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let ctx = ui.ctx().clone();
    let pal = Palette::get(&ctx);
    let tabs = snapshot(w);
    let height = 34.0;
    let avail = ui.available_width();
    let ready_id = Id::new("tabstrip-anim-ready");
    let ready = ctx.data(|d| d.get_temp::<bool>(ready_id)).unwrap_or(false);
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

        // Target layout (relative to the strip origin) and its smoothed counterpart.
        let drag_id = w.chrome.tab_drag.as_ref().map(|d| d.id);
        let gap = 2.0;
        let mut target = Vec::with_capacity(tabs.len());
        let mut cx = 0.0;
        for t in &tabs {
            let tw = if t.pinned { pinned_w } else { tab_w };
            target.push((cx, tw));
            cx += tw + gap;
        }
        let mut anim = Vec::with_capacity(tabs.len());
        let mut span: f32 = 0.0;
        for (t, &(tx, tw)) in tabs.iter().zip(&target) {
            let key = Id::new(("tabanim", t.id));
            let x = widgets::smooth(&ctx, key.with("x"), tx, None, 16.0);
            let aw = widgets::smooth(&ctx, key.with("w"), tw, ready.then_some(0.0), 16.0);
            span = span.max(x + aw + gap);
            anim.push((x, aw));
        }
        let (strip, _) = ui.allocate_exact_size(Vec2::new(span, height), Sense::hover());
        let mut rects = Vec::with_capacity(tabs.len());
        let mut order: Vec<usize> = (0..tabs.len())
            .filter(|&i| Some(tabs[i].id) != drag_id)
            .collect();
        order.extend((0..tabs.len()).filter(|&i| Some(tabs[i].id) == drag_id));
        for &(tx, tw) in &target {
            rects.push(Rect::from_min_size(
                strip.min + Vec2::new(tx, 0.0),
                Vec2::new(tw, height),
            ));
        }
        for i in order {
            let t = &tabs[i];
            let key = Id::new(("tabanim", t.id));
            let (mut x, aw) = anim[i];
            let width = target[i].1;
            let rect = Rect::from_min_size(strip.min + Vec2::new(x, 0.0), Vec2::new(aw, height));
            let resp = ui.interact(rect, Id::new(("tab", t.id)), Sense::click_and_drag());
            let hovered = resp.hovered() && drag_id.is_none_or(|d| d == t.id);
            let compact = t.pinned || width < 70.0;
            let mut paint_rect = rect;
            let dragging = drag_id == Some(t.id);
            let lift = widgets::fade(&ctx, key.with("lift"), dragging, 0.14);
            if dragging && let Some(p) = ctx.pointer_interact_pos() {
                let off = w
                    .chrome
                    .tab_drag
                    .as_ref()
                    .map(|d| d.grab_offset)
                    .unwrap_or(0.0);
                x = (p.x - off - strip.left()).clamp(0.0, (span - aw - gap).max(0.0));
                widgets::smooth_set(&ctx, key.with("x"), x);
                paint_rect = Rect::from_min_size(strip.min + Vec2::new(x, 0.0), rect.size());
            }
            if lift > 0.0 {
                paint_rect = paint_rect.translate(Vec2::new(0.0, -2.0 * lift));
                let shadow = egui::epaint::Shadow {
                    offset: [0, 3],
                    blur: 12,
                    spread: 0,
                    color: Color32::from_black_alpha((90.0 * lift) as u8),
                };
                ui.painter().add(shadow.as_shape(
                    paint_rect,
                    egui::CornerRadius {
                        nw: 8,
                        ne: 8,
                        sw: 4,
                        se: 4,
                    },
                ));
            }
            let appear = if width > 0.0 { aw / width } else { 1.0 };
            let mut fx = tab_fx(&ctx, t, hovered, key, appear);
            if lift > 0.0 {
                fx.hover = fx.hover.max(lift * 0.0);
            }
            let compact = compact && !(t.active && !t.pinned && width >= 48.0);
            let close_rect = paint_tab(ui, paint_rect, t, &fx, &pal, compact);
            let close_hover = close_rect.is_some_and(|r| ui.rect_contains_pointer(r));
            if let Some(cr) = close_rect {
                paint_close_button(
                    ui,
                    &ui.painter_at(rect.expand(2.0)),
                    cr,
                    key,
                    fx.close,
                    &pal,
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
                let grab = ctx
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
            if let Some(p) = ctx.pointer_interact_pos()
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
            ctx.request_repaint();
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
    ctx.data_mut(|d| d.insert_temp(ready_id, true));
    // Animate spinners (only while something loads or is still fading).
    if tabs.iter().any(|t| t.loading) {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

/// Vertical tab sidebar.
pub fn vertical_tabs(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let ctx = ui.ctx().clone();
    let pal = Palette::get(&ctx);
    let tabs = snapshot(w);
    let narrow = ui.available_width() < 120.0;
    let ready_id = Id::new("vtabs-anim-ready");
    let ready = ctx.data(|d| d.get_temp::<bool>(ready_id)).unwrap_or(false);
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
                let key = Id::new(("ptab", t.id));
                let fx = tab_fx(&ctx, t, resp.hovered(), key, 1.0);
                paint_tab(ui, rect, t, &fx, &pal, true);
                if fx.active > 0.0 {
                    ui.painter().rect_stroke(
                        rect,
                        8.0,
                        Stroke::new(1.5, pal.accent.gamma_multiply(fx.active)),
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
            let content_top = ui.cursor().top();
            let mut indicator: Option<(f32, f32)> = None; // (y relative to content, left)
            for (i, t) in tabs.iter().enumerate().filter(|(_, t)| !t.pinned) {
                let key = Id::new(("vtab", t.id));
                let row_h = widgets::smooth(&ctx, key.with("h"), 32.0, ready.then_some(0.0), 16.0);
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_h),
                    Sense::click_and_drag(),
                );
                if t.active {
                    indicator = Some((rect.top() - content_top, rect.left()));
                }
                let hovered = resp.hovered();
                let fx = tab_fx(&ctx, t, hovered, key, row_h / 32.0);
                let mut painter = ui.painter_at(rect);
                painter.set_opacity(fx.appear.clamp(0.0, 1.0));
                let painter = &painter;
                if fx.active > 0.0 {
                    painter.rect_filled(rect, 8.0, pal.toolbar.gamma_multiply(fx.active));
                }
                let hover_a = fx.hover * (1.0 - fx.active);
                if hover_a > 0.0 {
                    painter.rect_filled(rect, 8.0, pal.tab_hover.gamma_multiply(hover_a));
                }
                let center_y = rect.top() + 16.0;
                let icon_rect = Rect::from_center_size(
                    Pos2::new(rect.left() + 18.0, center_y),
                    Vec2::splat(16.0),
                );
                paint_tab_icon(painter, icon_rect, t, &fx, &pal, ui.input(|i| i.time));
                let close_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 16.0, center_y),
                    Vec2::splat(18.0),
                );
                let close_hover = ui.rect_contains_pointer(close_rect) && (hovered || t.active);
                if !narrow {
                    let text_right =
                        widgets::lerp(rect.right() - 10.0, close_rect.left() - 4.0, fx.close);
                    let text_rect = Rect::from_min_max(
                        Pos2::new(icon_rect.right() + 8.0, rect.top()),
                        Pos2::new(text_right.max(icon_rect.right() + 8.0), rect.bottom()),
                    );
                    let color = pal.text_dim.lerp_to_gamma(pal.text, fx.active);
                    let galley = painter.layout_no_wrap(
                        t.title.clone(),
                        egui::FontId::proportional(12.5),
                        color,
                    );
                    painter.with_clip_rect(text_rect).galley(
                        Pos2::new(text_rect.left(), center_y - galley.size().y / 2.0),
                        galley,
                        color,
                    );
                    if fx.close > 0.0 {
                        paint_close_button(ui, painter, close_rect, key, fx.close, &pal);
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
            // Sliding accent indicator bar for the active row.
            let ind_id = Id::new("vtabs-indicator-y");
            let present = indicator.is_some();
            let alpha = widgets::fade(&ctx, ind_id.with("a"), present, 0.16);
            if let Some((y, left)) = indicator {
                let y = widgets::smooth(&ctx, ind_id, y, None, 20.0);
                ctx.data_mut(|d| d.insert_temp(ind_id.with("left"), left));
                let _ = y;
            }
            if alpha > 0.0 {
                let y = ctx.data(|d| d.get_temp::<f32>(ind_id)).unwrap_or(0.0);
                let left = ctx
                    .data(|d| d.get_temp::<f32>(ind_id.with("left")))
                    .unwrap_or(0.0);
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        Pos2::new(left, content_top + y + 8.0),
                        Vec2::new(3.0, 16.0),
                    ),
                    2.0,
                    pal.accent.gamma_multiply(alpha),
                );
            }
        });
    ctx.data_mut(|d| d.insert_temp(ready_id, true));
    if tabs.iter().any(|t| t.loading) {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
    let _ = AppCmd::Quit; // keep import used in all cfgs
}
