//! Small reusable widgets for the chrome.

use egui::{Color32, Id, Response, RichText, Sense, Ui, Vec2};

use super::Palette;

// ---------------------------------------------------------------------------
// Animation helpers. Everything here only requests repaints while a transition
// is actually running, so an idle chrome costs nothing.
// ---------------------------------------------------------------------------

/// Ease-out cubic: fast start, gentle landing.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Ease-in-out cubic.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Linear interpolation.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Eased 0..1 value following `on` over `time` seconds (cubic-out both ways).
pub fn fade(ctx: &egui::Context, id: Id, on: bool, time: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, time, egui::emath::easing::cubic_out)
}

/// Frame-rate independent exponential smoothing of a value towards `target`.
/// `init` is the starting value the first time `id` is seen (defaults to `target`).
/// `rate` is roughly "1/seconds"; ~14 settles in about a fifth of a second.
pub fn smooth(ctx: &egui::Context, id: Id, target: f32, init: Option<f32>, rate: f32) -> f32 {
    let cur: f32 = ctx
        .data(|d| d.get_temp::<f32>(id))
        .or(init)
        .unwrap_or(target);
    let dt = ctx.input(|i| i.stable_dt).min(0.05);
    let mut next = cur + (target - cur) * (1.0 - (-rate * dt).exp());
    if (target - next).abs() < 0.002 * (1.0 + target.abs()) {
        next = target;
    } else {
        ctx.request_repaint();
    }
    if next != cur || ctx.data(|d| d.get_temp::<f32>(id)).is_none() {
        ctx.data_mut(|d| d.insert_temp(id, next));
    }
    next
}

/// Overwrite the stored value of a [`smooth`] animation (e.g. after a drag).
pub fn smooth_set(ctx: &egui::Context, id: Id, value: f32) {
    ctx.data_mut(|d| d.insert_temp(id, value));
}

/// Round, borderless icon button used across the toolbar.
pub fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str, enabled: bool) -> Response {
    let pal = Palette::get(ui.ctx());
    let size = Vec2::splat(ui.spacing().interact_size.y.max(28.0));
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let ctx = ui.ctx();
        let hot = enabled && (resp.hovered() || resp.has_focus());
        let hover_t = fade(ctx, resp.id.with("hover"), hot, 0.14);
        let press_t = fade(
            ctx,
            resp.id.with("press"),
            enabled && resp.is_pointer_button_down_on(),
            0.08,
        );
        if hover_t > 0.0 {
            let fill = pal
                .tab_hover
                .lerp_to_gamma(pal.field, press_t)
                .gamma_multiply(hover_t);
            ui.painter().circle_filled(
                rect.center(),
                ((size.x - 2.0) / 2.0) * (1.0 - 0.06 * press_t),
                fill,
            );
        }
        let color = if enabled {
            pal.text
        } else {
            pal.text_dim.gamma_multiply(0.5)
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(17.0 * (1.0 - 0.1 * press_t)),
            color,
        );
    }
    let resp = resp.on_hover_text(tooltip);
    if enabled {
        resp
    } else {
        resp.on_hover_cursor(egui::CursorIcon::Default)
    }
}

/// Icon button with a small numeric badge.
pub fn badge_button(
    ui: &mut Ui,
    glyph: &str,
    tooltip: &str,
    badge: Option<String>,
    active: bool,
) -> Response {
    let pal = Palette::get(ui.ctx());
    let resp = icon_button(ui, glyph, tooltip, true);
    let ctx = ui.ctx().clone();
    let active_t = fade(&ctx, resp.id.with("active"), active, 0.18);
    if active_t > 0.0 {
        ui.painter().circle_filled(
            resp.rect.center_bottom() - egui::vec2(0.0, 4.0),
            2.0 * active_t,
            pal.accent,
        );
    }
    // Badge: scales in/out and "pops" whenever the text changes.
    let state_id = resp.id.with("badge-state");
    let now = ui.input(|i| i.time);
    let (mut last_text, mut changed_at): (String, f64) = ctx
        .data(|d| d.get_temp(state_id))
        .unwrap_or((String::new(), f64::NEG_INFINITY));
    if let Some(t) = &badge
        && *t != last_text
    {
        if !last_text.is_empty() {
            changed_at = now;
        }
        last_text = t.clone();
        ctx.data_mut(|d| d.insert_temp(state_id, (last_text.clone(), changed_at)));
    }
    let appear = fade(&ctx, resp.id.with("badge-on"), badge.is_some(), 0.2);
    if appear > 0.0 && !last_text.is_empty() {
        let pop_t = ((now - changed_at) / 0.35) as f32;
        let pop = if (0.0..1.0).contains(&pop_t) {
            ctx.request_repaint();
            1.0 + 0.45 * (pop_t * std::f32::consts::PI).sin()
        } else {
            1.0
        };
        let scale = appear * pop;
        let pos = resp.rect.right_top() + egui::vec2(-7.0, 7.0);
        let galley = ui.painter().layout_no_wrap(
            last_text,
            egui::FontId::proportional(9.5 * scale.min(1.4)),
            pal.on_accent().gamma_multiply(appear),
        );
        let w = (galley.size().x + 6.0).max(14.0 * scale);
        let r = egui::Rect::from_center_size(pos, egui::vec2(w, 13.0 * scale));
        ui.painter()
            .rect_filled(r, 7.0 * scale, pal.accent.gamma_multiply(appear));
        ui.painter().galley(
            r.center() - galley.size() / 2.0,
            galley,
            pal.on_accent().gamma_multiply(appear),
        );
    }
    resp
}

/// Text button with an animated hover fill and press feedback.
fn animated_button(
    ui: &mut Ui,
    text: &str,
    base: Color32,
    hover: Color32,
    fg: Color32,
) -> Response {
    let enabled = ui.is_enabled();
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let pad = ui.spacing().button_padding;
    let size = (galley.size() + pad * 2.0).max(Vec2::new(0.0, ui.spacing().interact_size.y));
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let ctx = ui.ctx();
        let hover_t = fade(ctx, resp.id.with("hover"), enabled && resp.hovered(), 0.15);
        let press_t = fade(
            ctx,
            resp.id.with("press"),
            enabled && resp.is_pointer_button_down_on(),
            0.08,
        );
        let fill = base.lerp_to_gamma(hover, hover_t);
        let r = rect.shrink(press_t * 1.0);
        let opacity = if enabled { 1.0 } else { 0.45 };
        let mut painter = ui.painter().clone();
        painter.set_opacity(opacity);
        painter.rect_filled(r, 8.0, fill);
        painter.galley(r.center() - galley.size() / 2.0, galley, fg);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn accent_button(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::get(ui.ctx());
    let hover = pal.accent.lerp_to_gamma(Color32::WHITE, 0.14);
    animated_button(ui, text, pal.accent, hover, pal.on_accent())
}

pub fn subtle_button(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::get(ui.ctx());
    let hover = pal.field.lerp_to_gamma(pal.text, 0.09);
    animated_button(ui, text, pal.field, hover, pal.text)
}

/// Circular letter avatar (profiles, sites without favicon).
pub fn letter_avatar(ui: &mut Ui, letter: &str, color: Color32, diameter: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    paint_letter_avatar(ui.painter(), rect, letter, color);
    resp
}

pub fn paint_letter_avatar(
    painter: &egui::Painter,
    rect: egui::Rect,
    letter: &str,
    color: Color32,
) {
    painter.circle_filled(rect.center(), rect.width() / 2.0, color);
    let l: String = letter.chars().take(2).collect::<String>();
    let l = if l.chars().count() > 1 && l.chars().all(|c| c.is_alphanumeric()) {
        l.chars().take(1).collect()
    } else {
        l
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        l.to_uppercase(),
        egui::FontId::proportional(rect.width() * 0.52),
        Color32::WHITE,
    );
}

/// Favicon or letter fallback.
pub fn paint_site_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    favicon: Option<&egui::TextureHandle>,
    title: &str,
    url: &str,
) {
    if let Some(tex) = favicon {
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        let host = url::Url::parse(url).ok().and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        });
        if url.starts_with("ferro:") || url.is_empty() {
            let pal = Palette::get(&painter.ctx().clone());
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                egui_phosphor::regular::GLOBE_SIMPLE,
                egui::FontId::proportional(rect.height()),
                pal.accent,
            );
            return;
        }
        let key = host.clone().unwrap_or_else(|| title.to_string());
        let letter = key
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default();
        painter.rect_filled(rect, 4.0, super::theme::color_for(&key));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            letter.to_uppercase(),
            egui::FontId::proportional(rect.height() * 0.7),
            Color32::WHITE,
        );
    }
}

/// Section title used in panels and settings.
pub fn section(ui: &mut Ui, title: &str) {
    let pal = Palette::get(ui.ctx());
    ui.add_space(6.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(pal.text_dim),
    );
    ui.add_space(2.0);
}

/// Material-style loading arc: the head spins while the tail catches up, so the arc
/// breathes between short and long.
pub fn paint_spinner(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: Color32,
    time: f64,
) {
    use std::f32::consts::TAU;
    let t = time as f32;
    let rot = t * TAU * 0.85;
    let phase = (t * 0.75).fract();
    let tri = if phase < 0.5 {
        phase * 2.0
    } else {
        2.0 - phase * 2.0
    };
    let sweep = lerp(0.12, 0.72, ease_in_out_cubic(tri)) * TAU;
    let start = rot + phase * TAU * 0.5;
    let n = 24;
    let points: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = start + i as f32 / n as f32 * sweep;
            center + radius * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
}
