//! Address bar with live suggestions (history, bookmarks, open tabs, search engine).

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Id, Rect, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::omnibox::{self as core_omnibox, Suggestion, SuggestionKind};

use super::{Palette, widgets};
use crate::actions::Action;
use crate::window::{BrowserWindow, Cmd, Disposition, Env};

/// Search suggestions fetched in the background: (query, suggestions).
type RemoteSlot = Arc<parking_lot::Mutex<Option<(String, Vec<String>)>>>;

#[derive(Clone, Copy)]
pub struct FieldRect(pub Rect);

impl Default for FieldRect {
    fn default() -> Self {
        Self(Rect::NOTHING)
    }
}

#[derive(Default)]
pub struct OmniboxState {
    /// Text being edited (None = not editing, show the tab URL).
    pub editing: Option<String>,
    pub open: bool,
    pub selected: usize,
    pub suggestions: Vec<Suggestion>,
    pub inline_completion: Option<String>,
    last_query: String,
    remote: RemoteSlot,
    remote_results: (String, Vec<String>),
    last_keystroke: Option<Instant>,
    fetched_for: String,
    pub field_rect: FieldRect,
    had_focus: bool,
    /// The click that focused the field is still held: select all on release unless dragged.
    click_focus_pending: bool,
}

impl OmniboxState {
    pub fn close(&mut self) {
        self.open = false;
        self.editing = None;
        self.selected = 0;
        self.inline_completion = None;
    }

    pub fn is_open(&self) -> bool {
        self.open && !self.suggestions.is_empty()
    }

    pub fn poll_suggestions(&mut self) {
        if let Some(r) = self.remote.lock().take() {
            self.remote_results = r;
            self.last_query.clear(); // force re-merge
        }
    }
}

fn field_id() -> Id {
    Id::new("omnibox-field")
}

pub fn omnibox(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    let (url_display, secure, bookmarked, zoom, url_string) = match w.active_tab() {
        Some(t) => {
            let u = t.url_string();
            (
                t.omnibox_display(),
                t.is_secure(),
                env.profile.bookmarks.is_bookmarked(&u),
                t.zoom,
                u,
            )
        }
        None => (String::new(), None, false, 1.0, String::new()),
    };
    let height = ui.spacing().interact_size.y.max(30.0);
    let width = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let focused = ui.memory(|m| m.has_focus(field_id()));
    let ctx = ui.ctx().clone();
    let focus_t = widgets::fade(&ctx, Id::new("omnibox-focus-anim"), focused, 0.18);
    let hover_t = widgets::fade(
        &ctx,
        Id::new("omnibox-hover-anim"),
        ui.rect_contains_pointer(outer) && !focused,
        0.14,
    );
    let focused_fill = if pal.dark {
        Color32::from_rgb(0x16, 0x18, 0x1d)
    } else {
        Color32::WHITE
    };
    let fill = pal
        .field
        .lerp_to_gamma(pal.field.lerp_to_gamma(pal.text, 0.05), hover_t)
        .lerp_to_gamma(focused_fill, focus_t);
    let stroke = Stroke::new(1.5, pal.accent.gamma_multiply(focus_t));
    if focus_t > 0.0 {
        // Soft focus halo that grows with the ring.
        ui.painter().rect_stroke(
            outer.expand(1.5 + 1.5 * focus_t),
            height / 2.0 + 2.0,
            Stroke::new(3.0, pal.accent.gamma_multiply(0.18 * focus_t)),
            egui::StrokeKind::Outside,
        );
    }
    ui.painter()
        .rect(outer, height / 2.0, fill, stroke, egui::StrokeKind::Inside);
    w.chrome.omnibox.field_rect = FieldRect(outer);

    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(outer.shrink2(Vec2::new(10.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Security indicator.
    let (glyph, color, tip) = match secure {
        Some(true) if url_string.starts_with("ferro:") => {
            (icon::FLAME, pal.accent, "Página de FerroSurf")
        }
        Some(true) => (icon::LOCK_SIMPLE, pal.text_dim, "Conexión segura"),
        Some(false) => (icon::WARNING_CIRCLE, pal.danger, "Conexión no segura"),
        None => (icon::MAGNIFYING_GLASS, pal.text_dim, ""),
    };
    let glyph = if focused {
        icon::MAGNIFYING_GLASS
    } else {
        glyph
    };
    inner
        .label(RichText::new(glyph).color(color).size(15.0))
        .on_hover_text(tip);

    // Right side controls inside the field.
    let right_w = if (zoom - 1.0).abs() > 0.01 {
        92.0
    } else {
        34.0
    };
    let edit_w = (inner.available_width() - right_w).max(40.0);

    let mut text = w
        .chrome
        .omnibox
        .editing
        .clone()
        .unwrap_or_else(|| url_display.clone());
    let display_text = if focused {
        text.clone()
    } else {
        core_omnibox::display_url(&text)
    };
    let mut buffer = if focused { text.clone() } else { display_text };
    let edit = egui::TextEdit::singleline(&mut buffer)
        .id(field_id())
        .frame(egui::Frame::NONE)
        .hint_text(format!(
            "Busca con {} o escribe una dirección",
            env.profile.settings.current_search_engine().name
        ))
        .desired_width(edit_w)
        .margin(egui::Margin::symmetric(4, 0))
        .font(egui::TextStyle::Body);
    let mut output = edit.show(&mut inner);
    let resp = output.response.clone();

    // Focus requests (Ctrl+L, new tab).
    if w.chrome.focus_omnibox {
        w.chrome.focus_omnibox = false;
        resp.request_focus();
        w.chrome.select_all_omnibox = true;
        w.chrome.omnibox.editing = Some(url_display.clone());
        buffer = url_display.clone();
    }
    if resp.gained_focus() || (w.chrome.select_all_omnibox && resp.has_focus()) {
        // Focus from a click: egui already placed the caret, but on the *displayed* text
        // (scheme hidden), which has different char indices than the URL we edit from the next
        // frame on. Map the caret to the URL and decide on release (plain click = select all,
        // drag = keep the dragged range). Ctrl+L / Tab / new tab select everything right away.
        let by_pointer = !w.chrome.select_all_omnibox
            && !focused
            && resp.hovered()
            && ui.input(|i| i.pointer.any_pressed() || i.pointer.any_released());
        w.chrome.select_all_omnibox = false;
        if w.chrome.omnibox.editing.is_none() {
            w.chrome.omnibox.editing = Some(url_display.clone());
        }
        let edit_text = w
            .chrome
            .omnibox
            .editing
            .clone()
            .unwrap_or_else(|| url_display.clone());
        let range = if by_pointer {
            let shown = output
                .state
                .cursor
                .char_range()
                .map_or(0, |r| r.primary.index.0);
            let anchor = core_omnibox::display_to_edit_index(&text, shown);
            w.chrome.omnibox.click_focus_pending = true;
            egui::text::CCursorRange::one(egui::text::CCursor::new(anchor))
        } else {
            w.chrome.omnibox.click_focus_pending = false;
            egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(edit_text.chars().count()),
            )
        };
        output.state.cursor.set_char_range(Some(range));
        // `store` takes the state by value; the release check below still
        // reads it this frame.
        output.state.clone().store(ui.ctx(), field_id());
    }
    if w.chrome.omnibox.click_focus_pending
        && (!resp.has_focus() || !ui.input(|i| i.pointer.any_down()))
    {
        w.chrome.omnibox.click_focus_pending = false;
        if resp.has_focus() {
            let edit_text = w
                .chrome
                .omnibox
                .editing
                .clone()
                .unwrap_or_else(|| url_display.clone());
            let current = output
                .state
                .cursor
                .char_range()
                .map_or((0, 0), |r| (r.secondary.index.0, r.primary.index.0));
            let (anchor, caret) = core_omnibox::focus_click_selection(&edit_text, current);
            output
                .state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(anchor),
                    egui::text::CCursor::new(caret),
                )));
            output.state.store(ui.ctx(), field_id());
        }
    }
    if focused {
        text = buffer.clone();
        if resp.changed() {
            w.chrome.omnibox.editing = Some(text.clone());
            w.chrome.omnibox.open = !text.trim().is_empty();
            w.chrome.omnibox.selected = 0;
            w.chrome.omnibox.last_keystroke = Some(Instant::now());
        }
    }
    let lost = resp.lost_focus();
    w.chrome.omnibox.had_focus = resp.has_focus();

    // Keyboard navigation of suggestions.
    if resp.has_focus() && w.chrome.omnibox.is_open() {
        let n = w.chrome.omnibox.suggestions.len();
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                w.chrome.omnibox.selected = (w.chrome.omnibox.selected + 1) % n;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                w.chrome.omnibox.selected = (w.chrome.omnibox.selected + n - 1) % n;
            }
        });
    }
    let enter = lost && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if enter {
        let mods = ui.input(|i| i.modifiers);
        let disposition = if mods.alt {
            Disposition::NewForegroundTab
        } else {
            Disposition::CurrentTab
        };
        let chosen = if w.chrome.omnibox.is_open() {
            w.chrome
                .omnibox
                .suggestions
                .get(w.chrome.omnibox.selected)
                .cloned()
        } else {
            None
        };
        let input = match chosen {
            Some(s) if s.kind == SuggestionKind::OpenTab => {
                if let Some(id) = s.url.strip_prefix("tab:").and_then(|i| i.parse().ok()) {
                    cmds.push(Cmd::ActivateTab(id));
                }
                None
            }
            Some(s) if s.kind == SuggestionKind::Command => {
                run_command(&s.url, cmds);
                None
            }
            Some(s) if s.kind != SuggestionKind::WhatYouTyped => Some(s.url),
            _ => {
                // Ctrl+Enter: www.<text>.com
                if mods.command && !text.contains('.') && !text.contains(' ') {
                    Some(format!("www.{}.com", text.trim()))
                } else if let Some(c) = &w.chrome.omnibox.inline_completion {
                    Some(c.clone())
                } else {
                    Some(text.clone())
                }
            }
        };
        if let Some(input) = input.filter(|s| !s.trim().is_empty()) {
            cmds.push(Cmd::Navigate { input, disposition });
        }
        w.chrome.omnibox.close();
    } else if lost {
        // Clicking a suggestion is handled by the dropdown before focus is lost.
        let pointer_on_dropdown = ui.ctx().pointer_latest_pos().is_some_and(|p| {
            ui.ctx()
                .layer_id_at(p)
                .is_some_and(|l| l.id == Id::new("omnibox-dropdown"))
        });
        if !pointer_on_dropdown {
            w.chrome.omnibox.close();
        }
    }

    // Right side: zoom badge + bookmark star.
    inner.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let star = icon::STAR;
        let star_resp = ui.add(
            egui::Button::new(RichText::new(star).size(16.0).color(if bookmarked {
                pal.accent
            } else {
                pal.text_dim
            }))
            .frame(false),
        );
        if star_resp
            .on_hover_text("Añadir esta página a marcadores (Ctrl+D)")
            .clicked()
        {
            cmds.push(Cmd::Action(Action::BookmarkPage));
        }
        if (zoom - 1.0).abs() > 0.01 {
            let z = ui.add(
                egui::Button::new(RichText::new(format!("{:.0}%", zoom * 100.0)).small())
                    .corner_radius(10)
                    .fill(pal.tab_hover),
            );
            if z.on_hover_text("Restablecer zoom").clicked() {
                cmds.push(Cmd::Action(Action::ZoomReset));
            }
        }
    });

    update_suggestions(w, env);
}

fn run_command(cmd: &str, cmds: &mut Vec<Cmd>) {
    let action = match cmd {
        "cmd:settings" => Action::ShowSettings,
        "cmd:history" => Action::ShowHistory,
        "cmd:bookmarks" => Action::ShowBookmarks,
        "cmd:downloads" => Action::ShowDownloads,
        "cmd:extensions" => Action::ShowExtensions,
        "cmd:assistant" => Action::ToggleAssistant,
        "cmd:palette" => Action::CommandPalette,
        _ => return,
    };
    cmds.push(Cmd::Action(action));
}

fn update_suggestions(w: &mut BrowserWindow, env: &mut Env) {
    let st = &mut w.chrome.omnibox;
    let Some(query) = st.editing.clone() else {
        st.suggestions.clear();
        return;
    };
    let q = query.trim().to_string();
    if q.is_empty() {
        st.suggestions.clear();
        st.inline_completion = None;
        return;
    }
    // Remote search suggestions, debounced.
    if env.profile.settings.privacy.search_suggestions
        && !env.profile.profile.ephemeral
        && st.fetched_for != q
        && st
            .last_keystroke
            .is_some_and(|t| t.elapsed() > Duration::from_millis(150))
        && !q.contains("://")
    {
        st.fetched_for = q.clone();
        let engine = env.profile.settings.current_search_engine();
        if let Some(url) = engine.suggest_url(&q) {
            let slot = st.remote.clone();
            let wake = env.shared.wake.clone();
            let query = q.clone();
            std::thread::spawn(move || {
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(3)))
                    .build()
                    .into();
                if let Ok(mut resp) = agent.get(url.as_str()).call()
                    && let Ok(body) = resp.body_mut().read_to_string()
                {
                    *slot.lock() = Some((query, ferrosurf_core::search::parse_suggestions(&body)));
                    if let Some(wake) = wake {
                        wake();
                    }
                }
            });
        }
    }
    if st.last_query == q {
        return;
    }
    st.last_query = q.clone();

    let mut out: Vec<Suggestion> = Vec::new();
    let engines = env.profile.settings.search_engines();
    let engine = env.profile.settings.current_search_engine();
    if let Some(interp) = ferrosurf_core::omnibox::interpret(&q, &engine, &engines) {
        let (title, url) = match &interp {
            ferrosurf_core::omnibox::Interpretation::Navigate(u) => (u.to_string(), q.clone()),
            ferrosurf_core::omnibox::Interpretation::Search {
                engine_id, terms, ..
            } => {
                let name = engines
                    .iter()
                    .find(|e| &e.id == engine_id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                (format!("{terms} — buscar con {name}"), q.clone())
            }
        };
        out.push(Suggestion {
            kind: SuggestionKind::WhatYouTyped,
            title,
            url,
            score: 10_000,
        });
    }
    // Commands starting with ">".
    if let Some(c) = q.strip_prefix('>') {
        for (id, label) in [
            ("cmd:settings", "Ajustes"),
            ("cmd:history", "Historial"),
            ("cmd:bookmarks", "Marcadores"),
            ("cmd:downloads", "Descargas"),
            ("cmd:extensions", "Extensiones"),
            ("cmd:assistant", "Asistente IA"),
            ("cmd:palette", "Paleta de comandos"),
        ] {
            if let Some(s) = ferrosurf_core::util::match_score(c.trim(), label)
                .or(c.trim().is_empty().then_some(1))
            {
                out.push(Suggestion {
                    kind: SuggestionKind::Command,
                    title: label.into(),
                    url: id.into(),
                    score: 5000 + s,
                });
            }
        }
    }
    // Open tabs.
    for t in &w.tabs {
        let hay = format!("{} {}", t.title, t.url_string());
        if let Some(s) = ferrosurf_core::util::match_score(&q, &hay) {
            out.push(Suggestion {
                kind: SuggestionKind::OpenTab,
                title: t.display_title(),
                url: format!("tab:{}", t.id),
                score: 300 + s,
            });
        }
    }
    for (s, b) in env.profile.bookmarks.search(&q, 4) {
        out.push(Suggestion {
            kind: SuggestionKind::Bookmark,
            title: b.title.clone(),
            url: b.url.clone(),
            score: 400 + s,
        });
    }
    if let Ok(hits) = env.profile.history.suggest(&q, 6) {
        for (s, e) in hits {
            out.push(Suggestion {
                kind: SuggestionKind::History,
                title: e.title.clone(),
                url: e.url.clone(),
                score: 200 + s,
            });
        }
    }
    // Inline completion from the best history/bookmark host.
    let candidates: Vec<String> = out
        .iter()
        .filter(|s| matches!(s.kind, SuggestionKind::History | SuggestionKind::Bookmark))
        .map(|s| s.url.clone())
        .collect();
    st.inline_completion =
        ferrosurf_core::omnibox::inline_completion(&q, candidates.iter().map(String::as_str));
    if st.remote_results.0 == q || q.starts_with(&st.remote_results.0) {
        for (i, s) in st.remote_results.1.iter().take(4).enumerate() {
            if s != &q {
                out.push(Suggestion {
                    kind: SuggestionKind::SearchSuggestion,
                    title: s.clone(),
                    url: s.clone(),
                    score: 100 - i as u32,
                });
            }
        }
    }
    let ranked = ferrosurf_core::omnibox::rank(out, 9);
    st.suggestions = ranked;
    st.selected = st.selected.min(st.suggestions.len().saturating_sub(1));
}

pub fn dropdown(ctx: &egui::Context, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let dd_anim = Id::new("omnibox-dropdown-anim");
    let sel_anim = Id::new("omnibox-dropdown-sel");
    if !w.chrome.omnibox.is_open() || !w.chrome.omnibox.had_focus {
        // Reset so the next opening animates from scratch.
        ctx.animate_bool_with_time(dd_anim, false, 0.0);
        ctx.data_mut(|d| d.remove::<f32>(sel_anim));
        return;
    }
    let appear = widgets::fade(ctx, dd_anim, true, 0.16);
    let pal = Palette::get(ctx);
    let field = w.chrome.omnibox.field_rect.0;
    let mut clicked: Option<Suggestion> = None;
    let area = egui::Area::new(Id::new("omnibox-dropdown"))
        .order(egui::Order::Foreground)
        .fixed_pos(field.left_bottom() + Vec2::new(0.0, 4.0 - (1.0 - appear) * 6.0))
        .show(ctx, |ui| {
            ui.set_opacity(appear);
            egui::Frame::new()
                .fill(pal.toolbar)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(12)
                .shadow(ctx.global_style().visuals.popup_shadow)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(field.width() - 12.0);
                    // Selection highlight glides between rows.
                    let row_h = 32.0;
                    let pitch = row_h + ui.spacing().item_spacing.y;
                    let top0 = ui.cursor().top();
                    if !w.chrome.omnibox.suggestions.is_empty() {
                        let off = widgets::smooth(
                            ctx,
                            sel_anim,
                            w.chrome.omnibox.selected as f32 * pitch,
                            None,
                            24.0,
                        );
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                egui::Pos2::new(ui.cursor().left(), top0 + off),
                                Vec2::new(ui.available_width(), row_h),
                            ),
                            8.0,
                            pal.accent.gamma_multiply(0.22),
                        );
                    }
                    for (i, s) in w.chrome.omnibox.suggestions.iter().enumerate() {
                        let selected = i == w.chrome.omnibox.selected;
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 32.0),
                            Sense::click(),
                        );
                        let hover_t = widgets::fade(
                            ctx,
                            Id::new(("omnibox-row-hover", i)),
                            resp.hovered() && !selected,
                            0.1,
                        );
                        if hover_t > 0.0 {
                            ui.painter().rect_filled(
                                rect,
                                8.0,
                                pal.tab_hover.gamma_multiply(hover_t),
                            );
                        }
                        let glyph = match s.kind {
                            SuggestionKind::WhatYouTyped if s.title.contains(" — buscar con ") => {
                                icon::MAGNIFYING_GLASS
                            }
                            SuggestionKind::WhatYouTyped => icon::GLOBE_SIMPLE,
                            SuggestionKind::OpenTab => icon::SQUARES_FOUR,
                            SuggestionKind::Bookmark => icon::STAR,
                            SuggestionKind::History => icon::CLOCK_COUNTER_CLOCKWISE,
                            SuggestionKind::SearchSuggestion => icon::MAGNIFYING_GLASS,
                            SuggestionKind::Command => icon::COMMAND,
                        };
                        let p = ui.painter();
                        p.text(
                            rect.left_center() + Vec2::new(16.0, 0.0),
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            egui::FontId::proportional(15.0),
                            pal.text_dim,
                        );
                        let title = if s.title.is_empty() {
                            ferrosurf_core::util::pretty_url(&s.url)
                        } else {
                            s.title.clone()
                        };
                        let title_g =
                            p.layout_no_wrap(title, egui::FontId::proportional(13.0), pal.text);
                        let tw = title_g.size().x.min(rect.width() * 0.6);
                        let text_rect = Rect::from_min_max(
                            rect.left_top() + Vec2::new(34.0, 0.0),
                            rect.right_bottom() - Vec2::new(8.0, 0.0),
                        );
                        let clip = p.with_clip_rect(text_rect);
                        clip.galley(
                            egui::pos2(text_rect.left(), rect.center().y - title_g.size().y / 2.0),
                            title_g,
                            pal.text,
                        );
                        let sub = match s.kind {
                            SuggestionKind::History | SuggestionKind::Bookmark => {
                                ferrosurf_core::util::pretty_url(&s.url)
                            }
                            SuggestionKind::OpenTab => "Cambiar a esta pestaña".into(),
                            _ => String::new(),
                        };
                        if !sub.is_empty() {
                            clip.text(
                                egui::pos2(text_rect.left() + tw + 10.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                format!("— {sub}"),
                                egui::FontId::proportional(12.0),
                                pal.accent,
                            );
                        }
                        if resp.clicked() {
                            clicked = Some(s.clone());
                        }
                    }
                });
        });
    w.chrome.overlay_rects.push(area.response.rect);
    if let Some(s) = clicked {
        match s.kind {
            SuggestionKind::OpenTab => {
                if let Some(id) = s.url.strip_prefix("tab:").and_then(|i| i.parse().ok()) {
                    cmds.push(Cmd::ActivateTab(id));
                }
            }
            SuggestionKind::Command => run_command(&s.url, cmds),
            _ => cmds.push(Cmd::Navigate {
                input: s.url,
                disposition: Disposition::CurrentTab,
            }),
        }
        w.chrome.omnibox.close();
        ctx.memory_mut(|m| m.surrender_focus(field_id()));
    }
    let _ = (env, widgets::section as fn(&mut Ui, &str));
}
