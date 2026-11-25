//! Right sidebar: an icon rail with web panels (quick-access pages like Vivaldi/Edge) and
//! the open panel, which renders its own webview next to the page.

use std::sync::Arc;

use egui::{Rect, RichText};
use egui_phosphor::regular as icon;
use ferrosurf_core::settings::WebPanel;

use super::{Palette, widgets};
use crate::window::{BrowserWindow, Cmd, Env};

const RAIL_WIDTH: f32 = 44.0;

/// The narrow icon rail on the right edge.
pub fn rail(root: &mut egui::Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(root.ctx());
    egui::Panel::right("sidebar_rail")
        .exact_size(RAIL_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(egui::Margin::symmetric(6, 8)),
        )
        .show_separator_line(true)
        .show(root, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let open = w.sidebar.as_ref().map(|s| s.panel_id.clone());
                let panels = env.profile.settings.sidebar.panels.clone();
                let mut remove: Option<String> = None;
                for p in &panels {
                    let active = open.as_deref() == Some(p.id.as_str());
                    let resp = panel_button(ui, env, p, active, &pal);
                    if resp.clicked() {
                        cmds.push(Cmd::ToggleSidebarPanel(p.id.clone()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Abrir en pestaña nueva").clicked() {
                            if let Ok(url) = url::Url::parse(&p.url) {
                                cmds.push(Cmd::OpenUrl {
                                    url,
                                    disposition: crate::window::Disposition::NewForegroundTab,
                                });
                            }
                            ui.close();
                        }
                        if ui.button("Quitar del panel lateral").clicked() {
                            remove = Some(p.id.clone());
                            ui.close();
                        }
                    });
                }
                if widgets::icon_button(ui, icon::PLUS, "Añadir la página actual", true).clicked()
                    && let Some(tab) = w.active_tab()
                    && let Some(url) = tab
                        .url
                        .as_ref()
                        .filter(|u| matches!(u.scheme(), "http" | "https"))
                {
                    let panel = WebPanel::new(&tab.display_title(), url.as_str());
                    env.profile.settings.sidebar.panels.push(panel);
                    env.profile.mark_settings_dirty();
                    env.shared.toast("Página añadida al panel lateral");
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    if widgets::icon_button(ui, icon::GEAR_SIX, "Configurar panel lateral", true)
                        .clicked()
                    {
                        w.chrome.open_settings(super::SettingsPage::Sidebar);
                    }
                });
                if let Some(id) = remove {
                    if open.as_deref() == Some(id.as_str()) {
                        cmds.push(Cmd::CloseSidebar);
                    }
                    env.profile.settings.sidebar.panels.retain(|p| p.id != id);
                    env.profile.mark_settings_dirty();
                }
            });
        });
}

fn panel_button(
    ui: &mut egui::Ui,
    env: &Env,
    p: &WebPanel,
    active: bool,
    pal: &Palette,
) -> egui::Response {
    let size = egui::vec2(32.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hover = widgets::fade(ui.ctx(), resp.id, resp.hovered() || active, 0.12);
    if hover > 0.0 {
        ui.painter().rect_filled(
            rect,
            8.0,
            if active {
                pal.accent.gamma_multiply(0.25)
            } else {
                pal.tab_hover.gamma_multiply(hover)
            },
        );
    }
    if active {
        let bar = Rect::from_min_size(
            egui::pos2(rect.left() - 5.0, rect.top() + 6.0),
            egui::vec2(3.0, rect.height() - 12.0),
        );
        ui.painter().rect_filled(bar, 2.0, pal.accent);
    }
    match crate::favicons::texture(ui.ctx(), &env.profile.favicons, &p.url) {
        Some(tex) => {
            ui.painter().image(
                tex.id(),
                Rect::from_center_size(rect.center(), egui::vec2(20.0, 20.0)),
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            let letter = p
                .title
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                egui::FontId::proportional(15.0),
                pal.text,
            );
        }
    }
    resp.on_hover_text(&p.title)
}

/// The open web panel (between the rail and the page).
pub fn panel(root: &mut egui::Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(root.ctx());
    let Some(panel_id) = w.sidebar.as_ref().map(|s| s.panel_id.clone()) else {
        return;
    };
    let title = env
        .profile
        .settings
        .sidebar
        .panels
        .iter()
        .find(|p| p.id == panel_id)
        .map(|p| p.title.clone())
        .unwrap_or_default();
    let resp = egui::Panel::right("sidebar_panel")
        .resizable(true)
        .default_size(env.profile.settings.sidebar.width)
        .size_range(260.0..=900.0)
        .frame(egui::Frame::new().fill(pal.toolbar))
        .show(root, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&title).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(ui, icon::X, "Cerrar", true).clicked() {
                                cmds.push(Cmd::CloseSidebar);
                            }
                            if widgets::icon_button(
                                ui,
                                icon::ARROW_SQUARE_OUT,
                                "Abrir en una pestaña",
                                true,
                            )
                            .clicked()
                            {
                                cmds.push(Cmd::SidebarToTab);
                            }
                            if widgets::icon_button(ui, icon::ARROW_CLOCKWISE, "Recargar", true)
                                .clicked()
                            {
                                cmds.push(Cmd::SidebarReload);
                            }
                        });
                    });
                });
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, egui::Sense::hover());
            w.set_sidebar_rect(rect);
            if let Some(sb) = &w.sidebar
                && let Some(render) = sb.offscreen.render_to_parent_callback()
            {
                let render: Arc<super::BlitToParent> = Arc::from(render);
                let cb = egui_glow::CallbackFn::new(move |info, painter| {
                    let vp = info.viewport_in_pixels();
                    let target = euclid::default::Rect::new(
                        euclid::default::Point2D::new(vp.left_px, vp.from_bottom_px),
                        euclid::default::Size2D::new(vp.width_px, vp.height_px),
                    );
                    render(painter.gl(), target);
                });
                ui.painter().add(egui::PaintCallback {
                    rect,
                    callback: Arc::new(cb),
                });
            }
        });
    let width = resp.response.rect.width();
    if (width - env.profile.settings.sidebar.width).abs() > 4.0
        && !root.ctx().input(|i| i.pointer.any_down())
    {
        env.profile.settings.sidebar.width = width;
        env.profile.mark_settings_dirty();
    }
}

/// Settings page: "Panel lateral".
pub fn settings_page(ui: &mut egui::Ui, env: &mut Env) -> bool {
    let pal = Palette::get(ui.ctx());
    let mut changed = false;
    let sb = &mut env.profile.settings.sidebar;
    widgets::section(ui, "Panel lateral");
    changed |= ui
        .checkbox(&mut sb.enabled, "Mostrar la barra de paneles a la derecha")
        .changed();
    ui.label(
        RichText::new(
            "Fija páginas de uso rápido (traductor, chat, notas, música…) y ábrelas junto a la página actual.",
        )
        .color(pal.text_dim)
        .small(),
    );
    ui.add_space(10.0);
    widgets::section(ui, "Paneles web");
    let mut remove = None;
    let mut swap = None;
    let n = sb.panels.len();
    for (i, p) in sb.panels.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui
                .add(egui::TextEdit::singleline(&mut p.title).desired_width(140.0))
                .changed();
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut p.url)
                        .desired_width((ui.available_width() - 110.0).max(120.0)),
                )
                .changed();
            if ui
                .add_enabled(i > 0, egui::Button::new(icon::ARROW_UP))
                .clicked()
            {
                swap = Some((i, i - 1));
            }
            if ui
                .add_enabled(i + 1 < n, egui::Button::new(icon::ARROW_DOWN))
                .clicked()
            {
                swap = Some((i, i + 1));
            }
            if ui.button(icon::TRASH).on_hover_text("Quitar").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some((a, b)) = swap {
        sb.panels.swap(a, b);
        changed = true;
    }
    if let Some(i) = remove {
        sb.panels.remove(i);
        changed = true;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if widgets::subtle_button(ui, &format!("{}  Añadir panel", icon::PLUS)).clicked() {
            sb.panels.push(WebPanel::new("Nuevo panel", "https://"));
            changed = true;
        }
        for (name, url) in [
            ("Claude", "https://claude.ai/"),
            ("ChatGPT", "https://chatgpt.com/"),
            ("WhatsApp", "https://web.whatsapp.com/"),
            ("Spotify", "https://open.spotify.com/"),
        ] {
            if !sb.panels.iter().any(|p| p.url == url)
                && widgets::subtle_button(ui, &format!("+ {name}")).clicked()
            {
                sb.panels.push(WebPanel::new(name, url));
                changed = true;
            }
        }
    });

    ui.add_space(14.0);
    widgets::section(ui, "Atajo del asistente (Ctrl+J)");
    ui.label(
        RichText::new(
            "Ctrl+J puede abrir un panel web (por ejemplo claude.ai) en lugar del asistente integrado.",
        )
        .color(pal.text_dim)
        .small(),
    );
    let current = sb
        .assistant_panel
        .as_ref()
        .and_then(|id| sb.panels.iter().find(|p| &p.id == id))
        .map(|p| p.title.clone())
        .unwrap_or_else(|| "Asistente IA integrado".into());
    egui::ComboBox::from_id_salt("assistant-panel")
        .selected_text(current)
        .show_ui(ui, |ui| {
            changed |= ui
                .selectable_value(&mut sb.assistant_panel, None, "Asistente IA integrado")
                .changed();
            for p in &sb.panels {
                changed |= ui
                    .selectable_value(&mut sb.assistant_panel, Some(p.id.clone()), &p.title)
                    .changed();
            }
        });
    if changed {
        env.profile.mark_settings_dirty();
    }
    changed
}
