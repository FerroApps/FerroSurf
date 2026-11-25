//! The browser chrome, drawn with egui around (and on top of) the Servo content area.

pub mod assistant;
pub mod dialogs;
pub mod find;
pub mod library;
pub mod omnibox;
pub mod palette;
pub mod settings;
pub mod tabs;
pub mod theme;
pub mod toolbar;
pub mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Id, Pos2, Rect};
use egui_phosphor::regular as icon;
use ferrosurf_core::BookmarkId;
use ferrosurf_core::permissions::PermissionKind;
use servo::OffscreenRenderingContext;

use crate::tab::TabId;
use crate::window::{BrowserWindow, Cmd, Env, WakeFn};
pub use theme::Palette;

/// App-wide UI state shared by all windows.
#[derive(Default)]
pub struct SharedUi {
    pub toasts: Vec<(String, Instant)>,
    pub mcp_url: String,
    pub devtools: Option<(u16, String)>,
    pub blocked_total: u64,
    pub downloads_attention: bool,
    /// Bumped when keybindings are edited.
    pub keymap_revision: u64,
    pub assistant: assistant::AssistantState,
    pub mcp_clients: Vec<ferrosurf_mcp::ServerStatus>,
    pub wake: Option<WakeFn>,
    /// External MCP actions waiting for confirmation (Ajustes → MCP → Confirmar acciones).
    pub mcp_approvals: Vec<crate::mcp_host::PendingApproval>,
    /// The user chose "allow for this session".
    pub mcp_trusted_session: bool,
}

impl SharedUi {
    pub fn toast(&mut self, msg: &str) {
        self.toasts.push((msg.to_string(), Instant::now()));
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryTab {
    Bookmarks,
    History,
    Downloads,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Appearance,
    Startup,
    Search,
    Privacy,
    Downloads,
    Assistant,
    Mcp,
    Extensions,
    Profiles,
    Shortcuts,
    Import,
    Advanced,
    About,
}

pub struct PermissionPrompt {
    pub tab: TabId,
    pub origin: String,
    pub kind: PermissionKind,
    pub request: Option<servo::PermissionRequest>,
}

pub struct AuthPrompt {
    pub tab: TabId,
    pub request: Option<servo::AuthenticationRequest>,
    pub user: String,
    pub password: String,
}

impl AuthPrompt {
    pub fn new(tab: TabId, request: servo::AuthenticationRequest) -> Self {
        Self {
            tab,
            request: Some(request),
            user: String::new(),
            password: String::new(),
        }
    }
}

/// Inline editor shown after bookmarking a page.
pub struct BookmarkEditor {
    pub id: BookmarkId,
    pub title: String,
    pub url: String,
    pub folder: BookmarkId,
}

impl BookmarkEditor {
    pub fn new(b: &ferrosurf_core::Bookmark) -> Self {
        Self {
            id: b.id,
            title: b.title.clone(),
            url: b.url.clone(),
            folder: ferrosurf_core::bookmarks::TOOLBAR_ID,
        }
    }
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub focus: bool,
    pub query: String,
    pub last_sent: String,
    /// "current/total" reported by the page script.
    pub result: String,
    pub result_slot: Arc<parking_lot::Mutex<Option<String>>>,
}

/// Per-window chrome state.
#[derive(Default)]
pub struct Chrome {
    pub focus_omnibox: bool,
    pub select_all_omnibox: bool,
    pub omnibox: omnibox::OmniboxState,
    pub find: FindState,
    pub library: Option<LibraryTab>,
    pub library_state: library::LibraryState,
    pub settings: Option<SettingsPage>,
    pub settings_state: settings::SettingsState,
    pub assistant_open: bool,
    pub palette: palette::Palette,
    pub permission_prompts: Vec<PermissionPrompt>,
    pub auth_prompts: Vec<AuthPrompt>,
    pub bookmark_editor: Option<BookmarkEditor>,
    pub zoom_bubble_until: Option<Instant>,
    pub tab_drag: Option<tabs::TabDrag>,
    pub dialogs: dialogs::DialogState,
    /// Screen rects (points) of floating chrome over the content (dropdowns etc).
    pub overlay_rects: Vec<Rect>,
}

impl Chrome {
    pub fn on_tab_switched(&mut self) {
        self.omnibox.close();
        if self.find.open {
            self.find.result.clear();
            self.find.last_sent.clear();
        }
    }

    pub fn on_content_click(&mut self) {
        self.omnibox.close();
        self.bookmark_editor = None;
    }

    pub fn open_library(&mut self, tab: LibraryTab) {
        if self.library == Some(tab) {
            self.library = None;
        } else {
            self.library = Some(tab);
        }
    }

    pub fn open_settings(&mut self, page: SettingsPage) {
        self.settings = Some(page);
    }

    pub fn show_zoom_bubble(&mut self) {
        self.zoom_bubble_until = Some(Instant::now() + Duration::from_millis(1600));
    }

    /// Escape handling: close the top-most transient UI. Returns true if something closed.
    pub fn close_transient(&mut self) -> bool {
        if self.palette.is_open() {
            self.palette.close();
        } else if self.omnibox.is_open() {
            self.omnibox.close();
        } else if self.bookmark_editor.is_some() {
            self.bookmark_editor = None;
        } else if self.find.open {
            self.find.open = false;
            self.find.query.clear();
        } else if self.settings.is_some() {
            self.settings = None;
        } else if self.library.is_some() {
            self.library = None;
        } else {
            return false;
        }
        true
    }

    pub fn has_modal(&self) -> bool {
        self.palette.is_open() || !self.auth_prompts.is_empty()
    }

    /// Whether a pointer at `p` (physical px) is over chrome drawn on top of the page.
    pub fn blocks_content_at(&self, ctx: &egui::Context, p_px: Pos2, ppp_hint: f32) -> bool {
        let _ = ppp_hint;
        if self.has_modal() {
            return true;
        }
        let ppp = ctx.pixels_per_point();
        let p = Pos2::new(p_px.x / ppp, p_px.y / ppp);
        if self.overlay_rects.iter().any(|r| r.contains(p)) {
            return true;
        }
        match ctx.layer_id_at(p) {
            Some(layer) => layer.order != egui::Order::Background,
            None => false,
        }
    }
}

/// Draw the whole chrome for one frame.
pub fn draw(
    root: &mut egui::Ui,
    w: &mut BrowserWindow,
    env: &mut Env,
    offscreen: &Rc<OffscreenRenderingContext>,
    cmds: &mut Vec<Cmd>,
) {
    let ctx = root.ctx().clone();
    let pal = Palette::get(&ctx);
    w.chrome.overlay_rects.clear();

    // Async results that arrived since last frame.
    if let Some(res) = w.chrome.find.result_slot.lock().take() {
        w.chrome.find.result = res;
    }
    w.chrome.omnibox.poll_suggestions();

    let fullscreen_content = w.fullscreen_by_page;
    let app = env.profile.settings.appearance.clone();
    let vertical = app.tab_layout == ferrosurf_core::settings::TabLayout::Vertical;

    if !fullscreen_content {
        if !vertical {
            egui::Panel::top("tabstrip")
                .frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin {
                    left: 6,
                    right: 6,
                    top: 6,
                    bottom: 0,
                }))
                .show_separator_line(false)
                .show(root, |ui| tabs::tab_strip(ui, w, env, cmds));
        }
        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(pal.toolbar)
                    .inner_margin(egui::Margin::symmetric(8, 5)),
            )
            .show_separator_line(false)
            .show(root, |ui| toolbar::toolbar(ui, w, env, cmds));
        if app.show_bookmarks_bar {
            egui::Panel::top("bookmarks_bar")
                .frame(
                    egui::Frame::new()
                        .fill(pal.toolbar)
                        .inner_margin(egui::Margin {
                            left: 8,
                            right: 8,
                            top: 0,
                            bottom: 4,
                        }),
                )
                .show_separator_line(false)
                .show(root, |ui| toolbar::bookmarks_bar(ui, w, env, cmds));
        }
        // Thin separator under the toolbar.
        egui::Panel::top("toolbar_sep")
            .exact_size(1.0)
            .frame(egui::Frame::new().fill(pal.border))
            .show_separator_line(false)
            .show(root, |_| {});
        if vertical {
            egui::Panel::left("vtabs")
                .resizable(true)
                .default_size(230.0)
                .size_range(56.0..=420.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.bg)
                        .inner_margin(egui::Margin::same(6)),
                )
                .show(root, |ui| tabs::vertical_tabs(ui, w, env, cmds));
        }
        if w.chrome.assistant_open {
            egui::Panel::right("assistant")
                .resizable(true)
                .default_size(380.0)
                .size_range(280.0..=720.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.toolbar)
                        .inner_margin(egui::Margin::same(10)),
                )
                .show(root, |ui| assistant::panel(ui, w, env, cmds));
        }
        if let Some(tab) = w.chrome.library {
            egui::Panel::right("library")
                .resizable(true)
                .default_size(400.0)
                .size_range(300.0..=760.0)
                .frame(
                    egui::Frame::new()
                        .fill(pal.toolbar)
                        .inner_margin(egui::Margin::same(10)),
                )
                .show(root, |ui| library::panel(ui, tab, w, env, cmds));
        }
    }

    // Content area: the Servo framebuffer is blitted here.
    egui::CentralPanel::no_frame().show(root, |ui| {
        let rect = ui.max_rect();
        w.set_content_rect(rect);
        content_area(ui, rect, w, env, offscreen, cmds);
    });

    // Floating chrome.
    if w.chrome.find.open {
        find::find_bar(&ctx, w, cmds);
    }
    omnibox::dropdown(&ctx, w, env, cmds);
    dialogs::show_all(&ctx, w, env, cmds);
    if w.chrome.settings.is_some() {
        settings::window(&ctx, w, env, cmds);
    }
    palette::show(&ctx, w, env, cmds);
    mcp_approval(&ctx, env, cmds, &pal);
    toasts(&ctx, env, &pal);
}

/// Servo's "blit the offscreen framebuffer into the window" callback.
type BlitToParent = dyn Fn(&glow::Context, euclid::default::Rect<i32>) + Send + Sync;

fn content_area(
    ui: &mut egui::Ui,
    rect: Rect,
    w: &mut BrowserWindow,
    env: &mut Env,
    offscreen: &Rc<OffscreenRenderingContext>,
    cmds: &mut Vec<Cmd>,
) {
    let pal = Palette::get(ui.ctx());
    let tab_state = w.active_tab().map(|t| {
        (
            t.webview.is_some(),
            t.crashed.clone(),
            t.status_text.clone(),
            t.zoom,
            t.id,
        )
    });
    let Some((live, crashed, status, zoom, tab_id)) = tab_state else {
        ui.painter().rect_filled(rect, 0.0, pal.bg);
        return;
    };

    if let (true, None) = (live, &crashed) {
        if let Some(render) = offscreen.render_to_parent_callback() {
            let render: Arc<BlitToParent> = Arc::from(render);
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
        } else {
            ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
        }
    } else if let Some(reason) = crashed {
        ui.painter().rect_filled(rect, 0.0, pal.bg);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(40.0)), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(rect.height() * 0.25);
                ui.label(
                    egui::RichText::new(icon::SMILEY_SAD)
                        .size(56.0)
                        .color(pal.text_dim),
                );
                ui.heading("Esta pestaña ha dejado de funcionar");
                ui.label(egui::RichText::new(reason).color(pal.text_dim).small());
                ui.add_space(12.0);
                if widgets::accent_button(ui, &format!("{}  Recargar", icon::ARROW_CLOCKWISE))
                    .clicked()
                {
                    cmds.push(Cmd::ReloadTab(tab_id));
                }
            });
        });
    } else {
        ui.painter().rect_filled(rect, 0.0, pal.bg);
    }

    // Status bubble (link previews).
    if env.profile.settings.appearance.show_status_bubble
        && let Some(text) = status.filter(|s| !s.is_empty())
    {
        let galley = ui.painter().layout_no_wrap(
            ferrosurf_core::util::pretty_url(&text),
            egui::FontId::proportional(12.0),
            pal.text,
        );
        let size = galley.size() + egui::vec2(16.0, 8.0);
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + 6.0, rect.bottom() - size.y - 6.0),
            size.min(egui::vec2(rect.width() * 0.6, 40.0)),
        );
        ui.painter().rect(
            r,
            6.0,
            pal.toolbar,
            egui::Stroke::new(1.0, pal.border),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .with_clip_rect(r)
            .galley(r.min + egui::vec2(8.0, 4.0), galley, pal.text);
    }

    // Zoom indicator.
    if w.chrome
        .zoom_bubble_until
        .is_some_and(|t| t > Instant::now())
    {
        let text = format!("{}  {:.0}%", icon::MAGNIFYING_GLASS_PLUS, zoom * 100.0);
        let r = Rect::from_center_size(
            Pos2::new(rect.right() - 70.0, rect.top() + 26.0),
            egui::vec2(110.0, 32.0),
        );
        ui.painter().rect(
            r,
            8.0,
            pal.toolbar,
            egui::Stroke::new(1.0, pal.border),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(14.0),
            pal.text,
        );
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        w.chrome.overlay_rects.push(r);
    }

    dialogs::doorhangers(ui, rect, w, env, cmds);
}

fn mcp_approval(ctx: &egui::Context, env: &mut Env, cmds: &mut Vec<Cmd>, pal: &Palette) {
    let Some(first) = env.shared.mcp_approvals.first() else {
        return;
    };
    let desc = first.description.clone();
    let mut decision: Option<(bool, bool)> = None;
    egui::Modal::new(Id::new("mcp-approval"))
        .frame(
            egui::Frame::new()
                .fill(pal.toolbar)
                .stroke(egui::Stroke::new(1.0, pal.border))
                .corner_radius(14)
                .inner_margin(egui::Margin::same(18)),
        )
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}  Un agente MCP quiere actuar",
                    icon::PLUGS_CONNECTED
                ))
                .heading(),
            );
            ui.add_space(6.0);
            ui.label(format!("Solicitud: {desc}"));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if widgets::accent_button(ui, "Permitir").clicked() {
                    decision = Some((true, false));
                }
                if widgets::subtle_button(ui, "Permitir durante esta sesión").clicked() {
                    decision = Some((true, true));
                }
                if widgets::subtle_button(ui, "Denegar").clicked() {
                    decision = Some((false, false));
                }
            });
        });
    if let Some((allow, always)) = decision {
        cmds.push(Cmd::App(crate::window::AppCmd::ResolveMcpApproval {
            allow,
            always,
        }));
    }
}

fn toasts(ctx: &egui::Context, env: &mut Env, pal: &Palette) {
    env.shared
        .toasts
        .retain(|(_, t)| t.elapsed() < Duration::from_secs(4));
    if env.shared.toasts.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("toasts"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_max_width(screen.width() * 0.6);
            for (msg, _) in &env.shared.toasts {
                egui::Frame::new()
                    .fill(if pal.dark {
                        Color32::from_rgb(0x2b, 0x2f, 0x38)
                    } else {
                        Color32::from_rgb(0x26, 0x29, 0x30)
                    })
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 9))
                    .shadow(ctx.global_style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(msg).color(Color32::from_rgb(0xf2, 0xf3, 0xf5)),
                        );
                    });
                ui.add_space(6.0);
            }
        });
    ctx.request_repaint_after(Duration::from_millis(250));
}

use std::rc::Rc;
