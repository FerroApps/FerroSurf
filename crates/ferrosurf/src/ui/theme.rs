//! Visual theme: graphite dark / paper light with a configurable accent.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use ferrosurf_core::settings::{Density, ThemeMode};

use crate::profile_data::ProfileData;

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub fn is_dark(profile: &ProfileData, system: Option<winit::window::Theme>) -> bool {
    match profile.settings.appearance.theme {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => !matches!(system, Some(winit::window::Theme::Light)),
    }
}

/// Palette used by custom-drawn chrome widgets.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub accent: Color32,
    pub bg: Color32,
    pub toolbar: Color32,
    pub tab_active: Color32,
    pub tab_hover: Color32,
    pub field: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub border: Color32,
    pub danger: Color32,
    pub ok: Color32,
}

impl Palette {
    pub fn new(dark: bool, accent: Color32) -> Self {
        if dark {
            Self {
                dark,
                accent,
                bg: Color32::from_rgb(0x15, 0x17, 0x1c),
                toolbar: Color32::from_rgb(0x1f, 0x22, 0x29),
                tab_active: Color32::from_rgb(0x1f, 0x22, 0x29),
                tab_hover: Color32::from_rgb(0x26, 0x2a, 0x32),
                field: Color32::from_rgb(0x2a, 0x2e, 0x37),
                text: Color32::from_rgb(0xe8, 0xea, 0xee),
                text_dim: Color32::from_rgb(0x8d, 0x93, 0x9e),
                border: Color32::from_rgb(0x32, 0x37, 0x41),
                danger: Color32::from_rgb(0xff, 0x5f, 0x56),
                ok: Color32::from_rgb(0x3f, 0xcf, 0x8e),
            }
        } else {
            Self {
                dark,
                accent,
                bg: Color32::from_rgb(0xe4, 0xe6, 0xea),
                toolbar: Color32::from_rgb(0xf7, 0xf8, 0xfa),
                tab_active: Color32::from_rgb(0xf7, 0xf8, 0xfa),
                tab_hover: Color32::from_rgb(0xee, 0xf0, 0xf3),
                field: Color32::from_rgb(0xe9, 0xec, 0xf0),
                text: Color32::from_rgb(0x1d, 0x20, 0x26),
                text_dim: Color32::from_rgb(0x65, 0x6b, 0x76),
                border: Color32::from_rgb(0xd2, 0xd6, 0xdd),
                danger: Color32::from_rgb(0xd9, 0x3a, 0x30),
                ok: Color32::from_rgb(0x1f, 0x9d, 0x63),
            }
        }
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Palette>(egui::Id::new("ferro-palette")))
            .unwrap_or_else(|| Palette::new(true, Color32::from_rgb(0xff, 0x6a, 0x3d)))
    }

    /// Text color readable on top of the accent.
    pub fn on_accent(&self) -> Color32 {
        let a = self.accent;
        let lum = 0.299 * a.r() as f32 + 0.587 * a.g() as f32 + 0.114 * a.b() as f32;
        if lum > 160.0 { Color32::from_rgb(0x15, 0x17, 0x1c) } else { Color32::WHITE }
    }
}

pub fn window_bg(ctx: &egui::Context) -> [f32; 3] {
    let c = Palette::get(ctx).bg;
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

pub fn apply(ctx: &egui::Context, profile: &ProfileData, system: Option<winit::window::Theme>) {
    let dark = is_dark(profile, system);
    let accent = parse_hex(&profile.accent_hex()).unwrap_or(Color32::from_rgb(0xff, 0x6a, 0x3d));
    let pal = Palette::new(dark, accent);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ferro-palette"), pal));
    let app = &profile.settings.appearance;
    let radius = app.corner_radius.clamp(0.0, 16.0) as u8;

    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = pal.toolbar;
    v.window_fill = pal.toolbar;
    v.extreme_bg_color = pal.field;
    v.faint_bg_color = pal.tab_hover;
    v.override_text_color = Some(pal.text);
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent.gamma_multiply(if dark { 0.45 } else { 0.35 });
    v.selection.stroke = Stroke::new(1.0, accent);
    v.window_corner_radius = CornerRadius::same(radius.saturating_add(4));
    v.menu_corner_radius = CornerRadius::same(radius.saturating_add(2));
    v.window_stroke = Stroke::new(1.0, pal.border);
    v.window_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 45 }) };
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if dark { 100 } else { 40 }) };
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(radius);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = pal.field;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text);
    v.widgets.hovered.weak_bg_fill = pal.tab_hover;
    v.widgets.hovered.bg_fill = pal.tab_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, pal.text);
    v.widgets.active.weak_bg_fill = pal.field;
    v.widgets.active.bg_fill = accent;
    v.widgets.active.fg_stroke = Stroke::new(1.5, pal.text);
    v.widgets.open.weak_bg_fill = pal.field;
    ctx.set_visuals(v);

    let base = app.ui_font_size.clamp(10.0, 20.0);
    let (spacing, pad) = match app.density {
        Density::Compact => (4.0, 3.0),
        Density::Comfortable => (6.0, 5.0),
        Density::Spacious => (8.0, 7.0),
    };
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(base - 2.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(base, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(base, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(base + 7.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(base - 1.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(spacing, spacing);
        style.spacing.button_padding = egui::vec2(pad + 3.0, pad);
        style.spacing.interact_size.y = base + 2.0 * pad + 4.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.window_margin = egui::Margin::same(14);
        style.interaction.tooltip_delay = 0.4;
        style.animation_time = 0.12;
    });
    ctx.set_zoom_factor(app.ui_scale.clamp(0.6, 2.5));
}

/// Deterministic color for a host/letter avatar.
pub fn color_for(s: &str) -> Color32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.55, 0.78, 1.0).into()
}
