//! Command palette (Ctrl+K): actions, tabs, bookmarks, history, settings pages, web search and
//! the assistant in one fuzzy list.

use egui::{
    Color32, FontId, Id, Key, Margin, Modifiers, Order, Pos2, Rect, RichText, Sense, Stroke, vec2,
};
use egui_phosphor::regular as icon;
use ferrosurf_core::HistoryEntry;
use ferrosurf_core::util::match_score;

use super::widgets;
use crate::actions::Action;
use crate::tab::TabId;
use crate::ui::SettingsPage;
use crate::ui::theme::Palette as Theme;
use crate::window::{AppCmd, BrowserWindow, Cmd, Disposition, Env};

const ROW_H: f32 = 38.0;
const HEADER_H: f32 = 26.0;

/// State of the command palette (one per window).
#[derive(Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    focus: bool,
    scroll_to_selected: bool,
    /// Cached history suggestions for `hist_key`.
    hist_key: Option<String>,
    hist: Vec<(u32, HistoryEntry)>,
}

impl Palette {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
        self.scroll_to_selected = false;
        self.hist_key = None;
        self.hist.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
        self.hist_key = None;
        self.hist.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn refresh_history(&mut self, env: &Env, q: &str) {
        if self.hist_key.as_deref() == Some(q) {
            return;
        }
        self.hist = if q.is_empty() {
            Vec::new()
        } else {
            env.profile.history.suggest(q, 5).unwrap_or_default()
        };
        self.hist_key = Some(q.to_string());
    }
}

enum Run {
    Action(Action),
    Tab(TabId),
    Url(String),
    Settings(SettingsPage),
    Assistant(String),
    Search(String),
    Go(String),
}

enum EntryIcon {
    Glyph(&'static str),
    Site {
        url: String,
        title: String,
        favicon: Option<egui::TextureHandle>,
    },
}

struct Entry {
    group: &'static str,
    icon: EntryIcon,
    title: String,
    sub: String,
    shortcut: Option<String>,
    run: Run,
    score: u32,
}

const SETTINGS_PAGES: &[(SettingsPage, &str, &str)] = &[
    (
        SettingsPage::Appearance,
        "Apariencia",
        "tema oscuro claro color acento fuente densidad pestañas",
    ),
    (
        SettingsPage::Startup,
        "Inicio",
        "página de inicio nueva pestaña arranque",
    ),
    (
        SettingsPage::Search,
        "Búsqueda",
        "buscador motor de búsqueda",
    ),
    (
        SettingsPage::Privacy,
        "Privacidad y seguridad",
        "rastreadores cookies permisos bloqueador historial",
    ),
    (SettingsPage::Downloads, "Descargas", "carpeta archivos"),
    (
        SettingsPage::Assistant,
        "Asistente",
        "ia inteligencia artificial modelo clave api",
    ),
    (
        SettingsPage::Mcp,
        "Servidor MCP",
        "mcp clientes herramientas",
    ),
    (
        SettingsPage::Extensions,
        "Extensiones",
        "complementos addons",
    ),
    (SettingsPage::Profiles, "Perfiles", "usuarios cuentas"),
    (
        SettingsPage::Shortcuts,
        "Atajos de teclado",
        "teclas combinaciones",
    ),
    (
        SettingsPage::Import,
        "Importar datos",
        "chrome firefox marcadores importar",
    ),
    (
        SettingsPage::Advanced,
        "Avanzado",
        "devtools agente de usuario zoom",
    ),
    (SettingsPage::About, "Acerca de", "versión información"),
];

fn looks_like_url(q: &str) -> bool {
    if q.is_empty() || q.contains(char::is_whitespace) {
        return false;
    }
    q.contains("://")
        || q.starts_with("ferro:")
        || q.starts_with("localhost")
        || (q.contains('.') && !q.ends_with('.') && !q.starts_with('.'))
}

fn host_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => match u.host_str() {
            Some(h) => h.trim_start_matches("www.").to_string(),
            None => ferrosurf_core::util::pretty_url(url),
        },
        Err(_) => ferrosurf_core::util::pretty_url(url),
    }
}

fn push_group(out: &mut Vec<Entry>, mut group: Vec<Entry>, limit: usize) {
    group.sort_by_key(|e| std::cmp::Reverse(e.score));
    group.truncate(limit);
    out.extend(group);
}

fn build_entries(p: &Palette, w: &BrowserWindow, env: &Env) -> Vec<Entry> {
    let q = p.query.trim();
    let mut out: Vec<Entry> = Vec::new();

    if looks_like_url(q) {
        out.push(Entry {
            group: "Navegar",
            icon: EntryIcon::Glyph(icon::ARROW_RIGHT),
            title: format!("Ir a {q}"),
            sub: String::new(),
            shortcut: None,
            run: Run::Go(q.to_string()),
            score: u32::MAX,
        });
    }

    let assistant_on = env.profile.settings.assistant.enabled;
    let mut actions = Vec::new();
    for a in Action::all() {
        if a == Action::CommandPalette || (!assistant_on && a == Action::ToggleAssistant) {
            continue;
        }
        let label = a.label();
        let score = if q.is_empty() {
            Some(0)
        } else {
            match_score(q, &label)
        };
        if let Some(score) = score {
            actions.push(Entry {
                group: "Acciones",
                icon: EntryIcon::Glyph(icon::LIGHTNING),
                title: label,
                sub: String::new(),
                shortcut: env.keymap.shortcut_for(a),
                run: Run::Action(a),
                score,
            });
        }
    }

    let mut tabs = Vec::new();
    for t in &w.tabs {
        let url = t.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
        let title = if t.title.is_empty() {
            ferrosurf_core::util::pretty_url(&url)
        } else {
            t.title.clone()
        };
        let title = if title.is_empty() {
            "Nueva pestaña".to_string()
        } else {
            title
        };
        let score = if q.is_empty() {
            Some(0)
        } else {
            match_score(q, &format!("{title} {url}"))
        };
        if let Some(score) = score {
            tabs.push(Entry {
                group: "Pestañas",
                sub: if url.is_empty() {
                    String::new()
                } else {
                    host_of(&url)
                },
                icon: EntryIcon::Site {
                    url,
                    title: title.clone(),
                    favicon: t.favicon.clone(),
                },
                title,
                shortcut: None,
                run: Run::Tab(t.id),
                score,
            });
        }
    }

    if q.is_empty() {
        push_group(&mut out, tabs, 6);
        actions.truncate(8);
        out.extend(actions);
        return out;
    }

    push_group(&mut out, actions, 6);
    push_group(&mut out, tabs, 5);

    let mut seen: Vec<String> = Vec::new();
    let mut bookmarks = Vec::new();
    for (score, b) in env.profile.bookmarks.search(q, 5) {
        seen.push(b.url.clone());
        let title = if b.title.is_empty() {
            b.url.clone()
        } else {
            b.title.clone()
        };
        bookmarks.push(Entry {
            group: "Marcadores",
            icon: EntryIcon::Site {
                url: b.url.clone(),
                title: title.clone(),
                favicon: None,
            },
            sub: host_of(&b.url),
            title,
            shortcut: None,
            run: Run::Url(b.url.clone()),
            score,
        });
    }
    push_group(&mut out, bookmarks, 5);

    let mut history = Vec::new();
    for (score, e) in &p.hist {
        if seen.contains(&e.url) {
            continue;
        }
        let title = if e.title.trim().is_empty() {
            ferrosurf_core::util::pretty_url(&e.url)
        } else {
            e.title.clone()
        };
        history.push(Entry {
            group: "Historial",
            icon: EntryIcon::Site {
                url: e.url.clone(),
                title: title.clone(),
                favicon: None,
            },
            sub: host_of(&e.url),
            title,
            shortcut: None,
            run: Run::Url(e.url.clone()),
            score: *score,
        });
    }
    // History suggestions arrive already ranked; keep their order.
    out.extend(history);

    let mut settings = Vec::new();
    for (page, name, kw) in SETTINGS_PAGES {
        if !assistant_on && *page == SettingsPage::Assistant {
            continue;
        }
        if let Some(score) = match_score(q, &format!("ajustes {name} {kw}")) {
            settings.push(Entry {
                group: "Ajustes",
                icon: EntryIcon::Glyph(icon::GEAR_SIX),
                title: format!("Ajustes: {name}"),
                sub: String::new(),
                shortcut: None,
                run: Run::Settings(*page),
                score,
            });
        }
    }
    push_group(&mut out, settings, 4);

    let engine = env.profile.settings.current_search_engine();
    out.push(Entry {
        group: "Buscar",
        icon: EntryIcon::Glyph(icon::MAGNIFYING_GLASS),
        title: format!("Buscar «{q}» en {}", engine.name),
        sub: String::new(),
        shortcut: None,
        run: Run::Search(q.to_string()),
        score: 0,
    });
    if assistant_on {
        out.push(Entry {
            group: "Buscar",
            icon: EntryIcon::Glyph(icon::SPARKLE),
            title: format!("Preguntar al asistente: «{q}»"),
            sub: String::new(),
            shortcut: None,
            run: Run::Assistant(q.to_string()),
            score: 0,
        });
    }
    out
}

fn run_entry(run: Run, new_tab: bool, w: &mut BrowserWindow, env: &Env, cmds: &mut Vec<Cmd>) {
    let disposition = if new_tab {
        Disposition::NewForegroundTab
    } else {
        Disposition::CurrentTab
    };
    match run {
        Run::Action(a) => cmds.push(Cmd::Action(a)),
        Run::Tab(id) => cmds.push(Cmd::ActivateTab(id)),
        Run::Url(u) => match url::Url::parse(&u) {
            Ok(url) => cmds.push(Cmd::OpenUrl { url, disposition }),
            Err(_) => cmds.push(Cmd::Navigate {
                input: u,
                disposition,
            }),
        },
        Run::Settings(page) => w.chrome.settings = Some(page),
        Run::Assistant(text) => {
            w.chrome.assistant_open = true;
            cmds.push(Cmd::App(AppCmd::StartAssistant { text }));
        }
        Run::Search(q) => match env.profile.settings.current_search_engine().search_url(&q) {
            Some(url) => cmds.push(Cmd::OpenUrl { url, disposition }),
            None => cmds.push(Cmd::Navigate {
                input: q,
                disposition,
            }),
        },
        Run::Go(input) => cmds.push(Cmd::Navigate { input, disposition }),
    }
}

/// Title with the matched query words highlighted, truncated to `width`.
fn highlighted(
    title: &str,
    q: &str,
    font: FontId,
    color: Color32,
    accent: Color32,
    width: f32,
) -> egui::text::LayoutJob {
    let lower = title.to_lowercase();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    if lower.len() == title.len() {
        for word in q.split_whitespace() {
            let word = word.to_lowercase();
            if let Some(pos) = lower.find(&word) {
                let end = pos + word.len();
                if title.is_char_boundary(pos) && title.is_char_boundary(end) {
                    ranges.push((pos, end));
                }
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut job = egui::text::LayoutJob::default();
    let mut cursor = 0;
    for (s, e) in merged {
        if s > cursor {
            job.append(
                &title[cursor..s],
                0.0,
                egui::TextFormat::simple(font.clone(), color),
            );
        }
        job.append(
            &title[s..e],
            0.0,
            egui::TextFormat::simple(font.clone(), accent),
        );
        cursor = e;
    }
    if cursor < title.len() {
        job.append(&title[cursor..], 0.0, egui::TextFormat::simple(font, color));
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(20.0));
    job
}

fn paint_entry_icon(painter: &egui::Painter, center: Pos2, ico: &EntryIcon, pal: &Theme) {
    match ico {
        EntryIcon::Glyph(g) => {
            let r = Rect::from_center_size(center, vec2(26.0, 26.0));
            painter.rect_filled(r, 7, pal.accent.gamma_multiply(0.16));
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                g,
                FontId::proportional(15.0),
                pal.accent,
            );
        }
        EntryIcon::Site {
            url,
            title,
            favicon,
        } => {
            let r = Rect::from_center_size(center, vec2(18.0, 18.0));
            widgets::paint_site_icon(painter, r, favicon.as_ref(), title, url);
        }
    }
}

pub fn show(ctx: &egui::Context, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let anim_id = Id::new("palette-anim");
    let layer = egui::LayerId::new(Order::Foreground, Id::new("palette-panel"));
    if !w.chrome.palette.is_open() {
        // Reset so the next opening animates from scratch.
        ctx.animate_bool_with_time(anim_id, false, 0.0);
        ctx.data_mut(|d| d.remove::<f32>(anim_id.with("sel")));
        if ctx
            .data(|d| d.get_temp::<bool>(anim_id.with("dirty")))
            .unwrap_or(false)
        {
            ctx.set_transform_layer(layer, egui::emath::TSTransform::IDENTITY);
            ctx.data_mut(|d| d.insert_temp(anim_id.with("dirty"), false));
        }
        return;
    }
    let appear = super::widgets::fade(ctx, anim_id, true, 0.18);
    let mut p = std::mem::take(&mut w.chrome.palette);
    let pal = Theme::get(ctx);
    let screen = ctx.content_rect();

    // Dimmed backdrop; clicking it closes the palette.
    let mut close = false;
    egui::Area::new(Id::new("palette-backdrop"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
            ui.painter().rect_filled(
                rect,
                0,
                Color32::from_black_alpha(((if pal.dark { 140.0 } else { 85.0 }) * appear) as u8),
            );
            if resp.clicked() || resp.drag_started() {
                close = true;
            }
        });

    let q = p.query.trim().to_string();
    p.refresh_history(env, &q);
    let entries = build_entries(&p, w, env);
    let n = entries.len();

    // Keyboard handling happens before the text field sees the events.
    let (down, up, enter, ctrl_enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            // Order matters: the exact-modifier variant first.
            false,
            i.consume_key(Modifiers::COMMAND, Key::Enter),
            i.key_pressed(Key::Escape),
        )
    });
    let enter = enter || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    if n > 0 {
        p.selected = p.selected.min(n - 1);
        if down {
            p.selected = (p.selected + 1) % n;
            p.scroll_to_selected = true;
        }
        if up {
            p.selected = (p.selected + n - 1) % n;
            p.scroll_to_selected = true;
        }
    }
    let mut run_idx: Option<(usize, bool)> = None;
    if n > 0 && (enter || ctrl_enter) {
        run_idx = Some((p.selected, ctrl_enter));
    }

    let width = 640.0_f32.min(screen.width() - 32.0).max(280.0);
    let top = (screen.height() * 0.12).clamp(24.0, 110.0);
    let pos = Pos2::new(screen.center().x - width / 2.0, screen.top() + top);
    let max_list_h = (screen.height() - top - 90.0).clamp(160.0, 430.0);

    if appear < 1.0 {
        let sc = 0.96 + 0.04 * appear;
        let pivot = Pos2::new(pos.x + width / 2.0, pos.y);
        ctx.set_transform_layer(
            layer,
            egui::emath::TSTransform::new(pivot.to_vec2() * (1.0 - sc), sc),
        );
        ctx.data_mut(|d| d.insert_temp(anim_id.with("dirty"), true));
    } else if ctx
        .data(|d| d.get_temp::<bool>(anim_id.with("dirty")))
        .unwrap_or(false)
    {
        ctx.set_transform_layer(layer, egui::emath::TSTransform::IDENTITY);
        ctx.data_mut(|d| d.insert_temp(anim_id.with("dirty"), false));
    }
    egui::Area::new(Id::new("palette-panel"))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            ui.set_opacity(appear);
            egui::Frame::new()
                .fill(pal.toolbar)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(14)
                .shadow(ctx.global_style().visuals.window_shadow)
                .show(ui, |ui| {
                    ui.set_width(width);
                    egui::Frame::new()
                        .inner_margin(Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(icon::MAGNIFYING_GLASS)
                                        .size(19.0)
                                        .color(pal.text_dim),
                                );
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut p.query)
                                        .frame(egui::Frame::NONE)
                                        .font(FontId::proportional(17.0))
                                        .hint_text("Escribe un comando, pestaña o dirección…")
                                        .desired_width(f32::INFINITY),
                                );
                                if p.focus || !r.has_focus() {
                                    r.request_focus();
                                    p.focus = false;
                                }
                                if r.changed() {
                                    p.selected = 0;
                                    p.scroll_to_selected = true;
                                }
                            });
                        });
                    let (sep, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                    ui.painter().rect_filled(sep, 0, pal.border);

                    egui::ScrollArea::vertical()
                        .max_height(max_list_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let content_top = ui.cursor().top();
                            ui.add_space(4.0);
                            let mut prev_group = "";
                            for (i, e) in entries.iter().enumerate() {
                                if e.group != prev_group {
                                    prev_group = e.group;
                                    let (r, _) = ui
                                        .allocate_exact_size(vec2(width, HEADER_H), Sense::hover());
                                    ui.painter().text(
                                        Pos2::new(r.left() + 18.0, r.bottom() - 6.0),
                                        egui::Align2::LEFT_BOTTOM,
                                        e.group.to_uppercase(),
                                        FontId::proportional(11.0),
                                        pal.text_dim,
                                    );
                                }
                                let (rect, resp) =
                                    ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                                let selected = i == p.selected;
                                if resp.hovered()
                                    && ui.input(|inp| inp.pointer.delta() != egui::Vec2::ZERO)
                                    && !selected
                                {
                                    p.selected = i;
                                }
                                if !ui.is_rect_visible(rect) {
                                    if resp.clicked() {
                                        run_idx = Some((i, ui.input(|inp| inp.modifiers.command)));
                                    }
                                    continue;
                                }
                                let inner = rect.shrink2(vec2(8.0, 1.0));
                                if selected {
                                    // The highlight glides to the selected row.
                                    let y = super::widgets::smooth(
                                        ctx,
                                        anim_id.with("sel"),
                                        inner.top() - content_top,
                                        None,
                                        26.0,
                                    );
                                    let hl = Rect::from_min_size(
                                        Pos2::new(inner.left(), content_top + y),
                                        inner.size(),
                                    );
                                    ui.painter().rect_filled(
                                        hl,
                                        8,
                                        pal.accent.gamma_multiply(if pal.dark {
                                            0.22
                                        } else {
                                            0.18
                                        }),
                                    );
                                    let bar = Rect::from_min_size(
                                        hl.left_top() + vec2(0.0, 8.0),
                                        vec2(3.0, hl.height() - 16.0),
                                    );
                                    ui.painter().rect_filled(bar, 2, pal.accent);
                                    if p.scroll_to_selected {
                                        ui.scroll_to_rect(rect, None);
                                        p.scroll_to_selected = false;
                                    }
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(inner, 8, pal.tab_hover);
                                }
                                paint_entry_icon(
                                    ui.painter(),
                                    Pos2::new(inner.left() + 24.0, rect.center().y),
                                    &e.icon,
                                    &pal,
                                );

                                let left = inner.left() + 46.0;
                                let mut right = inner.right() - 12.0;
                                if let Some(sc) = &e.shortcut {
                                    let g = ui.painter().layout_no_wrap(
                                        sc.clone(),
                                        FontId::proportional(11.5),
                                        pal.text_dim,
                                    );
                                    let chip = Rect::from_min_size(
                                        Pos2::new(
                                            right - g.size().x - 12.0,
                                            rect.center().y - (g.size().y + 6.0) / 2.0,
                                        ),
                                        g.size() + vec2(12.0, 6.0),
                                    );
                                    ui.painter().rect(
                                        chip,
                                        6,
                                        pal.field,
                                        Stroke::new(1.0, pal.border),
                                        egui::StrokeKind::Inside,
                                    );
                                    ui.painter()
                                        .galley(chip.min + vec2(6.0, 3.0), g, pal.text_dim);
                                    right = chip.left() - 10.0;
                                }
                                let avail = (right - left).max(20.0);
                                let title_w = if e.sub.is_empty() {
                                    avail
                                } else {
                                    avail * 0.68
                                };
                                let job = highlighted(
                                    &e.title,
                                    &q,
                                    FontId::proportional(14.0),
                                    pal.text,
                                    pal.accent,
                                    title_w,
                                );
                                let galley = ui.painter().layout_job(job);
                                let ty = rect.center().y - galley.size().y / 2.0;
                                let gw = galley.size().x;
                                ui.painter().galley(Pos2::new(left, ty), galley, pal.text);
                                if !e.sub.is_empty() {
                                    let sx = left + gw + 10.0;
                                    if right - sx > 24.0 {
                                        let mut job = egui::text::LayoutJob::simple_singleline(
                                            e.sub.clone(),
                                            FontId::proportional(12.0),
                                            pal.text_dim,
                                        );
                                        job.wrap =
                                            egui::text::TextWrapping::truncate_at_width(right - sx);
                                        let g = ui.painter().layout_job(job);
                                        ui.painter().galley(
                                            Pos2::new(sx, rect.center().y - g.size().y / 2.0 + 0.5),
                                            g,
                                            pal.text_dim,
                                        );
                                    }
                                }
                                if resp.clicked() {
                                    run_idx = Some((i, ui.input(|inp| inp.modifiers.command)));
                                }
                            }
                            ui.add_space(4.0);
                        });

                    let (sep, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                    ui.painter().rect_filled(sep, 0, pal.border);
                    egui::Frame::new()
                        .inner_margin(Margin::symmetric(16, 7))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 14.0;
                                let hint = |ui: &mut egui::Ui, text: String| {
                                    ui.label(RichText::new(text).small().color(pal.text_dim));
                                };
                                hint(
                                    ui,
                                    format!("{}{}  navegar", icon::ARROW_UP, icon::ARROW_DOWN),
                                );
                                hint(ui, format!("{}  abrir", icon::KEY_RETURN));
                                hint(ui, "Ctrl+Intro  en pestaña nueva".to_string());
                                hint(ui, "Esc  cerrar".to_string());
                            });
                        });
                });
        });

    // Remember the on-screen area so page input is not forwarded underneath.
    w.chrome.overlay_rects.push(screen);

    let chosen =
        run_idx.and_then(|(i, new_tab)| entries.into_iter().nth(i).map(|e| (e.run, new_tab)));
    if close || esc || chosen.is_some() {
        p.close();
    }
    w.chrome.palette = p;
    if let Some((run, new_tab)) = chosen {
        run_entry(run, new_tab, w, env, cmds);
    }
}
