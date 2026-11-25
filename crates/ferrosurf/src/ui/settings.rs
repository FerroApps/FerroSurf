//! The settings window: a large centered window with a searchable sidebar and one page per area.

use std::collections::{BTreeMap, HashMap};

use egui::{Align, Color32, Layout, RichText, Sense, Ui, vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::import::{self, DetectedBrowser};
use ferrosurf_core::permissions::{Decision, PermissionKind};
use ferrosurf_core::profile::PROFILE_COLORS;
use ferrosurf_core::search::SearchEngine;
use ferrosurf_core::settings::{
    AssistantProvider, Density, McpServerConfig, McpTransportConfig, StartupBehavior, TabLayout,
    ThemeMode,
};

use super::theme::{parse_hex, to_hex};
use super::{Palette, SettingsPage, widgets};
use crate::actions::Action;
use crate::window::{AppCmd, BrowserWindow, Cmd, Disposition, Env};

#[derive(Default)]
struct McpForm {
    http: bool,
    name: String,
    command: String,
    args: String,
    env: String,
    url: String,
    headers: String,
}

#[derive(Default)]
struct NewEngine {
    name: String,
    keyword: String,
    url: String,
}

/// Scratch state of the settings window (per browser window).
#[derive(Default)]
pub struct SettingsState {
    filter: String,
    scale_edit: Option<f32>,
    font_edit: Option<f32>,
    radius_edit: Option<f32>,
    clear_history: bool,
    clear_cookies: bool,
    clear_cache: bool,
    new_engine: NewEngine,
    mcp_form: McpForm,
    mcp_import: String,
    ext_icons: HashMap<String, Option<egui::TextureHandle>>,
    confirm_remove_ext: Option<String>,
    logo: Option<egui::TextureHandle>,
    browsers: Option<Vec<DetectedBrowser>>,
    new_profile_name: String,
    new_profile_color: String,
    created_profile: Option<String>,
    editing_profile: Option<(String, String, String)>,
    confirm_delete_profile: Option<String>,
    capturing: Option<String>,
}

const PAGES: &[(SettingsPage, &str, &str, &str)] = &[
    (
        SettingsPage::Appearance,
        icon::PALETTE,
        "Apariencia",
        "tema color acento oscuro claro densidad pestañas fuente escala",
    ),
    (
        SettingsPage::Startup,
        icon::HOUSE,
        "Inicio",
        "inicio pagina de inicio nueva pestaña sesion arranque",
    ),
    (
        SettingsPage::Search,
        icon::MAGNIFYING_GLASS,
        "Búsqueda",
        "buscador motor sugerencias palabra clave",
    ),
    (
        SettingsPage::Privacy,
        icon::SHIELD_CHECK,
        "Privacidad y seguridad",
        "rastreadores https cookies historial permisos borrar datos",
    ),
    (
        SettingsPage::Downloads,
        icon::DOWNLOAD_SIMPLE,
        "Descargas",
        "carpeta descargar guardar",
    ),
    (
        SettingsPage::Assistant,
        icon::SPARKLE,
        "Asistente IA",
        "ia claude anthropic ollama modelo clave api openai",
    ),
    (
        SettingsPage::Mcp,
        icon::PLUG,
        "MCP",
        "servidor agentes herramientas protocolo",
    ),
    (
        SettingsPage::Extensions,
        icon::PUZZLE_PIECE,
        "Extensiones",
        "complementos addons instalar",
    ),
    (
        SettingsPage::Profiles,
        icon::USERS,
        "Perfiles",
        "usuarios cuentas invitado",
    ),
    (
        SettingsPage::Shortcuts,
        icon::KEYBOARD,
        "Atajos de teclado",
        "teclas combinaciones",
    ),
    (
        SettingsPage::Import,
        icon::UPLOAD_SIMPLE,
        "Importar datos",
        "marcadores historial chrome firefox exportar",
    ),
    (
        SettingsPage::Advanced,
        icon::GEAR,
        "Avanzado",
        "user agent devtools suspension pestañas carpeta",
    ),
    (
        SettingsPage::About,
        icon::INFO,
        "Acerca de",
        "version motor servo",
    ),
];

// ───────────────────────────── entry point ─────────────────────────────

pub fn window(ctx: &egui::Context, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let Some(mut page) = w.chrome.settings else {
        return;
    };
    let mut st = std::mem::take(&mut w.chrome.settings_state);
    let pal = Palette::get(ctx);
    let screen = ctx.content_rect().size();
    let size = vec2(
        860.0f32.min(screen.x - 24.0).max(320.0),
        620.0f32.min(screen.y - 24.0).max(240.0),
    );
    let mut open = true;

    egui::Window::new(RichText::new("Ajustes").strong())
        .id(egui::Id::new("ferrosurf_settings_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .min_size(vec2(640.0f32.min(size.x), 480.0f32.min(size.y)))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(pal.toolbar)
                .corner_radius(14)
                .inner_margin(0),
        )
        .show(ctx, |ui| {
            let total = ui.available_size();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    vec2(212.0, total.y),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_min_height(total.y);
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(192.0);
                                sidebar(ui, &mut st, &mut page, &pal);
                            });
                    },
                );
                let (r, _) = ui.allocate_exact_size(vec2(1.0, total.y), Sense::hover());
                ui.painter().rect_filled(r, 0.0, pal.border);
                let rest = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(rest, total.y),
                    Layout::top_down(Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(20, 14))
                                    .show(ui, |ui| {
                                        ui.set_width((ui.available_width()).max(300.0));
                                        ui.spacing_mut().item_spacing.y = 8.0;
                                        let title = PAGES
                                            .iter()
                                            .find(|p| p.0 == page)
                                            .map(|p| (p.1, p.2))
                                            .unwrap_or(("", ""));
                                        ui.label(
                                            RichText::new(format!("{}  {}", title.0, title.1))
                                                .size(22.0)
                                                .strong(),
                                        );
                                        ui.add_space(4.0);
                                        if page_ui(ui, page, &mut st, env, cmds) {
                                            env.profile.mark_settings_dirty();
                                            cmds.push(Cmd::App(AppCmd::SettingsChanged));
                                        }
                                        ui.add_space(20.0);
                                    });
                            });
                    },
                );
            });
        });

    w.chrome.settings_state = st;
    w.chrome.settings = if open { Some(page) } else { None };
}

fn sidebar(ui: &mut Ui, st: &mut SettingsState, page: &mut SettingsPage, pal: &Palette) {
    ui.add(
        egui::TextEdit::singleline(&mut st.filter)
            .hint_text(format!("{}  Buscar en ajustes", icon::MAGNIFYING_GLASS))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);
    let q = st.filter.trim().to_lowercase();
    ui.spacing_mut().item_spacing.y = 2.0;
    let mut any = false;
    for (p, glyph, label, keywords) in PAGES {
        if !q.is_empty() && !label.to_lowercase().contains(&q) && !keywords.contains(&q) {
            continue;
        }
        any = true;
        let selected = *p == *page;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        if selected {
            ui.painter()
                .rect_filled(rect, 8.0, pal.accent.gamma_multiply(0.18));
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 8.0, pal.tab_hover);
        }
        let color = if selected { pal.accent } else { pal.text };
        ui.painter().text(
            rect.left_center() + vec2(12.0, 0.0),
            egui::Align2::LEFT_CENTER,
            *glyph,
            egui::FontId::proportional(17.0),
            color,
        );
        ui.painter().text(
            rect.left_center() + vec2(40.0, 0.0),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::FontId::proportional(13.5),
            color,
        );
        if resp.clicked() {
            *page = *p;
        }
    }
    if !any {
        ui.label(RichText::new("Sin resultados").color(pal.text_dim));
    }
}

fn page_ui(
    ui: &mut Ui,
    page: SettingsPage,
    st: &mut SettingsState,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
) -> bool {
    match page {
        SettingsPage::Appearance => appearance(ui, st, env),
        SettingsPage::Startup => startup(ui, env),
        SettingsPage::Search => search(ui, st, env),
        SettingsPage::Privacy => privacy(ui, st, env, cmds),
        SettingsPage::Downloads => downloads(ui, env),
        SettingsPage::Assistant => assistant(ui, env),
        SettingsPage::Mcp => mcp(ui, st, env, cmds),
        SettingsPage::Extensions => extensions(ui, st, env, cmds),
        SettingsPage::Profiles => profiles(ui, st, env, cmds),
        SettingsPage::Shortcuts => shortcuts(ui, st, env),
        SettingsPage::Import => import_page(ui, st, env),
        SettingsPage::Advanced => advanced(ui, env),
        SettingsPage::About => about(ui, st, env, cmds),
    }
}

// ───────────────────────────── widgets ─────────────────────────────

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let pal = Palette::get(ui.ctx());
    egui::Frame::new()
        .fill(pal.bg)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            add(ui)
        })
        .inner
}

fn sep(ui: &mut Ui) {
    let pal = Palette::get(ui.ctx());
    ui.add_space(2.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(r, 0.0, pal.border.gamma_multiply(0.7));
    ui.add_space(2.0);
}

fn dim(ui: &mut Ui, text: &str) {
    let pal = Palette::get(ui.ctx());
    ui.label(RichText::new(text).small().color(pal.text_dim));
}

/// Label (and optional description) on the left, control right-aligned.
fn row<R>(
    ui: &mut Ui,
    label: &str,
    description: &str,
    add_control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let pal = Palette::get(ui.ctx());
    let total = ui.available_width();
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            vec2(total * 0.56, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(label);
                if !description.is_empty() {
                    ui.label(RichText::new(description).small().color(pal.text_dim));
                }
            },
        );
        ui.with_layout(Layout::right_to_left(Align::Center), add_control)
            .inner
    })
    .inner
}

/// Animated on/off switch.
fn switch(ui: &mut Ui, on: &mut bool) -> bool {
    let pal = Palette::get(ui.ctx());
    let (rect, mut resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    let mut changed = false;
    if resp.clicked() {
        *on = !*on;
        changed = true;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let off = if pal.dark {
        Color32::from_gray(0x55)
    } else {
        Color32::from_gray(0xbb)
    };
    let fill = off.lerp_to_gamma(pal.accent, t);
    ui.painter().rect_filled(rect, 11.0, fill);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    changed
}

/// Switch row. Returns true when toggled.
fn toggle(ui: &mut Ui, value: &mut bool, label: &str, description: &str) -> bool {
    row(ui, label, description, |ui| switch(ui, value))
}

fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let pal = Palette::get(ui.ctx());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (v, label) in options {
            let sel = *value == *v;
            let text = if sel {
                RichText::new(*label).color(pal.on_accent()).strong()
            } else {
                RichText::new(*label)
            };
            let btn = egui::Button::new(text)
                .fill(if sel { pal.accent } else { pal.field })
                .corner_radius(8);
            if ui.add(btn).clicked() && !sel {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

fn swatch(
    ui: &mut Ui,
    color: Color32,
    selected: bool,
    glyph: Option<&str>,
    tip: &str,
) -> egui::Response {
    let pal = Palette::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    ui.painter().circle_filled(rect.center(), 11.0, color);
    if selected {
        ui.painter()
            .circle_stroke(rect.center(), 13.5, egui::Stroke::new(2.0, pal.text));
    } else if resp.hovered() {
        ui.painter()
            .circle_stroke(rect.center(), 13.0, egui::Stroke::new(1.5, pal.text_dim));
    }
    if let Some(g) = glyph {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            g,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
    }
    resp.on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn text_row(ui: &mut Ui, label: &str, description: &str, value: &mut String, hint: &str) -> bool {
    row(ui, label, description, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(260.0),
        )
        .changed()
    })
}

/// Slider whose value is only committed on release (avoids rescaling the UI under the cursor).
fn deferred_slider(
    ui: &mut Ui,
    edit: &mut Option<f32>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    suffix: &str,
) -> bool {
    let mut v = edit.unwrap_or(*value);
    ui.spacing_mut().slider_width = 160.0;
    let r = ui.add(
        egui::Slider::new(&mut v, range)
            .step_by(step)
            .suffix(suffix)
            .fixed_decimals(2),
    );
    if r.changed() {
        *edit = Some(v);
    }
    if edit.is_some() && (r.drag_stopped() || (r.changed() && !r.dragged())) {
        *value = v;
        *edit = None;
        return true;
    }
    false
}

fn copy_box(ui: &mut Ui, text: &str) {
    let pal = Palette::get(ui.ctx());
    egui::Frame::new()
        .fill(pal.field)
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0)).wrap());
        });
    if widgets::subtle_button(ui, &format!("{}  Copiar", icon::COPY)).clicked() {
        ui.ctx().copy_text(text.to_string());
    }
}

fn load_png_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture(
        name,
        egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
        egui::TextureOptions::LINEAR,
    ))
}

// ───────────────────────────── pages ─────────────────────────────

fn appearance(ui: &mut Ui, st: &mut SettingsState, env: &mut Env) -> bool {
    let mut ch = false;
    let profile_color =
        parse_hex(&env.profile.profile.color).unwrap_or(Color32::from_rgb(0xff, 0x6a, 0x3d));
    let ap = &mut env.profile.settings.appearance;

    widgets::section(ui, "Tema");
    card(ui, |ui| {
        row(ui, "Tema", "Claro, oscuro o el del sistema", |ui| {
            ch |= segmented(
                ui,
                &mut ap.theme,
                &[
                    (ThemeMode::System, "Sistema"),
                    (ThemeMode::Light, "Claro"),
                    (ThemeMode::Dark, "Oscuro"),
                ],
            );
        });
        sep(ui);
        ui.label("Color de acento");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let is_profile = ap.accent.is_empty();
            if swatch(
                ui,
                profile_color,
                is_profile,
                Some(icon::USER),
                "Color del perfil",
            )
            .clicked()
            {
                ap.accent.clear();
                ch = true;
            }
            for hex in PROFILE_COLORS {
                let c = parse_hex(hex).unwrap_or(Color32::GRAY);
                if swatch(ui, c, ap.accent.eq_ignore_ascii_case(hex), None, hex).clicked() {
                    ap.accent = hex.to_string();
                    ch = true;
                }
            }
            let mut custom = parse_hex(&ap.accent).unwrap_or(profile_color);
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut custom,
                egui::color_picker::Alpha::Opaque,
            )
            .changed()
            {
                ap.accent = to_hex(custom);
                ch = true;
            }
            ui.label("Personalizado");
        });
        dim(ui, "«Perfil» usa el color del perfil activo.");
    });

    widgets::section(ui, "Interfaz");
    card(ui, |ui| {
        row(
            ui,
            "Densidad",
            "Espacio alrededor de pestañas y barras",
            |ui| {
                ch |= segmented(
                    ui,
                    &mut ap.density,
                    &[
                        (Density::Compact, "Compacta"),
                        (Density::Comfortable, "Cómoda"),
                        (Density::Spacious, "Amplia"),
                    ],
                );
            },
        );
        sep(ui);
        row(ui, "Disposición de pestañas", "", |ui| {
            ch |= segmented(
                ui,
                &mut ap.tab_layout,
                &[
                    (TabLayout::Horizontal, "Horizontal"),
                    (TabLayout::Vertical, "Vertical"),
                ],
            );
        });
        sep(ui);
        ch |= toggle(
            ui,
            &mut ap.show_bookmarks_bar,
            "Mostrar barra de marcadores",
            "",
        );
        sep(ui);
        ch |= toggle(ui, &mut ap.show_home_button, "Mostrar botón de inicio", "");
        sep(ui);
        ch |= toggle(
            ui,
            &mut ap.show_status_bubble,
            "Burbuja de estado",
            "Muestra la dirección de los enlaces al pasar el ratón",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut ap.web_dark_mode,
            "Modo oscuro en páginas web",
            "Pide a los sitios que usen su tema oscuro",
        );
    });

    widgets::section(ui, "Tamaño y forma");
    card(ui, |ui| {
        row(ui, "Escala de la interfaz", "", |ui| {
            ch |= deferred_slider(
                ui,
                &mut st.scale_edit,
                &mut ap.ui_scale,
                0.75..=1.75,
                0.05,
                "×",
            );
        });
        sep(ui);
        row(ui, "Tamaño de fuente", "", |ui| {
            ch |= deferred_slider(
                ui,
                &mut st.font_edit,
                &mut ap.ui_font_size,
                11.0..=18.0,
                0.5,
                " px",
            );
        });
        sep(ui);
        row(ui, "Radio de las esquinas", "", |ui| {
            ch |= deferred_slider(
                ui,
                &mut st.radius_edit,
                &mut ap.corner_radius,
                0.0..=14.0,
                1.0,
                " px",
            );
        });
    });
    ch
}

fn startup(ui: &mut Ui, env: &mut Env) -> bool {
    let mut ch = false;
    let s = &mut env.profile.settings;
    widgets::section(ui, "Al iniciar");
    card(ui, |ui| {
        for (v, label) in [
            (
                StartupBehavior::RestoreSession,
                "Restaurar la sesión anterior",
            ),
            (StartupBehavior::NewTab, "Abrir una pestaña nueva"),
            (StartupBehavior::Homepage, "Abrir la página de inicio"),
        ] {
            ch |= ui.radio_value(&mut s.startup, v, label).changed();
        }
    });
    widgets::section(ui, "Páginas");
    card(ui, |ui| {
        ch |= text_row(
            ui,
            "Página de inicio",
            "Botón de inicio y arranque con página de inicio",
            &mut s.homepage,
            "https://…",
        );
        sep(ui);
        ch |= text_row(
            ui,
            "Página de nueva pestaña",
            "ferro://newtab es la página integrada",
            &mut s.new_tab_url,
            "ferro://newtab",
        );
    });
    ch
}

fn sanitize_id(name: &str) -> String {
    let id: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let id = id.trim_matches('-').to_string();
    if id.is_empty() { "motor".into() } else { id }
}

fn search(ui: &mut Ui, st: &mut SettingsState, env: &mut Env) -> bool {
    let mut ch = false;
    let pal = Palette::get(ui.ctx());
    let s = &mut env.profile.settings;
    widgets::section(ui, "Buscador predeterminado");
    card(ui, |ui| {
        for e in s.search_engines() {
            ui.horizontal(|ui| {
                if ui.radio(s.search_engine == e.id, e.name.clone()).clicked() {
                    s.search_engine = e.id.clone();
                    ch = true;
                }
                if !e.keyword.is_empty() {
                    ui.label(
                        RichText::new(format!("palabra clave: {}", e.keyword))
                            .small()
                            .color(pal.text_dim),
                    );
                }
            });
        }
        sep(ui);
        ch |= toggle(
            ui,
            &mut s.privacy.search_suggestions,
            "Sugerencias de búsqueda",
            "Envía lo que escribes en la barra de direcciones al buscador",
        );
    });

    widgets::section(ui, "Buscadores personalizados");
    card(ui, |ui| {
        let mut remove = None;
        if s.custom_search_engines.is_empty() {
            dim(ui, "Aún no has añadido ninguno.");
        }
        for (i, e) in s.custom_search_engines.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&e.name).strong());
                ui.label(RichText::new(&e.keyword).monospace().color(pal.accent));
                ui.label(RichText::new(&e.url).small().color(pal.text_dim));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icon::TRASH, "Eliminar", true).clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            let gone = s.custom_search_engines.remove(i);
            if s.search_engine == gone.id {
                s.search_engine = "duckduckgo".into();
            }
            ch = true;
        }
        sep(ui);
        let n = &mut st.new_engine;
        ui.label(egui::RichText::new("Añadir un buscador").strong());
        ui.add_space(4.0);
        egui::Grid::new("new-engine-form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                let field_w = (ui.available_width() - 12.0).max(200.0);
                ui.label("Nombre");
                ui.add(
                    egui::TextEdit::singleline(&mut n.name)
                        .hint_text("p. ej. Wikipedia ES")
                        .margin(egui::Margin::symmetric(8, 5))
                        .desired_width(field_w),
                );
                ui.end_row();
                ui.label("Palabra clave");
                ui.add(
                    egui::TextEdit::singleline(&mut n.keyword)
                        .hint_text("p. ej. wes  →  escribe «wes término»")
                        .margin(egui::Margin::symmetric(8, 5))
                        .desired_width(field_w),
                );
                ui.end_row();
                ui.label("URL de búsqueda");
                ui.add(
                    egui::TextEdit::singleline(&mut n.url)
                        .hint_text("https://es.wikipedia.org/w/index.php?search={searchTerms}")
                        .margin(egui::Margin::symmetric(8, 5))
                        .desired_width(field_w),
                );
                ui.end_row();
            });
        ui.label(
            egui::RichText::new("Usa {searchTerms} donde deba ir lo que escribas.")
                .small()
                .weak(),
        );
        let valid = !n.name.trim().is_empty() && n.url.contains("{searchTerms}");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    valid,
                    egui::Button::new(format!("{}  Añadir buscador", icon::PLUS)).corner_radius(8),
                )
                .clicked()
            {
                let base = sanitize_id(&n.name);
                let existing: Vec<String> = s.search_engines().into_iter().map(|e| e.id).collect();
                let mut id = base.clone();
                let mut k = 2;
                while existing.contains(&id) {
                    id = format!("{base}-{k}");
                    k += 1;
                }
                s.custom_search_engines.push(SearchEngine {
                    id,
                    name: n.name.trim().to_string(),
                    url: n.url.trim().to_string(),
                    suggest_url: None,
                    keyword: n.keyword.trim().to_string(),
                });
                *n = NewEngine::default();
                ch = true;
            }
            dim(ui, "La URL debe contener {searchTerms}");
        });
    });
    ch
}

fn permission_label(k: &PermissionKind) -> String {
    match k {
        PermissionKind::Geolocation => "Ubicación".into(),
        PermissionKind::Notifications => "Notificaciones".into(),
        PermissionKind::Camera => "Cámara".into(),
        PermissionKind::Microphone => "Micrófono".into(),
        PermissionKind::Clipboard => "Portapapeles".into(),
        PermissionKind::Midi => "MIDI".into(),
        PermissionKind::Popups => "Ventanas emergentes".into(),
        PermissionKind::Autoplay => "Reproducción automática".into(),
        PermissionKind::Downloads => "Descargas múltiples".into(),
        PermissionKind::Other(s) => s.clone(),
    }
}

fn decision_label(d: Decision) -> &'static str {
    match d {
        Decision::Allow => "Permitir",
        Decision::Deny => "Bloquear",
        Decision::Ask => "Preguntar",
    }
}

fn privacy(ui: &mut Ui, st: &mut SettingsState, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let mut ch = false;
    let blocked = env.shared.blocked_total;
    let pr = &mut env.profile.settings.privacy;
    widgets::section(ui, "Protección");
    card(ui, |ui| {
        ch |= toggle(
            ui,
            &mut pr.block_trackers,
            "Bloquear anuncios y rastreadores",
            &format!("{blocked} solicitudes bloqueadas en esta sesión"),
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut pr.https_only,
            "Usar siempre HTTPS",
            "Convierte las direcciones http:// en https://",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut pr.do_not_track,
            "Enviar «No rastrear»",
            "Pide a los sitios que no te sigan",
        );
    });
    widgets::section(ui, "Historial");
    card(ui, |ui| {
        ch |= toggle(
            ui,
            &mut pr.remember_history,
            "Recordar el historial de navegación",
            "",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut pr.clear_history_on_exit,
            "Borrar el historial al salir",
            "",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut pr.clear_cookies_on_exit,
            "Borrar cookies y datos de sitios al salir",
            "",
        );
    });

    widgets::section(ui, "Borrar datos de navegación");
    card(ui, |ui| {
        ui.checkbox(&mut st.clear_history, "Historial de navegación");
        ui.checkbox(&mut st.clear_cookies, "Cookies y datos de sitios");
        ui.checkbox(&mut st.clear_cache, "Imágenes y archivos en caché");
        let any = st.clear_history || st.clear_cookies || st.clear_cache;
        if ui
            .add_enabled(
                any,
                egui::Button::new(format!("{}  Borrar datos", icon::TRASH)).corner_radius(8),
            )
            .clicked()
        {
            cmds.push(Cmd::App(AppCmd::ClearBrowsingData {
                history: st.clear_history,
                cookies: st.clear_cookies,
                cache: st.clear_cache,
            }));
            st.clear_history = false;
            st.clear_cookies = false;
            st.clear_cache = false;
        }
    });

    widgets::section(ui, "Permisos de sitios");
    card(ui, |ui| {
        let all = env.profile.permissions.all();
        if all.is_empty() {
            dim(
                ui,
                "Ningún sitio tiene permisos guardados. Se te preguntará cuando un sitio los solicite.",
            );
            return;
        }
        let mut change = None;
        for (i, (origin, kind, decision)) in all.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(origin).strong());
                ui.label(permission_label(kind));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut d = *decision;
                    egui::ComboBox::from_id_salt(("perm", i))
                        .selected_text(decision_label(d))
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for opt in [Decision::Allow, Decision::Deny, Decision::Ask] {
                                ui.selectable_value(&mut d, opt, decision_label(opt));
                            }
                        });
                    if d != *decision {
                        change = Some((origin.clone(), kind.clone(), d));
                    }
                });
            });
        }
        if let Some((o, k, d)) = change {
            env.profile.permissions.set(&o, k, d);
        }
        sep(ui);
        if widgets::subtle_button(ui, "Restablecer todos los permisos").clicked() {
            env.profile.permissions.clear_all();
        }
    });
    ch
}

fn downloads(ui: &mut Ui, env: &mut Env) -> bool {
    let mut ch = false;
    let d = &mut env.profile.settings.downloads;
    widgets::section(ui, "Descargas");
    card(ui, |ui| {
        row(
            ui,
            "Carpeta de descargas",
            &d.directory.display().to_string(),
            |ui| {
                if widgets::subtle_button(ui, "Cambiar…").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_directory(&d.directory)
                        .pick_folder()
                {
                    d.directory = dir;
                    ch = true;
                }
            },
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut d.ask_where_to_save,
            "Preguntar dónde guardar cada archivo",
            "",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut d.open_panel_on_start,
            "Abrir el panel de descargas al empezar una",
            "",
        );
    });
    ch
}

fn assistant(ui: &mut Ui, env: &mut Env) -> bool {
    let mut ch = false;
    let pal = Palette::get(ui.ctx());
    let a = &mut env.profile.settings.assistant;
    widgets::section(ui, "Asistente");
    card(ui, |ui| {
        ch |= toggle(
            ui,
            &mut a.enabled,
            "Activar el asistente IA",
            "Panel lateral para chatear sobre la página actual",
        );
        sep(ui);
        let old = a.provider;
        row(ui, "Proveedor", "", |ui| {
            segmented(
                ui,
                &mut a.provider,
                &[
                    (AssistantProvider::Anthropic, "Anthropic"),
                    (AssistantProvider::OpenAiCompatible, "Compatible con OpenAI"),
                ],
            );
        });
        if a.provider != old {
            ch = true;
            let anth = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
            let oai = ["llama3.2", "qwen2.5", "gpt-4o-mini"];
            let (from, to_model, from_env, to_env) = if a.provider == AssistantProvider::Anthropic {
                (
                    oai.as_slice(),
                    anth[1],
                    "OPENAI_API_KEY",
                    "ANTHROPIC_API_KEY",
                )
            } else {
                (
                    anth.as_slice(),
                    oai[0],
                    "ANTHROPIC_API_KEY",
                    "OPENAI_API_KEY",
                )
            };
            if a.model.is_empty() || from.contains(&a.model.as_str()) {
                a.model = to_model.into();
            }
            if a.api_key_env.is_empty() || a.api_key_env == from_env {
                a.api_key_env = to_env.into();
            }
            a.base_url.clear();
        }
        dim(
            ui,
            "Compatible con OpenAI: Ollama, LM Studio, OpenRouter, llama.cpp, vLLM…",
        );
        sep(ui);
        let (suggestions, url_hint): (&[&str], &str) = match a.provider {
            AssistantProvider::Anthropic => (
                &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
                "https://api.anthropic.com",
            ),
            AssistantProvider::OpenAiCompatible => (
                &[
                    "llama3.2",
                    "qwen2.5",
                    "qwen2.5-coder",
                    "mistral",
                    "gpt-4o-mini",
                ],
                "http://localhost:11434/v1",
            ),
        };
        row(ui, "Modelo", "Escribe uno o elige una sugerencia", |ui| {
            ui.horizontal(|ui| {
                ch |= ui
                    .add(egui::TextEdit::singleline(&mut a.model).desired_width(190.0))
                    .changed();
                ui.menu_button(icon::CARET_DOWN, |ui| {
                    for s in suggestions {
                        if ui.button(*s).clicked() {
                            a.model = s.to_string();
                            ch = true;
                            ui.close();
                        }
                    }
                });
            });
        });
        sep(ui);
        ch |= text_row(
            ui,
            "URL base",
            "Vacío = la del proveedor",
            &mut a.base_url,
            url_hint,
        );
        sep(ui);
        row(
            ui,
            "Clave de API",
            "Se guarda en texto plano en tu perfil; mejor usa una variable de entorno",
            |ui| {
                ch |= ui
                    .add(
                        egui::TextEdit::singleline(&mut a.api_key)
                            .password(true)
                            .desired_width(260.0),
                    )
                    .changed();
            },
        );
        sep(ui);
        ch |= text_row(
            ui,
            "Variable de entorno de la clave",
            "",
            &mut a.api_key_env,
            "ANTHROPIC_API_KEY",
        );
        match a.resolved_api_key() {
            Some(_) => {
                ui.label(
                    RichText::new(format!("{}  Clave de API encontrada", icon::CHECK_CIRCLE))
                        .color(pal.ok),
                );
            }
            None => {
                ui.label(
                    RichText::new(format!(
                        "{}  No hay clave de API (no hace falta para modelos locales)",
                        icon::WARNING
                    ))
                    .color(pal.danger),
                );
            }
        }
    });
    widgets::section(ui, "Comportamiento");
    card(ui, |ui| {
        row(ui, "Máximo de tokens por respuesta", "", |ui| {
            ch |= ui
                .add(
                    egui::DragValue::new(&mut a.max_tokens)
                        .range(256..=64000)
                        .speed(64),
                )
                .changed();
        });
        sep(ui);
        ch |= toggle(
            ui,
            &mut a.browser_tools,
            "Herramientas del navegador",
            "Permite leer la página, navegar, abrir pestañas…",
        );
        sep(ui);
        ui.label("Instrucciones del sistema personalizadas");
        ch |= ui
            .add(
                egui::TextEdit::multiline(&mut a.system_prompt)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("Opcional: tono, idioma, reglas…"),
            )
            .changed();
    });
    ch
}

/// Split a command line respecting single and double quotes.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_lines(s: &str, sepch: char) -> BTreeMap<String, String> {
    s.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(sepch)?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn import_mcp_json(text: &str) -> Result<Vec<McpServerConfig>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON no válido: {e}"))?;
    let servers = v
        .get("mcpServers")
        .and_then(|s| s.as_object())
        .ok_or("Falta el objeto «mcpServers»")?;
    let str_map = |v: Option<&serde_json::Value>| -> BTreeMap<String, String> {
        v.and_then(|m| m.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (name, cfg) in servers {
        let transport = if let Some(url) = cfg.get("url").and_then(|u| u.as_str()) {
            McpTransportConfig::Http {
                url: url.to_string(),
                headers: str_map(cfg.get("headers")),
            }
        } else if let Some(command) = cfg.get("command").and_then(|c| c.as_str()) {
            let args = cfg
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            McpTransportConfig::Stdio {
                command: command.to_string(),
                args,
                env: str_map(cfg.get("env")),
            }
        } else {
            return Err(format!("«{name}» no tiene «command» ni «url»"));
        };
        out.push(McpServerConfig {
            name: name.clone(),
            enabled: true,
            transport,
        });
    }
    Ok(out)
}

fn upsert_mcp(list: &mut Vec<McpServerConfig>, cfg: McpServerConfig) {
    if let Some(slot) = list.iter_mut().find(|c| c.name == cfg.name) {
        *slot = cfg;
    } else {
        list.push(cfg);
    }
}

fn mcp(ui: &mut Ui, st: &mut SettingsState, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let mut ch = false;
    let pal = Palette::get(ui.ctx());
    let mcp_url = env.shared.mcp_url.clone();
    let profile_id = env.profile.profile.id.clone();
    let mut reconnect = false;
    let mut restart = false;
    {
        let m = &mut env.profile.settings.mcp_server;
        widgets::section(ui, "Servidor MCP de FerroSurf");
        card(ui, |ui| {
            dim(
                ui,
                "Permite que agentes externos (Claude Desktop, Cursor, Claude Code…) controlen este navegador.",
            );
            ch |= toggle(ui, &mut m.enabled, "Activar el servidor MCP", "");
            sep(ui);
            row(ui, "Puerto", "0 = elegir uno libre automáticamente", |ui| {
                ch |= ui
                    .add(egui::DragValue::new(&mut m.port).range(0..=65535))
                    .changed();
            });
            sep(ui);
            ch |= toggle(
                ui,
                &mut m.allow_page_control,
                "Permitir controlar páginas",
                "Hacer clic, escribir y ejecutar JavaScript",
            );
            sep(ui);
            ch |= toggle(
                ui,
                &mut m.allow_data_access,
                "Permitir acceso a datos",
                "Leer historial y marcadores",
            );
            sep(ui);
            ch |= toggle(
                ui,
                &mut m.confirm_actions,
                "Confirmar acciones",
                "Pide confirmación antes de ejecutar herramientas que modifican algo",
            );
            sep(ui);
            ui.label("Dirección del servidor");
            if mcp_url.is_empty() {
                dim(ui, "El servidor no está en marcha.");
            } else {
                copy_box(ui, &mcp_url);
            }
            if widgets::subtle_button(
                ui,
                &format!("{}  Reiniciar servidor", icon::ARROW_CLOCKWISE),
            )
            .clicked()
            {
                restart = true;
            }
        });
    }

    widgets::section(ui, "Configuración para clientes MCP (stdio)");
    card(ui, |ui| {
        dim(ui, "Pega esto en la configuración de tu cliente MCP.");
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "ferrosurf".into());
        let json = serde_json::json!({"mcpServers": {"ferrosurf": {"command": exe, "args": ["--mcp-stdio", "--profile", profile_id]}}});
        copy_box(ui, &serde_json::to_string_pretty(&json).unwrap_or_default());
    });

    widgets::section(ui, "Servidores MCP externos (usados por el asistente)");
    card(ui, |ui| {
        let mut remove = None;
        let status = env.shared.mcp_clients.clone();
        let servers = &mut env.profile.settings.mcp_servers;
        if servers.is_empty() {
            dim(ui, "No hay servidores externos configurados.");
        }
        for (i, s) in servers.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let st_ = status.iter().find(|x| x.name == s.name);
                let (color, tip) = match st_ {
                    Some(x) if x.connected => {
                        (pal.ok, format!("Conectado · {} herramientas", x.tool_count))
                    }
                    Some(x) => (
                        pal.danger,
                        x.error.clone().unwrap_or_else(|| "Desconectado".into()),
                    ),
                    None => (pal.text_dim, "Sin conectar".to_string()),
                };
                let (r, resp) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, color);
                resp.on_hover_text(&tip);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&s.name).strong());
                    let detail = match &s.transport {
                        McpTransportConfig::Stdio { command, args, .. } => {
                            format!("{command} {}", args.join(" "))
                        }
                        McpTransportConfig::Http { url, .. } => url.clone(),
                    };
                    ui.label(
                        RichText::new(detail)
                            .small()
                            .monospace()
                            .color(pal.text_dim),
                    );
                    match st_ {
                        Some(x) if x.connected => ui.label(
                            RichText::new(format!("{} herramientas", x.tool_count))
                                .small()
                                .color(pal.ok),
                        ),
                        Some(x) => ui.label(
                            RichText::new(x.error.clone().unwrap_or_else(|| "Error".into()))
                                .small()
                                .color(pal.danger),
                        ),
                        None => ui.label(""),
                    };
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icon::TRASH, "Eliminar", true).clicked() {
                        remove = Some(i);
                    }
                    if switch(ui, &mut s.enabled) {
                        ch = true;
                        reconnect = true;
                    }
                });
            });
            sep(ui);
        }
        if let Some(i) = remove {
            servers.remove(i);
            ch = true;
            reconnect = true;
        }
        if widgets::subtle_button(ui, &format!("{}  Reconectar", icon::ARROW_CLOCKWISE)).clicked() {
            reconnect = true;
        }
    });

    widgets::section(ui, "Añadir servidor");
    card(ui, |ui| {
        let f = &mut st.mcp_form;
        segmented(
            ui,
            &mut f.http,
            &[(false, "Local (stdio)"), (true, "Remoto (HTTP)")],
        );
        ui.add(
            egui::TextEdit::singleline(&mut f.name)
                .hint_text("Nombre")
                .desired_width(f32::INFINITY),
        );
        if f.http {
            ui.add(
                egui::TextEdit::singleline(&mut f.url)
                    .hint_text("https://servidor/mcp")
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::multiline(&mut f.headers)
                    .hint_text("Cabeceras, una por línea: Authorization: Bearer …")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut f.command)
                    .hint_text("Comando (npx, uvx, /ruta/al/binario)")
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut f.args)
                    .hint_text(
                        "Argumentos: -y @modelcontextprotocol/server-filesystem \"/mi carpeta\"",
                    )
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::multiline(&mut f.env)
                    .hint_text("Variables de entorno, una por línea: CLAVE=valor")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
        }
        let valid = !f.name.trim().is_empty()
            && if f.http {
                !f.url.trim().is_empty()
            } else {
                !f.command.trim().is_empty()
            };
        if ui
            .add_enabled(
                valid,
                egui::Button::new(format!("{}  Añadir servidor", icon::PLUS)).corner_radius(8),
            )
            .clicked()
        {
            let transport = if f.http {
                McpTransportConfig::Http {
                    url: f.url.trim().to_string(),
                    headers: parse_lines(&f.headers, ':'),
                }
            } else {
                McpTransportConfig::Stdio {
                    command: f.command.trim().to_string(),
                    args: split_args(&f.args),
                    env: parse_lines(&f.env, '='),
                }
            };
            upsert_mcp(
                &mut env.profile.settings.mcp_servers,
                McpServerConfig {
                    name: f.name.trim().to_string(),
                    enabled: true,
                    transport,
                },
            );
            *f = McpForm::default();
            ch = true;
            reconnect = true;
        }
    });

    widgets::section(ui, "Importar desde JSON");
    card(ui, |ui| {
        dim(
            ui,
            "Acepta el formato {\"mcpServers\": {\"nombre\": {\"command\": …, \"args\": […]}}} de Claude Desktop, Cursor, etc.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut st.mcp_import)
                .code_editor()
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        );
        if ui
            .add_enabled(
                !st.mcp_import.trim().is_empty(),
                egui::Button::new("Importar").corner_radius(8),
            )
            .clicked()
        {
            match import_mcp_json(&st.mcp_import) {
                Ok(list) => {
                    let n = list.len();
                    for c in list {
                        upsert_mcp(&mut env.profile.settings.mcp_servers, c);
                    }
                    st.mcp_import.clear();
                    env.shared
                        .toast(&format!("{n} servidor(es) MCP importados"));
                    ch = true;
                    reconnect = true;
                }
                Err(e) => env.shared.toast(&e),
            }
        }
    });

    if restart {
        env.profile.mark_settings_dirty();
        env.profile.flush_settings();
        cmds.push(Cmd::App(AppCmd::RestartMcpServer));
    }
    if reconnect {
        env.profile.mark_settings_dirty();
        env.profile.flush_settings();
        cmds.push(Cmd::App(AppCmd::ReconnectMcpClients));
    }
    ch
}

fn extensions(ui: &mut Ui, st: &mut SettingsState, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let pal = Palette::get(ui.ctx());
    let mut changed = false;
    let mut toggle_ext: Option<(String, bool)> = None;
    let mut remove_ext: Option<String> = None;

    widgets::section(ui, "Instaladas");
    ui.horizontal(|ui| {
        if widgets::subtle_button(
            ui,
            &format!("{}  Cargar desempaquetada…", icon::FOLDER_OPEN),
        )
        .clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            match env.profile.extensions.install_unpacked(&dir) {
                Ok(_) => {
                    env.shared.toast("Extensión cargada");
                    changed = true;
                }
                Err(e) => env.shared.toast(&format!("No se pudo cargar: {e}")),
            }
        }
        if widgets::subtle_button(
            ui,
            &format!("{}  Instalar desde archivo…", icon::UPLOAD_SIMPLE),
        )
        .clicked()
            && let Some(file) = rfd::FileDialog::new()
                .add_filter("Extensión", &["zip", "xpi", "crx"])
                .pick_file()
        {
            // Installed by the app (shared path with store installs, panic-safe).
            cmds.push(Cmd::App(AppCmd::InstallExtensionFile(file)));
        }
        if widgets::subtle_button(ui, &format!("{}  Recargar", icon::ARROW_CLOCKWISE)).clicked() {
            match env.profile.extensions.reload() {
                Ok(()) => {
                    env.shared.toast("Extensiones recargadas");
                    changed = true;
                }
                Err(e) => env.shared.toast(&format!("Error al recargar: {e}")),
            }
        }
    });

    let list = env.profile.extensions.list().to_vec();
    card(ui, |ui| {
        if list.is_empty() {
            dim(ui, "No hay extensiones instaladas.");
        }
        for (i, ext) in list.iter().enumerate() {
            if i > 0 {
                sep(ui);
            }
            let tex = st
                .ext_icons
                .entry(ext.id.clone())
                .or_insert_with(|| {
                    let path = ext.icon_path(48)?;
                    let bytes = std::fs::read(path).ok()?;
                    load_png_texture(ui.ctx(), &format!("ext-{}", ext.id), &bytes)
                })
                .clone();
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                match &tex {
                    Some(t) => {
                        ui.painter().image(
                            t.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        ui.painter().rect_filled(r, 8.0, pal.field);
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            icon::PUZZLE_PIECE,
                            egui::FontId::proportional(20.0),
                            pal.text_dim,
                        );
                    }
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&ext.manifest.name).strong());
                        ui.label(
                            RichText::new(format!("v{}", ext.manifest.version))
                                .small()
                                .color(pal.text_dim),
                        );
                    });
                    if let Some(d) = ext
                        .manifest
                        .description
                        .as_deref()
                        .filter(|d| !d.is_empty())
                    {
                        ui.add(
                            egui::Label::new(RichText::new(d).small().color(pal.text_dim)).wrap(),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if st.confirm_remove_ext.as_deref() == Some(ext.id.as_str()) {
                        if widgets::accent_button(ui, "Eliminar").clicked() {
                            remove_ext = Some(ext.id.clone());
                        }
                        if widgets::subtle_button(ui, "Cancelar").clicked() {
                            st.confirm_remove_ext = None;
                        }
                    } else {
                        if widgets::icon_button(ui, icon::TRASH, "Quitar", true).clicked() {
                            st.confirm_remove_ext = Some(ext.id.clone());
                        }
                        let mut on = ext.enabled;
                        if switch(ui, &mut on) {
                            toggle_ext = Some((ext.id.clone(), on));
                        }
                    }
                });
            });
        }
    });

    if let Some((id, on)) = toggle_ext {
        match env.profile.extensions.set_enabled(&id, on) {
            Ok(()) => changed = true,
            Err(e) => env.shared.toast(&format!("Error: {e}")),
        }
    }
    if let Some(id) = remove_ext {
        st.confirm_remove_ext = None;
        st.ext_icons.remove(&id);
        match env.profile.extensions.uninstall(&id) {
            Ok(()) => {
                env.shared.toast("Extensión eliminada");
                changed = true;
            }
            Err(e) => env.shared.toast(&format!("No se pudo eliminar: {e}")),
        }
    }
    if changed {
        st.ext_icons
            .retain(|id, _| env.profile.extensions.get(id).is_some());
        cmds.push(Cmd::App(AppCmd::ExtensionsChanged));
    }

    widgets::section(ui, "Compatibilidad");
    card(ui, |ui| {
        ui.label("FerroSurf ejecuta extensiones WebExtension (Chrome / Firefox) con un subconjunto de funciones:");
        for line in [
            "Content scripts (JS y CSS) con sus patrones de coincidencia",
            "chrome.storage.local",
            "Reglas declarativeNetRequest (bloqueo y redirección)",
            "Página de nueva pestaña personalizada (chrome_url_overrides)",
            "Servidores MCP declarados en «ferrosurf.mcp_servers» del manifiesto",
        ] {
            ui.label(RichText::new(format!("{}  {line}", icon::CHECK)).color(pal.text_dim));
        }
    });
    false
}

fn profiles(ui: &mut Ui, st: &mut SettingsState, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let pal = Palette::get(ui.ctx());
    let current = env.profile.profile.id.clone();
    let list = env.profile.profiles.borrow().profiles().to_vec();
    let default_id = env.profile.profiles.borrow().default_profile().id.clone();
    let mut ask = env.profile.profiles.borrow().ask_on_startup();
    let mut error: Option<String> = None;
    let mut do_update = None;
    let mut do_delete: Option<String> = None;
    let mut do_default: Option<String> = None;

    widgets::section(ui, "Tus perfiles");
    card(ui, |ui| {
        for (i, p) in list.iter().enumerate() {
            if i > 0 {
                sep(ui);
            }
            let color = parse_hex(&p.color).unwrap_or(pal.accent);
            let editing = st.editing_profile.as_ref().is_some_and(|e| e.0 == p.id);
            ui.horizontal(|ui| {
                widgets::letter_avatar(ui, &p.initial(), color, 36.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.name).strong());
                        if p.id == current {
                            ui.label(
                                RichText::new("En uso")
                                    .small()
                                    .color(pal.on_accent())
                                    .background_color(pal.accent),
                            );
                        }
                        if p.id == default_id {
                            ui.label(RichText::new("Predeterminado").small().color(pal.text_dim));
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if st.confirm_delete_profile.as_deref() == Some(p.id.as_str()) {
                        if widgets::accent_button(ui, "Eliminar").clicked() {
                            do_delete = Some(p.id.clone());
                        }
                        if widgets::subtle_button(ui, "Cancelar").clicked() {
                            st.confirm_delete_profile = None;
                        }
                        ui.label("¿Eliminar con todos sus datos?");
                        return;
                    }
                    if p.id != current
                        && widgets::icon_button(ui, icon::TRASH, "Eliminar perfil", true).clicked()
                    {
                        st.confirm_delete_profile = Some(p.id.clone());
                    }
                    if widgets::icon_button(ui, icon::PENCIL_SIMPLE, "Editar", true).clicked() {
                        st.editing_profile = if editing {
                            None
                        } else {
                            Some((p.id.clone(), p.name.clone(), p.color.clone()))
                        };
                    }
                    if p.id != default_id && widgets::subtle_button(ui, "Predeterminado").clicked()
                    {
                        do_default = Some(p.id.clone());
                    }
                    if p.id != current && widgets::subtle_button(ui, "Abrir").clicked() {
                        cmds.push(Cmd::App(AppCmd::OpenProfile(p.id.clone())));
                    }
                });
            });
            if editing && let Some((_, name, col)) = st.editing_profile.as_mut() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
                    for hex in PROFILE_COLORS {
                        let c = parse_hex(hex).unwrap_or(Color32::GRAY);
                        if swatch(ui, c, col.eq_ignore_ascii_case(hex), None, hex).clicked() {
                            *col = hex.to_string();
                        }
                    }
                });
                if widgets::accent_button(ui, "Guardar").clicked() && !name.trim().is_empty() {
                    let mut np = p.clone();
                    np.name = name.trim().to_string();
                    np.color = col.clone();
                    do_update = Some(np);
                }
            }
        }
    });

    widgets::section(ui, "Crear perfil");
    card(ui, |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.new_profile_name)
                .hint_text("Nombre del perfil")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            for hex in PROFILE_COLORS {
                let c = parse_hex(hex).unwrap_or(Color32::GRAY);
                let sel = st.new_profile_color.eq_ignore_ascii_case(hex);
                if swatch(ui, c, sel, None, hex).clicked() {
                    st.new_profile_color = hex.to_string();
                }
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !st.new_profile_name.trim().is_empty(),
                    egui::Button::new(format!("{}  Crear perfil", icon::PLUS)).corner_radius(8),
                )
                .clicked()
            {
                let color =
                    (!st.new_profile_color.is_empty()).then(|| st.new_profile_color.clone());
                let res = env
                    .profile
                    .profiles
                    .borrow_mut()
                    .create(st.new_profile_name.trim(), color.as_deref());
                match res {
                    Ok(p) => {
                        st.new_profile_name.clear();
                        st.created_profile = Some(p.id);
                    }
                    Err(e) => error = Some(format!("No se pudo crear el perfil: {e}")),
                }
            }
            if let Some(id) = st.created_profile.clone() {
                ui.label(RichText::new("Perfil creado").color(pal.ok));
                if widgets::subtle_button(ui, "Abrir").clicked() {
                    cmds.push(Cmd::App(AppCmd::OpenProfile(id)));
                    st.created_profile = None;
                }
            }
        });
    });

    widgets::section(ui, "Opciones");
    card(ui, |ui| {
        if toggle(ui, &mut ask, "Preguntar qué perfil usar al iniciar", "")
            && let Err(e) = env.profile.profiles.borrow_mut().set_ask_on_startup(ask)
        {
            error = Some(format!("Error: {e}"));
        }
        sep(ui);
        row(
            ui,
            "Navegación como invitado",
            "No guarda nada al cerrar la ventana",
            |ui| {
                if widgets::subtle_button(ui, "Abrir ventana de invitado").clicked() {
                    cmds.push(Cmd::App(AppCmd::NewGuestWindow));
                }
            },
        );
    });

    if let Some(np) = do_update {
        let res = env.profile.profiles.borrow_mut().update(np.clone());
        match res {
            Ok(()) => {
                if np.id == current {
                    env.profile.profile = np;
                    cmds.push(Cmd::App(AppCmd::SettingsChanged));
                }
                st.editing_profile = None;
            }
            Err(e) => error = Some(format!("Error: {e}")),
        }
    }
    if let Some(id) = do_delete {
        st.confirm_delete_profile = None;
        let res = env.profile.profiles.borrow_mut().delete(&id);
        if let Err(e) = res {
            error = Some(format!("No se pudo eliminar: {e}"));
        }
    }
    if let Some(id) = do_default {
        let res = env.profile.profiles.borrow_mut().set_default(&id);
        if let Err(e) = res {
            error = Some(format!("Error: {e}"));
        }
    }
    if let Some(e) = error {
        env.shared.toast(&e);
    }
    false
}

fn accel_string(key: egui::Key, m: egui::Modifiers) -> Option<String> {
    use egui::Key;
    let k: String = match key {
        Key::ArrowLeft => "left".into(),
        Key::ArrowRight => "right".into(),
        Key::ArrowUp => "up".into(),
        Key::ArrowDown => "down".into(),
        Key::Equals | Key::Plus => "+".into(),
        Key::Minus => "-".into(),
        Key::Comma => ",".into(),
        Key::Period => ".".into(),
        Key::Slash => "/".into(),
        Key::Space => return None,
        other => other.name().to_lowercase(),
    };
    let mut parts = Vec::new();
    if m.ctrl || m.command {
        parts.push("Ctrl".to_string());
    }
    if m.shift {
        parts.push("Shift".into());
    }
    if m.alt {
        parts.push("Alt".into());
    }
    let mut kk = k.clone();
    if let Some(f) = kk.get(..1) {
        kk = f.to_uppercase() + &kk[1..];
    }
    parts.push(kk);
    Some(parts.join("+"))
}

fn shortcuts(ui: &mut Ui, st: &mut SettingsState, env: &mut Env) -> bool {
    let pal = Palette::get(ui.ctx());
    let mut ch = false;
    // Capture the next key press.
    let mut captured: Option<Option<String>> = None;
    if st.capturing.is_some() {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                captured = Some(if key == egui::Key::Escape {
                    None
                } else {
                    accel_string(key, modifiers)
                });
                if captured
                    .as_ref()
                    .is_some_and(|c| c.is_some() || key == egui::Key::Escape)
                {
                    break;
                }
            }
        }
    }
    if let (Some(id), Some(res)) = (st.capturing.clone(), captured) {
        st.capturing = None;
        if let Some(accel) = res {
            env.profile.settings.keybindings.insert(id, accel);
            ch = true;
        }
    }

    widgets::section(ui, "Atajos");
    card(ui, |ui| {
        dim(
            ui,
            "Pulsa «Cambiar» y luego la combinación deseada. Escape cancela.",
        );
        let mut reset: Option<String> = None;
        let mut start: Option<String> = None;
        for action in Action::all() {
            let id = action.id();
            let customized = env.profile.settings.keybindings.contains_key(&id);
            ui.horizontal(|ui| {
                ui.label(action.label());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if customized
                        && widgets::icon_button(ui, icon::ARROW_CLOCKWISE, "Restablecer", true)
                            .clicked()
                    {
                        reset = Some(id.clone());
                    }
                    let capturing = st.capturing.as_deref() == Some(id.as_str());
                    if widgets::subtle_button(
                        ui,
                        if capturing {
                            "Pulsa una tecla…"
                        } else {
                            "Cambiar"
                        },
                    )
                    .clicked()
                    {
                        start = Some(id.clone());
                    }
                    let text = env
                        .keymap
                        .shortcut_for(action)
                        .unwrap_or_else(|| "—".into());
                    let color = if customized { pal.accent } else { pal.text_dim };
                    egui::Frame::new()
                        .fill(pal.field)
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).monospace().color(color));
                        });
                });
            });
        }
        if let Some(id) = start {
            st.capturing = Some(id);
        }
        if let Some(id) = reset {
            env.profile.settings.keybindings.remove(&id);
            ch = true;
        }
    });
    if ch {
        env.shared.keymap_revision += 1;
    }
    ch
}

fn import_page(ui: &mut Ui, st: &mut SettingsState, env: &mut Env) -> bool {
    let pal = Palette::get(ui.ctx());
    let browsers = st
        .browsers
        .get_or_insert_with(import::detect_browsers)
        .clone();
    let mut rescan = false;

    widgets::section(ui, "Desde otros navegadores");
    card(ui, |ui| {
        if browsers.is_empty() {
            dim(ui, "No se ha encontrado ningún navegador compatible.");
        }
        for (i, b) in browsers.iter().enumerate() {
            if i > 0 {
                sep(ui);
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&b.name).strong());
                    ui.label(
                        RichText::new(b.profile_dir.display().to_string())
                            .small()
                            .color(pal.text_dim),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::subtle_button(ui, "Importar historial").clicked() {
                        match import::import_history(b, &env.profile.history, 5000) {
                            Ok(n) => env
                                .shared
                                .toast(&format!("{n} entradas de historial importadas")),
                            Err(e) => env
                                .shared
                                .toast(&format!("No se pudo importar el historial: {e}")),
                        }
                    }
                    if widgets::subtle_button(ui, "Importar marcadores").clicked() {
                        match import::read_bookmarks(b) {
                            Ok(entries) => match env
                                .profile
                                .bookmarks
                                .import_entries(&format!("Importado de {}", b.name), &entries)
                            {
                                Ok(n) => env.shared.toast(&format!("{n} marcadores importados")),
                                Err(e) => env
                                    .shared
                                    .toast(&format!("No se pudieron guardar los marcadores: {e}")),
                            },
                            Err(e) => env
                                .shared
                                .toast(&format!("No se pudieron leer los marcadores: {e}")),
                        }
                    }
                });
            });
        }
        sep(ui);
        if widgets::subtle_button(ui, &format!("{}  Volver a buscar", icon::ARROW_CLOCKWISE))
            .clicked()
        {
            rescan = true;
        }
    });
    if rescan {
        st.browsers = None;
    }

    widgets::section(ui, "Archivos de marcadores");
    card(ui, |ui| {
        ui.horizontal(|ui| {
            if widgets::subtle_button(
                ui,
                &format!("{}  Importar archivo HTML…", icon::UPLOAD_SIMPLE),
            )
            .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Marcadores HTML", &["html", "htm"])
                    .pick_file()
            {
                match std::fs::read_to_string(&path) {
                    Ok(text) => match env.profile.bookmarks.import_html(&text, "Importado") {
                        Ok(n) => env.shared.toast(&format!("{n} marcadores importados")),
                        Err(e) => env.shared.toast(&format!("Archivo no válido: {e}")),
                    },
                    Err(e) => env
                        .shared
                        .toast(&format!("No se pudo leer el archivo: {e}")),
                }
            }
            if widgets::subtle_button(
                ui,
                &format!("{}  Exportar marcadores…", icon::DOWNLOAD_SIMPLE),
            )
            .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("marcadores.html")
                    .save_file()
            {
                match std::fs::write(&path, env.profile.bookmarks.export_html()) {
                    Ok(()) => env.shared.toast("Marcadores exportados"),
                    Err(e) => env.shared.toast(&format!("No se pudo exportar: {e}")),
                }
            }
        });
    });
    false
}

fn advanced(ui: &mut Ui, env: &mut Env) -> bool {
    let mut ch = false;
    let root = env.profile.paths.root.display().to_string();
    let s = &mut env.profile.settings;
    widgets::section(ui, "Motor");
    card(ui, |ui| {
        ch |= text_row(
            ui,
            "User agent personalizado",
            "Vacío = el predeterminado de Servo",
            &mut s.user_agent,
            "Mozilla/5.0 …",
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut s.enable_devtools,
            "Activar herramientas de desarrollo",
            "Requiere reiniciar FerroSurf",
        );
        if s.enable_devtools {
            sep(ui);
            row(ui, "Puerto de depuración", "Requiere reiniciar", |ui| {
                ch |= ui
                    .add(egui::DragValue::new(&mut s.devtools_port).range(1024..=65535))
                    .changed();
            });
        }
    });
    widgets::section(ui, "Pestañas");
    card(ui, |ui| {
        row(
            ui,
            "Suspender pestañas en segundo plano",
            "Minutos de inactividad; 0 = nunca",
            |ui| {
                ch |= ui
                    .add(
                        egui::DragValue::new(&mut s.tab_sleep_minutes)
                            .range(0..=1440)
                            .suffix(" min"),
                    )
                    .changed();
            },
        );
        sep(ui);
        ch |= toggle(
            ui,
            &mut s.confirm_close_many_tabs,
            "Confirmar al cerrar varias pestañas",
            "",
        );
    });
    widgets::section(ui, "Datos");
    card(ui, |ui| {
        row(ui, "Carpeta del perfil", &root, |ui| {
            if widgets::subtle_button(
                ui,
                &format!("{}  Abrir carpeta del perfil", icon::FOLDER_OPEN),
            )
            .clicked()
            {
                crate::app::open_external(&root);
            }
        });
    });
    ch
}

fn about(ui: &mut Ui, st: &mut SettingsState, env: &mut Env, cmds: &mut Vec<Cmd>) -> bool {
    let pal = Palette::get(ui.ctx());
    if st.logo.is_none() {
        st.logo = load_png_texture(
            ui.ctx(),
            "ferrosurf-logo",
            include_bytes!("../../resources/icons/ferrosurf-128.png"),
        );
    }
    card(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            if let Some(t) = &st.logo {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    t.id(),
                    vec2(88.0, 88.0),
                )));
            }
            ui.label(
                RichText::new(format!("FerroSurf {}", env!("CARGO_PKG_VERSION")))
                    .size(22.0)
                    .strong(),
            );
            ui.label(RichText::new("Motor: Servo 0.8").color(pal.text_dim));
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Datos: {}", env.profile.paths.root.display()))
                    .small()
                    .monospace()
                    .color(pal.text_dim),
            );
            ui.add_space(8.0);
            if widgets::accent_button(ui, "Abrir ferro://about").clicked()
                && let Ok(url) = url::Url::parse("ferro://about")
            {
                cmds.push(Cmd::OpenUrl {
                    url,
                    disposition: Disposition::NewForegroundTab,
                });
            }
            ui.add_space(8.0);
        });
    });
    false
}
