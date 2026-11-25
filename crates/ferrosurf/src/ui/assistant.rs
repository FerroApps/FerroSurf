//! AI assistant side panel ("Asistente").

use egui::{Color32, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use ferrosurf_core::settings::AssistantProvider;
use ferrosurf_mcp::assistant::{AssistantEvent, ChatMessage};

use super::widgets::{accent_button, icon_button, subtle_button};
use super::{Palette, SettingsPage};
use crate::window::{AppCmd, BrowserWindow, Cmd, Disposition, Env};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AssistantState {
    /// Provider-neutral transcript used for the next API call (replaced on TurnComplete).
    pub transcript: Vec<ChatMessage>,
    /// What the UI renders (user msgs, assistant text incl. in-progress streaming, tool call cards).
    pub entries: Vec<ChatEntry>,
    pub running: bool,
    pub error: Option<String>,
    pub input: String,
    /// Last user message, used by "Reintentar".
    pub last_user: Option<String>,
    /// Request keyboard focus for the composer on the next frame.
    pub focus_input: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChatEntry {
    User(String),
    Assistant(String),
    Tool {
        id: String,
        name: String,
        args: serde_json::Value,
        done: bool,
        is_error: bool,
        summary: String,
    },
}

impl AssistantState {
    /// Called by the app for every event coming from `run_turn`.
    pub fn apply(&mut self, ev: AssistantEvent) {
        match ev {
            AssistantEvent::TextDelta(t) => {
                if let Some(ChatEntry::Assistant(s)) = self.entries.last_mut() {
                    s.push_str(&t);
                } else {
                    self.entries.push(ChatEntry::Assistant(t));
                }
            }
            AssistantEvent::ToolCallStarted { id, name, args } => {
                self.entries.push(ChatEntry::Tool {
                    id,
                    name,
                    args,
                    done: false,
                    is_error: false,
                    summary: String::new(),
                });
            }
            AssistantEvent::ToolCallFinished {
                id,
                name,
                is_error,
                summary,
            } => {
                let found = self.entries.iter_mut().rev().find_map(|e| match e {
                    ChatEntry::Tool {
                        id: i,
                        done,
                        is_error: ie,
                        summary: s,
                        ..
                    } if *i == id => Some((done, ie, s)),
                    _ => None,
                });
                match found {
                    Some((done, ie, s)) => {
                        *done = true;
                        *ie = is_error;
                        *s = summary;
                    }
                    None => self.entries.push(ChatEntry::Tool {
                        id,
                        name,
                        args: serde_json::Value::Null,
                        done: true,
                        is_error,
                        summary,
                    }),
                }
            }
            AssistantEvent::TurnComplete { messages } => {
                self.transcript = messages;
                self.running = false;
                // Any tool card still spinning is finished by now.
                for e in &mut self.entries {
                    if let ChatEntry::Tool { done, .. } = e {
                        *done = true;
                    }
                }
            }
            AssistantEvent::Error(e) => {
                self.error = Some(e);
                self.running = false;
            }
        }
    }

    /// Called right before a turn starts (adds the user entry, sets running).
    pub fn begin_turn(&mut self, user_text: &str) {
        self.entries.push(ChatEntry::User(user_text.to_string()));
        self.last_user = Some(user_text.to_string());
        self.running = true;
        self.error = None;
    }

    pub fn clear(&mut self) {
        self.transcript.clear();
        self.entries.clear();
        self.error = None;
        self.input.clear();
        self.last_user = None;
        // A running turn keeps streaming into a fresh list; the app stops it separately.
    }
}

// ---------------------------------------------------------------------------
// Tool labels
// ---------------------------------------------------------------------------

pub fn tool_icon(name: &str) -> &'static str {
    if name.contains("__") {
        return icon::PLUG;
    }
    match name {
        "browser_screenshot" => icon::CAMERA,
        "browser_click" | "browser_select_option" | "browser_scroll" => icon::CURSOR_CLICK,
        "browser_type" => icon::KEYBOARD,
        "history_search" | "bookmarks_search" | "browser_find_interactive" => {
            icon::MAGNIFYING_GLASS
        }
        n if n.starts_with("browser_read")
            || n.starts_with("browser_get")
            || n == "browser_list_tabs" =>
        {
            icon::EYE
        }
        n if n.starts_with("browser_") => icon::GLOBE,
        n if n.starts_with("history") || n.starts_with("bookmark") => icon::CLOCK_COUNTER_CLOCKWISE,
        _ => icon::WRENCH,
    }
}

pub fn tool_label(name: &str, args: &serde_json::Value) -> String {
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "browser_read_page" => "Leyendo la página".into(),
        "browser_navigate" | "browser_open_tab" if !url.is_empty() => {
            format!("Navegando a {}", short_url(url))
        }
        "browser_navigate" => "Navegando".into(),
        "browser_open_tab" => "Abriendo una pestaña".into(),
        "browser_close_tab" => "Cerrando una pestaña".into(),
        "browser_activate_tab" => "Cambiando de pestaña".into(),
        "browser_list_tabs" => "Listando las pestañas".into(),
        "browser_back" => "Volviendo atrás".into(),
        "browser_forward" => "Avanzando".into(),
        "browser_reload" => "Recargando la página".into(),
        "browser_get_links" => "Obteniendo los enlaces".into(),
        "browser_get_html" => "Leyendo el HTML".into(),
        "browser_find_interactive" => "Buscando elementos interactivos".into(),
        "browser_click" => "Haciendo clic".into(),
        "browser_type" => "Escribiendo".into(),
        "browser_select_option" => "Seleccionando una opción".into(),
        "browser_scroll" => "Desplazando la página".into(),
        "browser_screenshot" => "Capturando la pantalla".into(),
        "browser_eval_js" => "Ejecutando JavaScript".into(),
        "history_search" => "Buscando en el historial".into(),
        "bookmarks_search" => "Buscando en los marcadores".into(),
        "bookmark_add" => "Guardando un marcador".into(),
        n => match n.split_once("__") {
            Some((server, tool)) => format!("{server} · {tool}"),
            None => n.to_string(),
        },
    }
}

fn short_url(url: &str) -> String {
    let u = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    if u.chars().count() > 40 {
        let t: String = u.chars().take(39).collect();
        format!("{t}…")
    } else {
        u.to_string()
    }
}

// ---------------------------------------------------------------------------
// Markdown-ish parsing (pure)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(String),
    Heading {
        level: u8,
        text: String,
    },
    /// `number` is Some for ordered list items.
    ListItem {
        number: Option<u32>,
        indent: usize,
        text: String,
    },
    Code {
        lang: String,
        code: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(String),
    Bold(String),
    Code(String),
    Link { text: String, url: String },
}

pub fn parse_blocks(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut lines = src.lines();

    fn flush(para: &mut Vec<&str>, blocks: &mut Vec<Block>) {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(para.join(" ")));
            para.clear();
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush(&mut para, &mut blocks);
            let lang = rest.trim().to_string();
            let mut code: Vec<&str> = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push(l);
            }
            blocks.push(Block::Code {
                lang,
                code: code.join("\n"),
            });
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut blocks);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading {
                level: hashes as u8,
                text: trimmed[hashes..].trim().to_string(),
            });
            continue;
        }
        let indent = line.len() - trimmed.len();
        if let Some(text) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|p| trimmed.strip_prefix(p))
        {
            flush(&mut para, &mut blocks);
            blocks.push(Block::ListItem {
                number: None,
                indent,
                text: text.trim().to_string(),
            });
            continue;
        }
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits <= 9 && trimmed[digits..].starts_with(". ") {
            flush(&mut para, &mut blocks);
            let n = trimmed[..digits].parse().unwrap_or(1);
            blocks.push(Block::ListItem {
                number: Some(n),
                indent,
                text: trimmed[digits + 2..].trim().to_string(),
            });
            continue;
        }
        // Continuation of a list item (indented text right after an item).
        if indent >= 2
            && para.is_empty()
            && let Some(Block::ListItem { text, .. }) = blocks.last_mut()
        {
            text.push(' ');
            text.push_str(trimmed.trim_end());
            continue;
        }
        para.push(trimmed.trim_end());
    }
    flush(&mut para, &mut blocks);
    blocks
}

pub fn parse_inline(src: &str) -> Vec<Span> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
        if pat.is_empty() || chars.len() < pat.len() {
            return None;
        }
        (from..=chars.len() - pat.len()).find(|&k| chars[k..k + pat.len()] == *pat)
    }
    fn flush(buf: &mut String, out: &mut Vec<Span>) {
        if !buf.is_empty() {
            out.push(Span::Text(std::mem::take(buf)));
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(end) = find(&chars, i + 1, &['`'])
            && end > i + 1
        {
            flush(&mut buf, &mut out);
            out.push(Span::Code(chars[i + 1..end].iter().collect()));
            i = end + 1;
            continue;
        }
        if c == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(end) = find(&chars, i + 2, &['*', '*'])
            && end > i + 2
        {
            flush(&mut buf, &mut out);
            out.push(Span::Bold(chars[i + 2..end].iter().collect()));
            i = end + 2;
            continue;
        }
        if c == '['
            && let Some(close) = find(&chars, i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(paren) = find(&chars, close + 2, &[')'])
        {
            let text: String = chars[i + 1..close].iter().collect();
            let url: String = chars[close + 2..paren].iter().collect();
            if !text.is_empty() && !url.trim().is_empty() {
                flush(&mut buf, &mut out);
                out.push(Span::Link {
                    text,
                    url: url.trim().to_string(),
                });
                i = paren + 1;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn open_link(cmds: &mut Vec<Cmd>, url: &str) {
    if let Ok(u) = url::Url::parse(url) {
        cmds.push(Cmd::OpenUrl {
            url: u,
            disposition: Disposition::NewForegroundTab,
        });
    }
}

fn render_inline(ui: &mut Ui, text: &str, size: f32, strong: bool, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in parse_inline(text) {
            match span {
                Span::Text(t) => {
                    let mut rt = RichText::new(t).size(size).color(pal.text);
                    if strong {
                        rt = rt.strong();
                    }
                    ui.label(rt);
                }
                Span::Bold(t) => {
                    ui.label(RichText::new(t).size(size).strong().color(pal.text));
                }
                Span::Code(t) => {
                    ui.label(
                        RichText::new(t)
                            .monospace()
                            .size(size - 1.0)
                            .color(pal.text)
                            .background_color(pal.field),
                    );
                }
                Span::Link { text, url } => {
                    let r = ui.add(egui::Hyperlink::from_label_and_url(
                        RichText::new(text).size(size).color(pal.accent),
                        url.clone(),
                    ));
                    if r.clicked() || r.middle_clicked() {
                        open_link(cmds, &url);
                    }
                }
            }
        }
    });
}

fn render_code(ui: &mut Ui, lang: &str, code: &str, pal: &Palette) {
    egui::Frame::new()
        .fill(pal.field)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if lang.is_empty() { "código" } else { lang })
                        .small()
                        .color(pal.text_dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("{} Copiar", icon::COPY)).small(),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(code.to_string());
                    }
                });
            });
            egui::ScrollArea::horizontal()
                .id_salt(code.len() ^ lang.len())
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(code).monospace().size(12.5).color(pal.text),
                        )
                        .extend(),
                    );
                });
        });
}

fn render_markdown(ui: &mut Ui, text: &str, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());
    ui.spacing_mut().item_spacing.y = 6.0;
    for block in parse_blocks(text) {
        match block {
            Block::Paragraph(t) => render_inline(ui, &t, 14.0, false, cmds),
            Block::Heading { level, text } => {
                ui.add_space(2.0);
                let size = match level {
                    1 => 18.0,
                    2 => 16.0,
                    _ => 14.5,
                };
                render_inline(ui, &text, size, true, cmds);
            }
            Block::ListItem {
                number,
                indent,
                text,
            } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0 + (indent / 2) as f32 * 14.0);
                    let marker = match number {
                        Some(n) => format!("{n}."),
                        None => "•".to_string(),
                    };
                    ui.add_sized(
                        [16.0, 18.0],
                        egui::Label::new(RichText::new(marker).color(pal.text_dim)),
                    );
                    ui.vertical(|ui| render_inline(ui, &text, 14.0, false, cmds));
                });
            }
            Block::Code { lang, code } => render_code(ui, &lang, &code, &pal),
        }
    }
}

fn tool_card(ui: &mut Ui, entry: &ChatEntry, pal: &Palette) {
    let ChatEntry::Tool {
        id,
        name,
        args,
        done,
        is_error,
        summary,
    } = entry
    else {
        return;
    };
    let label = tool_label(name, args);
    let state_id = ui.make_persistent_id(("assistant-tool", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(state_id)).unwrap_or(false);
    egui::Frame::new()
        .fill(pal.field)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = ui.horizontal(|ui| {
                ui.label(RichText::new(tool_icon(name)).size(15.0).color(pal.accent));
                ui.label(RichText::new(&label).size(12.5).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(if open {
                            icon::CARET_DOWN
                        } else {
                            icon::CARET_RIGHT
                        })
                        .small()
                        .color(pal.text_dim),
                    );
                    if !*done {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                        let t = ui.input(|i| i.time);
                        super::widgets::paint_spinner(
                            ui.painter(),
                            rect.center(),
                            5.5,
                            pal.accent,
                            t,
                        );
                        ui.ctx().request_repaint();
                    } else if *is_error {
                        ui.label(RichText::new(icon::X_CIRCLE).color(pal.danger));
                    } else {
                        ui.label(RichText::new(icon::CHECK_CIRCLE).color(pal.ok));
                    }
                });
            });
            let resp = ui.interact(header.response.rect, state_id.with("click"), Sense::click());
            if resp.clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(state_id, open));
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if open {
                ui.add_space(4.0);
                if !args.is_null() {
                    let json = serde_json::to_string_pretty(args).unwrap_or_default();
                    ui.label(
                        RichText::new(truncate(&json, 1500))
                            .monospace()
                            .size(11.5)
                            .color(pal.text_dim),
                    );
                }
                if !summary.is_empty() {
                    ui.add_space(2.0);
                    let color = if *is_error { pal.danger } else { pal.text };
                    ui.label(
                        RichText::new(truncate(summary, 1500))
                            .size(12.0)
                            .color(color),
                    );
                }
            }
        });
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

fn user_bubble(ui: &mut Ui, text: &str, pal: &Palette) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        let fill = pal
            .accent
            .gamma_multiply(if pal.dark { 0.28 } else { 0.16 });
        egui::Frame::new()
            .fill(fill)
            .corner_radius(egui::CornerRadius {
                nw: 14,
                ne: 14,
                sw: 14,
                se: 4,
            })
            .inner_margin(egui::Margin::symmetric(11, 8))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.88);
                ui.add(egui::Label::new(RichText::new(text).size(14.0).color(pal.text)).wrap());
            });
    });
}

fn caret(ui: &mut Ui, pal: &Palette) {
    let t = ui.input(|i| i.time);
    let on = (t * 2.0) as i64 % 2 == 0;
    ui.label(RichText::new("▍").color(if on { pal.accent } else { Color32::TRANSPARENT }));
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(250));
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

const SUGGESTIONS: &[(&str, bool)] = &[
    ("Resume esta página", true),
    ("¿Qué enlaces importantes hay aquí?", true),
    ("Traduce esta página al español", true),
    ("Busca en mi historial lo que leí sobre…", false),
    ("Rellena el formulario de esta página con…", false),
];

pub fn panel(ui: &mut Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ui.ctx());

    // Header
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon::SPARKLE).size(18.0).color(pal.accent));
        ui.label(
            RichText::new("Asistente")
                .size(16.0)
                .strong()
                .color(pal.text),
        );
        let model = env.profile.settings.assistant.model.clone();
        if !model.is_empty() {
            ui.label(RichText::new(model).small().color(pal.text_dim));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, icon::X, "Cerrar", true).clicked() {
                w.chrome.assistant_open = false;
            }
            if icon_button(ui, icon::GEAR, "Ajustes del asistente", true).clicked() {
                w.chrome.settings = Some(SettingsPage::Assistant);
            }
            if icon_button(ui, icon::NOTE_PENCIL, "Nueva conversación", true).clicked() {
                if env.shared.assistant.running {
                    cmds.push(Cmd::App(AppCmd::StopAssistant));
                }
                env.shared.assistant.clear();
            }
        });
    });
    ui.separator();

    let settings = &env.profile.settings.assistant;
    let needs_key =
        settings.provider == AssistantProvider::Anthropic && settings.resolved_api_key().is_none();
    let tool_total: usize = env.shared.mcp_clients.iter().map(|c| c.tool_count).sum();

    // Composer is laid out first (bottom panel) so the transcript fills the rest.
    let mut send: Option<String> = None;
    let mut stop = false;
    egui::Panel::bottom("assistant-composer")
        .frame(egui::Frame::new())
        .show_separator_line(false)
        .show(ui, |ui| {
            composer(
                ui,
                &mut env.shared.assistant,
                &pal,
                tool_total,
                &mut send,
                &mut stop,
            );
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::new())
        .show(ui, |ui| {
            let mut retry = false;
            let mut prefill: Option<String> = None;
            {
                let st = &mut env.shared.assistant;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add_space(6.0);
                        if needs_key && st.entries.is_empty() {
                            if onboarding_card(ui, &pal) {
                                w.chrome.settings = Some(SettingsPage::Assistant);
                            }
                            ui.add_space(8.0);
                        }
                        if st.entries.is_empty() {
                            empty_state(ui, &pal, &mut send, &mut prefill);
                        }
                        let last = st.entries.len().saturating_sub(1);
                        for (i, e) in st.entries.iter().enumerate() {
                            match e {
                                ChatEntry::User(t) => user_bubble(ui, t, &pal),
                                ChatEntry::Assistant(t) => {
                                    ui.vertical(|ui| {
                                        render_markdown(ui, t, cmds);
                                        if st.running && i == last {
                                            caret(ui, &pal);
                                        }
                                    });
                                }
                                ChatEntry::Tool { .. } => tool_card(ui, e, &pal),
                            }
                            ui.add_space(8.0);
                        }
                        if st.running
                            && !matches!(
                                st.entries.last(),
                                Some(ChatEntry::Assistant(_) | ChatEntry::Tool { done: false, .. })
                            )
                        {
                            ui.horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                                let t = ui.input(|i| i.time);
                                super::widgets::paint_spinner(
                                    ui.painter(),
                                    rect.center(),
                                    6.0,
                                    pal.accent,
                                    t,
                                );
                                ui.label(RichText::new("Pensando…").small().color(pal.text_dim));
                            });
                            ui.ctx().request_repaint();
                        }
                        if let Some(err) = st.error.clone() {
                            egui::Frame::new()
                                .fill(pal.danger.gamma_multiply(0.15))
                                .stroke(egui::Stroke::new(1.0, pal.danger.gamma_multiply(0.6)))
                                .corner_radius(8)
                                .inner_margin(egui::Margin::same(8))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_top(|ui| {
                                        ui.label(RichText::new(icon::WARNING).color(pal.danger));
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(truncate(&err, 600))
                                                    .size(12.5)
                                                    .color(pal.text),
                                            )
                                            .wrap(),
                                        );
                                    });
                                    ui.horizontal(|ui| {
                                        if st.last_user.is_some()
                                            && !st.running
                                            && subtle_button(
                                                ui,
                                                &format!("{} Reintentar", icon::ARROW_CLOCKWISE),
                                            )
                                            .clicked()
                                        {
                                            retry = true;
                                        }
                                        if subtle_button(ui, "Descartar").clicked() {
                                            st.error = None;
                                        }
                                    });
                                });
                            ui.add_space(6.0);
                        }
                        ui.add_space(4.0);
                    });
            }
            if retry && let Some(t) = env.shared.assistant.last_user.clone() {
                env.shared.assistant.error = None;
                send = Some(t);
            }
            if let Some(p) = prefill {
                env.shared.assistant.input = p;
                env.shared.assistant.focus_input = true;
            }
        });

    if stop {
        cmds.push(Cmd::App(AppCmd::StopAssistant));
    }
    if let Some(text) = send
        && !env.shared.assistant.running
    {
        env.shared.assistant.input.clear();
        cmds.push(Cmd::App(AppCmd::StartAssistant { text }));
    }
}

fn onboarding_card(ui: &mut Ui, pal: &Palette) -> bool {
    let mut open = false;
    egui::Frame::new()
        .fill(pal.field)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{} Configura el asistente", icon::SPARKLE)).strong().color(pal.text));
            ui.add_space(4.0);
            for line in [
                "Para usar Claude necesitas una clave de API de Anthropic: pégala en los ajustes o define la variable de entorno ANTHROPIC_API_KEY.",
                "También puedes usar un servidor local compatible con OpenAI, como Ollama (http://localhost:11434/v1), sin enviar nada a la nube.",
            ] {
                ui.add(egui::Label::new(RichText::new(line).size(12.5).color(pal.text_dim)).wrap());
                ui.add_space(3.0);
            }
            ui.add_space(4.0);
            if accent_button(ui, "Abrir ajustes del asistente").clicked() {
                open = true;
            }
        });
    open
}

fn empty_state(
    ui: &mut Ui,
    pal: &Palette,
    send: &mut Option<String>,
    prefill: &mut Option<String>,
) {
    ui.add_space(16.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon::SPARKLE).size(34.0).color(pal.accent));
        ui.label(
            RichText::new("¿En qué puedo ayudarte?")
                .size(17.0)
                .strong()
                .color(pal.text),
        );
        ui.label(
            RichText::new("Puedo leer la página, navegar y buscar en tu historial.")
                .small()
                .color(pal.text_dim),
        );
    });
    ui.add_space(12.0);
    for (text, immediate) in SUGGESTIONS {
        let r = ui.add_sized(
            [ui.available_width(), 32.0],
            egui::Button::new(RichText::new(*text).size(13.0).color(pal.text))
                .fill(pal.field)
                .corner_radius(16),
        );
        if r.clicked() {
            if *immediate {
                *send = Some((*text).to_string());
            } else {
                *prefill = Some(text.trim_end_matches('…').to_string() + " ");
            }
        }
        ui.add_space(2.0);
    }
}

fn composer(
    ui: &mut Ui,
    st: &mut AssistantState,
    pal: &Palette,
    tool_total: usize,
    send: &mut Option<String>,
    stop: &mut bool,
) {
    ui.add_space(6.0);
    egui::Frame::new()
        .fill(pal.field)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let id = egui::Id::new("assistant-input");
            // Enter sends, Shift+Enter inserts a newline.
            let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let out = egui::TextEdit::multiline(&mut st.input)
                .id(id)
                .hint_text("Pregunta lo que quieras sobre esta página…")
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE)
                .return_key(egui::KeyboardShortcut::new(
                    egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                ))
                .show(ui);
            if st.focus_input {
                out.response.request_focus();
                st.focus_input = false;
            }
            let can_send = !st.input.trim().is_empty() && !st.running;
            if enter && out.response.has_focus() && can_send {
                *send = Some(st.input.trim().to_string());
            }
            ui.horizontal(|ui| {
                let hint = if tool_total > 0 {
                    format!("Puede usar la página y {tool_total} herramientas MCP")
                } else {
                    "Puede usar la página".to_string()
                };
                ui.label(RichText::new(hint).size(11.0).color(pal.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if st.running {
                        if subtle_button(ui, &format!("{} Detener", icon::STOP)).clicked() {
                            *stop = true;
                        }
                    } else {
                        let btn = egui::Button::new(
                            RichText::new(icon::PAPER_PLANE_TILT)
                                .size(15.0)
                                .color(pal.on_accent()),
                        )
                        .fill(if can_send {
                            pal.accent
                        } else {
                            pal.accent.gamma_multiply(0.35)
                        })
                        .corner_radius(8);
                        if ui
                            .add_enabled(can_send, btn)
                            .on_hover_text("Enviar (Enter)")
                            .clicked()
                        {
                            *send = Some(st.input.trim().to_string());
                        }
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrosurf_mcp::assistant::ChatMessage;
    use serde_json::json;

    #[test]
    fn text_deltas_accumulate() {
        let mut s = AssistantState::default();
        s.begin_turn("hola");
        assert!(s.running);
        s.apply(AssistantEvent::TextDelta("Ho".into()));
        s.apply(AssistantEvent::TextDelta("la".into()));
        assert_eq!(
            s.entries,
            vec![
                ChatEntry::User("hola".into()),
                ChatEntry::Assistant("Hola".into())
            ]
        );
    }

    #[test]
    fn tool_cards_update() {
        let mut s = AssistantState::default();
        s.apply(AssistantEvent::ToolCallStarted {
            id: "a".into(),
            name: "browser_click".into(),
            args: json!({"x":1}),
        });
        s.apply(AssistantEvent::ToolCallStarted {
            id: "b".into(),
            name: "browser_read_page".into(),
            args: json!({}),
        });
        s.apply(AssistantEvent::ToolCallFinished {
            id: "a".into(),
            name: "browser_click".into(),
            is_error: true,
            summary: "boom".into(),
        });
        match &s.entries[0] {
            ChatEntry::Tool {
                done,
                is_error,
                summary,
                ..
            } => assert!(*done && *is_error && summary == "boom"),
            _ => panic!(),
        }
        match &s.entries[1] {
            ChatEntry::Tool { done, .. } => assert!(!*done),
            _ => panic!(),
        }
        // Text after a tool starts a new assistant entry.
        s.apply(AssistantEvent::TextDelta("x".into()));
        assert_eq!(s.entries.len(), 3);
    }

    #[test]
    fn turn_complete_and_error() {
        let mut s = AssistantState::default();
        s.begin_turn("q");
        s.apply(AssistantEvent::TurnComplete {
            messages: vec![ChatMessage::user("q"), ChatMessage::assistant("a")],
        });
        assert!(!s.running);
        assert_eq!(s.transcript.len(), 2);
        s.begin_turn("q2");
        s.apply(AssistantEvent::Error("fallo".into()));
        assert!(!s.running);
        assert_eq!(s.error.as_deref(), Some("fallo"));
        s.begin_turn("q3");
        assert!(s.error.is_none());
        s.clear();
        assert!(s.entries.is_empty() && s.transcript.is_empty());
    }

    #[test]
    fn parses_blocks() {
        let src = "# Título\n\nHola **mundo**\ncontinúa\n\n- uno\n- dos\n2. tres\n\n```rust\nfn main() {}\n\nlet x;\n```\nfin";
        let b = parse_blocks(src);
        assert_eq!(
            b[0],
            Block::Heading {
                level: 1,
                text: "Título".into()
            }
        );
        assert_eq!(b[1], Block::Paragraph("Hola **mundo** continúa".into()));
        assert_eq!(
            b[2],
            Block::ListItem {
                number: None,
                indent: 0,
                text: "uno".into()
            }
        );
        assert_eq!(
            b[4],
            Block::ListItem {
                number: Some(2),
                indent: 0,
                text: "tres".into()
            }
        );
        assert_eq!(
            b[5],
            Block::Code {
                lang: "rust".into(),
                code: "fn main() {}\n\nlet x;".into()
            }
        );
        assert_eq!(b[6], Block::Paragraph("fin".into()));
    }

    #[test]
    fn unclosed_fence_while_streaming() {
        let b = parse_blocks("```\nabc");
        assert_eq!(
            b,
            vec![Block::Code {
                lang: String::new(),
                code: "abc".into()
            }]
        );
    }

    #[test]
    fn parses_inline() {
        let s = parse_inline("a **b** `c` [d](https://e.com) f *g");
        assert_eq!(
            s,
            vec![
                Span::Text("a ".into()),
                Span::Bold("b".into()),
                Span::Text(" ".into()),
                Span::Code("c".into()),
                Span::Text(" ".into()),
                Span::Link {
                    text: "d".into(),
                    url: "https://e.com".into()
                },
                Span::Text(" f *g".into()),
            ]
        );
        assert_eq!(
            parse_inline("**sin cierre"),
            vec![Span::Text("**sin cierre".into())]
        );
    }

    #[test]
    fn labels() {
        assert_eq!(
            tool_label(
                "browser_navigate",
                &json!({"url":"https://www.rust-lang.org/"})
            ),
            "Navegando a rust-lang.org/"
        );
        assert_eq!(
            tool_label("github__list_issues", &json!({})),
            "github · list_issues"
        );
        assert_eq!(
            tool_label("history_search", &json!({})),
            "Buscando en el historial"
        );
    }
}
