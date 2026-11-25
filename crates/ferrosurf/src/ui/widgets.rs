//! Small reusable widgets for the chrome.

use egui::{Color32, Response, RichText, Sense, Ui, Vec2};

use super::Palette;

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
        if enabled && (resp.hovered() || resp.has_focus()) {
            let fill = if resp.is_pointer_button_down_on() {
                pal.field
            } else {
                pal.tab_hover
            };
            ui.painter()
                .rect_filled(rect.shrink(1.0), size.x / 2.0, fill);
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
            egui::FontId::proportional(17.0),
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
    if active {
        ui.painter().circle_filled(
            resp.rect.center_bottom() - egui::vec2(0.0, 4.0),
            2.0,
            pal.accent,
        );
    }
    if let Some(text) = badge {
        let pos = resp.rect.right_top() + egui::vec2(-7.0, 7.0);
        let galley =
            ui.painter()
                .layout_no_wrap(text, egui::FontId::proportional(9.5), pal.on_accent());
        let w = (galley.size().x + 6.0).max(14.0);
        let r = egui::Rect::from_center_size(pos, egui::vec2(w, 13.0));
        ui.painter().rect_filled(r, 7.0, pal.accent);
        ui.painter()
            .galley(r.center() - galley.size() / 2.0, galley, pal.on_accent());
    }
    resp
}

pub fn accent_button(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::get(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(pal.on_accent()).strong())
            .fill(pal.accent)
            .corner_radius(8),
    )
}

pub fn subtle_button(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::get(ui.ctx());
    ui.add(egui::Button::new(text).fill(pal.field).corner_radius(8))
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

/// Spinner-like loading arc.
pub fn paint_spinner(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    color: Color32,
    time: f64,
) {
    let start = (time * 6.0) as f32;
    let n = 20;
    let points: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = start + i as f32 / n as f32 * std::f32::consts::PI * 1.4;
            center + radius * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
}
