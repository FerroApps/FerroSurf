//! Library side panel: bookmarks, history and downloads.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Margin, Pos2, Rect, Response, RichText, Sense, Ui, vec2};
use egui_phosphor::regular as icon;
use ferrosurf_core::bookmarks::{OTHER_ID, TOOLBAR_ID};
use ferrosurf_core::downloads::{DownloadItem, DownloadManager, DownloadState};
use ferrosurf_core::{Bookmark, BookmarkId, BookmarkNode};
use time::{OffsetDateTime, UtcOffset};

use super::widgets;
use super::{LibraryTab, Palette};
use crate::window::{BrowserWindow, Cmd, Disposition, Env};

const BM_ROW_H: f32 = 44.0;
const FOLDER_ROW_H: f32 = 32.0;
const HIST_ROW_H: f32 = 40.0;

/// Drag & drop payload for bookmark rows (a newtype so it cannot clash with other payloads).
struct BmDrag(BookmarkId);

struct BookmarkEdit {
    id: BookmarkId,
    title: String,
    url: String,
    tags: String,
    focus: bool,
}

struct HistRow {
    day: String,
    time: String,
    url: String,
    title: String,
    host: String,
}

#[derive(Default)]
pub struct LibraryState {
    search: String,
    last_tab: Option<LibraryTab>,
    collapsed: HashSet<BookmarkId>,
    selected_folder: Option<BookmarkId>,
    editing: Option<BookmarkEdit>,
    renaming: Option<(BookmarkId, String)>,
    rename_focus: bool,
    hist: Vec<HistRow>,
    hist_key: String,
    hist_fetched: Option<Instant>,
    hist_dirty: bool,
}

enum Op {
    Open(String, Disposition),
    OpenAll(BookmarkId),
    Delete(BookmarkId),
    /// `index` refers to the position in the target folder before the node is removed.
    Move {
        id: BookmarkId,
        parent: BookmarkId,
        index: Option<usize>,
    },
    NewFolder(BookmarkId),
    SaveEdit {
        id: BookmarkId,
        title: String,
        url: String,
        tags: Vec<String>,
    },
    Rename {
        id: BookmarkId,
        title: String,
    },
    AddCurrent {
        title: String,
        url: String,
    },
    Import,
    Export,
}

pub fn panel(
    ui: &mut egui::Ui,
    tab: LibraryTab,
    w: &mut BrowserWindow,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
) {
    let mut st = std::mem::take(&mut w.chrome.library_state);
    if st.last_tab != Some(tab) {
        st.search.clear();
        st.last_tab = Some(tab);
        st.editing = None;
        st.renaming = None;
        st.hist_dirty = true;
    }
    let pal = Palette::get(ui.ctx());

    header(ui, tab, w, &pal, &mut st);

    match tab {
        LibraryTab::Bookmarks => bookmarks_view(ui, w, env, cmds, &mut st, &pal),
        LibraryTab::History => history_view(ui, env, cmds, &mut st, &pal),
        LibraryTab::Downloads => downloads_view(ui, env, &mut st, &pal),
    }

    drag_ghost(ui.ctx(), env, &pal);
    w.chrome.library_state = st;
}

// ---------------------------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------------------------

fn header(
    ui: &mut Ui,
    tab: LibraryTab,
    w: &mut BrowserWindow,
    pal: &Palette,
    st: &mut LibraryState,
) {
    let h = ui.spacing().interact_size.y.max(28.0);
    ui.horizontal(|ui| {
        let seg_w = (ui.available_width() - h - ui.spacing().item_spacing.x).max(120.0);
        let (rect, _) = ui.allocate_exact_size(vec2(seg_w, h), Sense::hover());
        ui.painter().rect_filled(rect, 9, pal.field);
        let tabs = [
            (LibraryTab::Bookmarks, "Marcadores"),
            (LibraryTab::History, "Historial"),
            (LibraryTab::Downloads, "Descargas"),
        ];
        let sw = (rect.width() - 4.0) / tabs.len() as f32;
        for (i, (t, label)) in tabs.iter().enumerate() {
            let r = Rect::from_min_size(
                rect.min + vec2(2.0 + i as f32 * sw, 2.0),
                vec2(sw, rect.height() - 4.0),
            );
            let resp = ui.interact(r, ui.id().with(("lib-seg", i)), Sense::click());
            let active = *t == tab;
            let color = if active {
                pal.on_accent()
            } else if resp.hovered() {
                pal.text
            } else {
                pal.text_dim
            };
            if active {
                ui.painter().rect_filled(r, 7, pal.accent);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 7, pal.tab_hover);
            }
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                label,
                FontId::proportional(13.0),
                color,
            );
            if resp.clicked() && !active {
                w.chrome.library = Some(*t);
            }
        }
        if widgets::icon_button(ui, icon::X, "Cerrar panel", true).clicked() {
            w.chrome.library = None;
        }
    });
    ui.add_space(6.0);
    let hint = match tab {
        LibraryTab::Bookmarks => "Buscar en marcadores",
        LibraryTab::History => "Buscar en el historial",
        LibraryTab::Downloads => "Buscar en descargas",
    };
    ui.horizontal(|ui| {
        let clear_w = if st.search.is_empty() {
            0.0
        } else {
            h + ui.spacing().item_spacing.x
        };
        ui.add(
            egui::TextEdit::singleline(&mut st.search)
                .hint_text(format!("{}  {hint}", icon::MAGNIFYING_GLASS))
                .desired_width((ui.available_width() - clear_w).max(60.0))
                .margin(vec2(10.0, 7.0)),
        );
        if !st.search.is_empty()
            && widgets::icon_button(ui, icon::X, "Borrar búsqueda", true).clicked()
        {
            st.search.clear();
        }
    });
    ui.add_space(6.0);
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn row_rect(ui: &mut Ui, height: f32, sense: Sense, pal: &Palette) -> (Rect, Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    if resp.hovered() && ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect.shrink2(vec2(0.0, 1.0)), 7, pal.tab_hover);
    }
    (rect, resp)
}

/// Paint single-line text that is truncated with an ellipsis at `right`.
fn paint_ellipsized(ui: &Ui, pos: Pos2, right: f32, text: &str, font: FontId, color: Color32) {
    let width = (right - pos.x).max(8.0);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let galley: Arc<egui::Galley> = ui.painter().layout_job(job);
    ui.painter().galley(pos, galley, color);
}

fn empty_state(ui: &mut Ui, pal: &Palette, glyph: &str, title: &str, subtitle: &str) {
    ui.add_space(56.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(glyph)
                .size(40.0)
                .color(pal.text_dim.gamma_multiply(0.7)),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(title).strong().size(15.0));
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(subtitle).small().color(pal.text_dim)).wrap());
    });
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

fn open_url(cmds: &mut Vec<Cmd>, url: &str, disposition: Disposition) {
    match url::Url::parse(url) {
        Ok(url) => cmds.push(Cmd::OpenUrl { url, disposition }),
        Err(_) => cmds.push(Cmd::Navigate {
            input: url.to_string(),
            disposition,
        }),
    }
}

fn click_disposition(ui: &Ui, resp: &Response) -> Option<Disposition> {
    if resp.middle_clicked() {
        Some(Disposition::NewBackgroundTab)
    } else if resp.clicked() {
        if ui.input(|i| i.modifiers.command) {
            Some(Disposition::NewBackgroundTab)
        } else {
            Some(Disposition::CurrentTab)
        }
    } else {
        None
    }
}

fn drag_ghost(ctx: &egui::Context, env: &Env, pal: &Palette) {
    let Some(id) = egui::DragAndDrop::payload::<BmDrag>(ctx).map(|p| p.0) else {
        return;
    };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    let (Some(pos), Some(node)) = (ctx.pointer_latest_pos(), env.profile.bookmarks.get(id)) else {
        return;
    };
    let glyph = if node.is_folder() {
        icon::FOLDER
    } else {
        icon::BOOKMARK_SIMPLE
    };
    let title = node.title().to_string();
    egui::Area::new(egui::Id::new("lib-dnd-ghost"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + vec2(14.0, 10.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(pal.accent)
                .corner_radius(8)
                .inner_margin(Margin::symmetric(10, 5))
                .shadow(ctx.global_style().visuals.popup_shadow)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("{glyph}  {title}"))
                            .color(pal.on_accent())
                            .strong(),
                    );
                });
        });
}

// ---------------------------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------------------------

struct Tree<'a> {
    pal: Palette,
    bm: &'a ferrosurf_core::BookmarkStore,
    folders: Vec<(BookmarkId, String)>,
    ops: Vec<Op>,
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Before,
    Into,
    After,
}

fn root_title(id: BookmarkId, title: &str) -> String {
    match id {
        TOOLBAR_ID => "Barra de marcadores".into(),
        OTHER_ID => "Otros marcadores".into(),
        _ => title.to_string(),
    }
}

fn is_root(id: BookmarkId) -> bool {
    id == TOOLBAR_ID || id == OTHER_ID
}

fn bookmarks_view(
    ui: &mut Ui,
    w: &mut BrowserWindow,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
    st: &mut LibraryState,
    pal: &Palette,
) {
    let current = w.active_tab().and_then(|t| {
        t.url.as_ref().map(|u| {
            let title = if t.title.is_empty() {
                ferrosurf_core::util::pretty_url(u.as_str())
            } else {
                t.title.clone()
            };
            (u.to_string(), title)
        })
    });
    let mut ops: Vec<Op> = Vec::new();

    ui.horizontal_wrapped(|ui| {
        if widgets::subtle_button(ui, &format!("{}  Carpeta", icon::PLUS))
            .on_hover_text("Nueva carpeta")
            .clicked()
        {
            let parent = st
                .selected_folder
                .filter(|id| {
                    env.profile
                        .bookmarks
                        .get(*id)
                        .is_some_and(|n| n.is_folder())
                })
                .unwrap_or(OTHER_ID);
            ops.push(Op::NewFolder(parent));
        }
        ui.add_enabled_ui(current.is_some(), |ui| {
            if widgets::subtle_button(
                ui,
                &format!("{}  Añadir página actual", icon::BOOKMARK_SIMPLE),
            )
            .clicked()
            {
                if let Some((url, title)) = &current {
                    ops.push(Op::AddCurrent {
                        title: title.clone(),
                        url: url.clone(),
                    });
                }
            }
        });
        ui.menu_button(icon::DOTS_THREE, |ui| {
            if ui
                .button(format!("{}  Importar desde HTML…", icon::DOWNLOAD_SIMPLE))
                .clicked()
            {
                ops.push(Op::Import);
                ui.close();
            }
            if ui
                .button(format!("{}  Exportar a HTML…", icon::UPLOAD_SIMPLE))
                .clicked()
            {
                ops.push(Op::Export);
                ui.close();
            }
        });
    });
    ui.add_space(4.0);

    let query = st.search.trim().to_string();
    {
        let mut tree = Tree {
            pal: *pal,
            bm: &env.profile.bookmarks,
            folders: env.profile.bookmarks.folders(),
            ops: Vec::new(),
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !query.is_empty() {
                    let hits = tree.bm.search(&query, 100);
                    if hits.is_empty() {
                        empty_state(
                            ui,
                            pal,
                            icon::MAGNIFYING_GLASS,
                            "Sin resultados",
                            &format!("Ningún marcador coincide con «{query}»."),
                        );
                    }
                    for (_, b) in hits {
                        bookmark_row(ui, b, 0, None, &mut tree, st);
                    }
                } else if tree.bm.is_empty()
                    && tree.bm.roots().iter().all(|r| r.children().is_empty())
                {
                    empty_state(
                        ui,
                        pal,
                        icon::BOOKMARK_SIMPLE,
                        "Aún no hay marcadores",
                        "Pulsa «Añadir página actual» o importa un archivo HTML.",
                    );
                } else {
                    for root in tree.bm.roots() {
                        node(ui, root, None, 0, &mut tree, st);
                    }
                }
                ui.add_space(12.0);
            });
        ops.append(&mut tree.ops);
    }
    apply_ops(ops, env, cmds, st);
}

fn node(
    ui: &mut Ui,
    n: &BookmarkNode,
    parent: Option<(BookmarkId, usize)>,
    depth: usize,
    t: &mut Tree,
    st: &mut LibraryState,
) {
    match n {
        BookmarkNode::Bookmark(b) => bookmark_row(ui, b, depth, parent, t, st),
        BookmarkNode::Folder {
            id,
            title,
            children,
            ..
        } => {
            folder_row(ui, *id, title, children.len(), parent, depth, t, st);
            if !st.collapsed.contains(id) {
                if children.is_empty() {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    let x = rect.left() + 12.0 + (depth + 1) as f32 * 14.0 + 22.0;
                    ui.painter().text(
                        Pos2::new(x, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Carpeta vacía",
                        FontId::proportional(12.0),
                        t.pal.text_dim.gamma_multiply(0.8),
                    );
                }
                for (i, ch) in children.iter().enumerate() {
                    node(ui, ch, Some((*id, i)), depth + 1, t, st);
                }
            }
        }
    }
}

fn drop_zone(ui: &Ui, rect: Rect, is_folder: bool, root: bool) -> Zone {
    let y = ui
        .input(|i| i.pointer.hover_pos())
        .map_or(rect.center().y, |p| p.y);
    let frac = ((y - rect.top()) / rect.height()).clamp(0.0, 1.0);
    if is_folder {
        if root || frac >= 0.28 {
            Zone::Into
        } else {
            Zone::Before
        }
    } else if frac < 0.5 {
        Zone::Before
    } else {
        Zone::After
    }
}

/// Draw the drop indicator and report a completed drop on this row.
#[allow(clippy::too_many_arguments)]
fn handle_drop(
    ui: &Ui,
    resp: &Response,
    rect: Rect,
    me: BookmarkId,
    is_folder: bool,
    parent: Option<(BookmarkId, usize)>,
    t: &mut Tree,
) {
    let Some(dragged) = resp.dnd_hover_payload::<BmDrag>().map(|p| p.0) else {
        return;
    };
    if dragged == me {
        return;
    }
    let mut zone = drop_zone(ui, rect, is_folder, is_root(me));
    // Rows without a known parent (search results) can only be dropped "into" folders.
    if parent.is_none() && zone != Zone::Into {
        return;
    }
    if !is_folder && zone == Zone::Into {
        zone = Zone::After;
    }
    let accent = t.pal.accent;
    match zone {
        Zone::Into => {
            ui.painter().rect(
                rect.shrink2(vec2(0.0, 1.0)),
                7,
                accent.gamma_multiply(0.14),
                egui::Stroke::new(1.5, accent),
                egui::StrokeKind::Inside,
            );
        }
        Zone::Before => {
            ui.painter().line_segment(
                [
                    rect.left_top() + vec2(4.0, 0.5),
                    rect.right_top() + vec2(-4.0, 0.5),
                ],
                egui::Stroke::new(2.0, accent),
            );
        }
        Zone::After => {
            ui.painter().line_segment(
                [
                    rect.left_bottom() + vec2(4.0, -0.5),
                    rect.right_bottom() + vec2(-4.0, -0.5),
                ],
                egui::Stroke::new(2.0, accent),
            );
        }
    }
    if resp.dnd_release_payload::<BmDrag>().is_some() {
        match zone {
            Zone::Into => t.ops.push(Op::Move {
                id: dragged,
                parent: me,
                index: None,
            }),
            Zone::Before => {
                if let Some((p, i)) = parent {
                    t.ops.push(Op::Move {
                        id: dragged,
                        parent: p,
                        index: Some(i),
                    });
                }
            }
            Zone::After => {
                if let Some((p, i)) = parent {
                    t.ops.push(Op::Move {
                        id: dragged,
                        parent: p,
                        index: Some(i + 1),
                    });
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn folder_row(
    ui: &mut Ui,
    id: BookmarkId,
    title: &str,
    count: usize,
    parent: Option<(BookmarkId, usize)>,
    depth: usize,
    t: &mut Tree,
    st: &mut LibraryState,
) {
    let pal = t.pal;
    let display = root_title(id, title);
    let indent = depth as f32 * 14.0;

    if st.renaming.as_ref().is_some_and(|(rid, _)| *rid == id) {
        let mut commit = false;
        let mut cancel = false;
        let focus = std::mem::take(&mut st.rename_focus);
        if let Some((_, buf)) = st.renaming.as_mut() {
            ui.horizontal(|ui| {
                ui.add_space(indent + 8.0);
                ui.label(RichText::new(icon::FOLDER).color(pal.accent));
                let r = ui.add(
                    egui::TextEdit::singleline(buf)
                        .desired_width(f32::INFINITY)
                        .margin(vec2(8.0, 5.0)),
                );
                if focus {
                    r.request_focus();
                }
                if r.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    } else {
                        commit = true;
                    }
                }
            });
        }
        if commit {
            if let Some((rid, buf)) = st.renaming.take() {
                t.ops.push(Op::Rename {
                    id: rid,
                    title: buf,
                });
            }
        } else if cancel {
            st.renaming = None;
        }
        return;
    }

    let (rect, resp) = row_rect(ui, FOLDER_ROW_H, Sense::click_and_drag(), &pal);
    let collapsed = st.collapsed.contains(&id);
    let selected = st.selected_folder == Some(id);
    if ui.is_rect_visible(rect) {
        let cx = rect.left() + 10.0 + indent;
        let cy = rect.center().y;
        let caret = if collapsed {
            icon::CARET_RIGHT
        } else {
            icon::CARET_DOWN
        };
        ui.painter().text(
            Pos2::new(cx + 5.0, cy),
            egui::Align2::CENTER_CENTER,
            caret,
            FontId::proportional(12.0),
            pal.text_dim,
        );
        let glyph = if collapsed {
            icon::FOLDER
        } else {
            icon::FOLDER_OPEN
        };
        ui.painter().text(
            Pos2::new(cx + 26.0, cy),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(17.0),
            pal.accent,
        );
        let count_text = count.to_string();
        let count_galley =
            ui.painter()
                .layout_no_wrap(count_text, FontId::proportional(11.5), pal.text_dim);
        let right = rect.right() - 10.0 - count_galley.size().x;
        ui.painter().galley(
            Pos2::new(
                rect.right() - 10.0 - count_galley.size().x,
                cy - count_galley.size().y / 2.0,
            ),
            count_galley,
            pal.text_dim,
        );
        let color = if selected { pal.accent } else { pal.text };
        paint_ellipsized(
            ui,
            Pos2::new(cx + 42.0, cy - 8.5),
            right - 8.0,
            &display,
            FontId::proportional(13.5),
            color,
        );
    }
    if !is_root(id) {
        resp.dnd_set_drag_payload(BmDrag(id));
    }
    handle_drop(ui, &resp, rect, id, true, parent, t);

    if resp.clicked() {
        st.selected_folder = Some(id);
        if collapsed {
            st.collapsed.remove(&id);
        } else {
            st.collapsed.insert(id);
        }
    }
    resp.context_menu(|ui| {
        if ui
            .button(format!("{}  Abrir todo", icon::ARROW_SQUARE_OUT))
            .clicked()
        {
            t.ops.push(Op::OpenAll(id));
            ui.close();
        }
        if ui
            .button(format!("{}  Nueva carpeta", icon::FOLDER_SIMPLE_PLUS))
            .clicked()
        {
            t.ops.push(Op::NewFolder(id));
            ui.close();
        }
        if !is_root(id) {
            if ui
                .button(format!("{}  Renombrar", icon::PENCIL_SIMPLE))
                .clicked()
            {
                st.renaming = Some((id, title.to_string()));
                st.rename_focus = true;
                ui.close();
            }
            ui.separator();
            if ui
                .button(RichText::new(format!("{}  Eliminar", icon::TRASH)).color(pal.danger))
                .clicked()
            {
                t.ops.push(Op::Delete(id));
                ui.close();
            }
        }
    });
}

fn bookmark_row(
    ui: &mut Ui,
    b: &Bookmark,
    depth: usize,
    parent: Option<(BookmarkId, usize)>,
    t: &mut Tree,
    st: &mut LibraryState,
) {
    let pal = t.pal;
    let indent = depth as f32 * 14.0;

    if st.editing.as_ref().is_some_and(|e| e.id == b.id) {
        bookmark_editor(ui, indent, t, st);
        return;
    }

    let (rect, resp) = row_rect(ui, BM_ROW_H, Sense::click_and_drag(), &pal);
    if ui.is_rect_visible(rect) {
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.left() + 26.0 + indent, rect.center().y),
            vec2(18.0, 18.0),
        );
        widgets::paint_site_icon(ui.painter(), icon_rect, None, &b.title, &b.url);
        let left = icon_rect.right() + 10.0;
        let right = rect.right() - 8.0;
        let title = if b.title.is_empty() { &b.url } else { &b.title };
        paint_ellipsized(
            ui,
            Pos2::new(left, rect.top() + 6.0),
            right,
            title,
            FontId::proportional(13.5),
            pal.text,
        );
        let mut sub = host_of(&b.url);
        if !b.tags.is_empty() {
            sub.push_str("  ·  ");
            sub.push_str(
                &b.tags
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        paint_ellipsized(
            ui,
            Pos2::new(left, rect.top() + 24.0),
            right,
            &sub,
            FontId::proportional(11.5),
            pal.text_dim,
        );
    }
    resp.dnd_set_drag_payload(BmDrag(b.id));
    handle_drop(ui, &resp, rect, b.id, false, parent, t);
    let resp = resp.on_hover_text(format!("{}\n{}", b.title, b.url));

    if let Some(d) = click_disposition(ui, &resp) {
        t.ops.push(Op::Open(b.url.clone(), d));
    }
    resp.context_menu(|ui| {
        if ui
            .button(format!(
                "{}  Abrir en pestaña nueva",
                icon::ARROW_SQUARE_OUT
            ))
            .clicked()
        {
            t.ops
                .push(Op::Open(b.url.clone(), Disposition::NewForegroundTab));
            ui.close();
        }
        if ui
            .button(format!("{}  Editar", icon::PENCIL_SIMPLE))
            .clicked()
        {
            st.editing = Some(BookmarkEdit {
                id: b.id,
                title: b.title.clone(),
                url: b.url.clone(),
                tags: b.tags.join(", "),
                focus: true,
            });
            ui.close();
        }
        ui.menu_button(format!("{}  Mover a carpeta", icon::FOLDER), |ui| {
            for (fid, path) in &t.folders {
                let path = if is_root(*fid) {
                    root_title(*fid, path)
                } else {
                    path.clone()
                };
                if ui.button(path).clicked() {
                    t.ops.push(Op::Move {
                        id: b.id,
                        parent: *fid,
                        index: None,
                    });
                    ui.close();
                }
            }
        });
        if ui
            .button(format!("{}  Copiar enlace", icon::LINK_SIMPLE))
            .clicked()
        {
            ui.ctx().copy_text(b.url.clone());
            ui.close();
        }
        ui.separator();
        if ui
            .button(RichText::new(format!("{}  Eliminar", icon::TRASH)).color(pal.danger))
            .clicked()
        {
            t.ops.push(Op::Delete(b.id));
            ui.close();
        }
    });
}

fn bookmark_editor(ui: &mut Ui, indent: f32, t: &mut Tree, st: &mut LibraryState) {
    let pal = t.pal;
    let mut save = false;
    let mut cancel = false;
    let Some(ed) = st.editing.as_mut() else {
        return;
    };
    let left = (indent as i8).clamp(0, 100);
    egui::Frame::new()
        .fill(pal.field)
        .corner_radius(10)
        .inner_margin(Margin::same(10))
        .outer_margin(Margin {
            left,
            right: 0,
            top: 2,
            bottom: 4,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut enter = false;
            ui.label(RichText::new("Título").small().color(pal.text_dim));
            let r = ui.add(egui::TextEdit::singleline(&mut ed.title).desired_width(f32::INFINITY));
            if ed.focus {
                r.request_focus();
                ed.focus = false;
            }
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(2.0);
            ui.label(RichText::new("URL").small().color(pal.text_dim));
            let r = ui.add(egui::TextEdit::singleline(&mut ed.url).desired_width(f32::INFINITY));
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(2.0);
            ui.label(
                RichText::new("Etiquetas (separadas por comas)")
                    .small()
                    .color(pal.text_dim),
            );
            let r = ui.add(egui::TextEdit::singleline(&mut ed.tags).desired_width(f32::INFINITY));
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if widgets::accent_button(ui, "Guardar").clicked() || enter {
                    save = true;
                }
                if widgets::subtle_button(ui, "Cancelar").clicked() {
                    cancel = true;
                }
            });
        });
    if save {
        if let Some(ed) = st.editing.take() {
            let tags = ed
                .tags
                .split(',')
                .map(|s| s.trim().trim_start_matches('#').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            t.ops.push(Op::SaveEdit {
                id: ed.id,
                title: ed.title.trim().to_string(),
                url: ed.url.trim().to_string(),
                tags,
            });
        }
    } else if cancel {
        st.editing = None;
    }
}

fn find_parent(n: &BookmarkNode, id: BookmarkId) -> Option<(BookmarkId, usize)> {
    if let BookmarkNode::Folder {
        id: pid, children, ..
    } = n
    {
        for (i, c) in children.iter().enumerate() {
            if c.id() == id {
                return Some((*pid, i));
            }
            if let Some(r) = find_parent(c, id) {
                return Some(r);
            }
        }
    }
    None
}

fn collect_urls(n: &BookmarkNode, out: &mut Vec<String>) {
    match n {
        BookmarkNode::Bookmark(b) => out.push(b.url.clone()),
        BookmarkNode::Folder { children, .. } => children.iter().for_each(|c| collect_urls(c, out)),
    }
}

fn apply_ops(ops: Vec<Op>, env: &mut Env, cmds: &mut Vec<Cmd>, st: &mut LibraryState) {
    for op in ops {
        match op {
            Op::Open(url, d) => open_url(cmds, &url, d),
            Op::OpenAll(id) => {
                let mut urls = Vec::new();
                if let Some(n) = env.profile.bookmarks.get(id) {
                    collect_urls(n, &mut urls);
                }
                if urls.is_empty() {
                    env.shared.toast("La carpeta no contiene marcadores");
                }
                for (i, u) in urls.iter().enumerate() {
                    let d = if i == 0 {
                        Disposition::NewForegroundTab
                    } else {
                        Disposition::NewBackgroundTab
                    };
                    open_url(cmds, u, d);
                }
            }
            Op::Delete(id) => {
                let folder = env.profile.bookmarks.get(id).is_some_and(|n| n.is_folder());
                match env.profile.bookmarks.remove(id) {
                    Ok(_) => env.shared.toast(if folder {
                        "Carpeta eliminada"
                    } else {
                        "Marcador eliminado"
                    }),
                    Err(e) => env.shared.toast(&format!("No se pudo eliminar: {e}")),
                }
            }
            Op::Move { id, parent, index } => {
                let mut index = index;
                for root in env.profile.bookmarks.roots() {
                    if let Some((p, old)) = find_parent(root, id) {
                        if p == parent {
                            if let Some(ix) = index {
                                if old < ix {
                                    index = Some(ix - 1);
                                }
                            }
                        }
                        break;
                    }
                }
                match env.profile.bookmarks.move_to(id, parent, index) {
                    Ok(()) => {
                        st.collapsed.remove(&parent);
                    }
                    Err(e) => env.shared.toast(&format!("No se pudo mover: {e}")),
                }
            }
            Op::NewFolder(parent) => {
                match env.profile.bookmarks.add_folder(parent, "Nueva carpeta") {
                    Ok(fid) => {
                        st.collapsed.remove(&parent);
                        st.renaming = Some((fid, "Nueva carpeta".into()));
                        st.rename_focus = true;
                        st.selected_folder = Some(fid);
                    }
                    Err(e) => env
                        .shared
                        .toast(&format!("No se pudo crear la carpeta: {e}")),
                }
            }
            Op::SaveEdit {
                id,
                title,
                url,
                tags,
            } => {
                if url.is_empty() {
                    env.shared.toast("La URL no puede estar vacía");
                } else if let Err(e) = env.profile.bookmarks.update(id, &title, &url, tags) {
                    env.shared.toast(&format!("No se pudo guardar: {e}"));
                }
            }
            Op::Rename { id, title } => {
                let title = title.trim();
                if !title.is_empty() {
                    if let Err(e) = env.profile.bookmarks.rename(id, title) {
                        env.shared.toast(&format!("No se pudo renombrar: {e}"));
                    }
                }
            }
            Op::AddCurrent { title, url } => {
                if env.profile.bookmarks.is_bookmarked(&url) {
                    env.shared.toast("Esta página ya está en marcadores");
                    continue;
                }
                let parent = st
                    .selected_folder
                    .filter(|id| {
                        env.profile
                            .bookmarks
                            .get(*id)
                            .is_some_and(|n| n.is_folder())
                    })
                    .unwrap_or(TOOLBAR_ID);
                match env.profile.bookmarks.add(parent, &title, &url) {
                    Ok(id) => {
                        st.collapsed.remove(&parent);
                        st.editing = Some(BookmarkEdit {
                            id,
                            title,
                            url,
                            tags: String::new(),
                            focus: true,
                        });
                        env.shared.toast("Página añadida a marcadores");
                    }
                    Err(e) => env.shared.toast(&format!("No se pudo añadir: {e}")),
                }
            }
            Op::Import => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Marcadores HTML", &["html", "htm"])
                    .pick_file();
                if let Some(path) = picked {
                    match std::fs::read_to_string(&path) {
                        Ok(html) => match env.profile.bookmarks.import_html(&html, "Importados") {
                            Ok(n) => env.shared.toast(&format!("{n} marcadores importados")),
                            Err(e) => env.shared.toast(&format!("No se pudo importar: {e}")),
                        },
                        Err(e) => env
                            .shared
                            .toast(&format!("No se pudo leer el archivo: {e}")),
                    }
                }
            }
            Op::Export => {
                let picked = rfd::FileDialog::new()
                    .set_file_name("marcadores.html")
                    .add_filter("Marcadores HTML", &["html"])
                    .save_file();
                if let Some(path) = picked {
                    match std::fs::write(&path, env.profile.bookmarks.export_html()) {
                        Ok(()) => env.shared.toast("Marcadores exportados"),
                        Err(e) => env.shared.toast(&format!("No se pudo exportar: {e}")),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------------

fn local_offset() -> UtcOffset {
    static OFFSET: OnceLock<UtcOffset> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        if let Ok(o) = UtcOffset::current_local_offset() {
            return o;
        }
        // `time` refuses to read the local offset in multi-threaded processes on Unix;
        // ask `date` once instead.
        #[cfg(unix)]
        if let Ok(out) = std::process::Command::new("date").arg("+%z").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1i8 } else { 1 };
                if let (Ok(h), Ok(m)) = (s[1..3].parse::<i8>(), s[3..5].parse::<i8>()) {
                    if let Ok(o) = UtcOffset::from_hms(sign * h, sign * m, 0) {
                        return o;
                    }
                }
            }
        }
        UtcOffset::UTC
    })
}

fn to_local(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(local_offset())
}

fn day_label(date: time::Date, today: time::Date) -> String {
    let diff = (today - date).whole_days();
    match diff {
        0 => "Hoy".into(),
        1 => "Ayer".into(),
        2..=6 => match date.weekday() {
            time::Weekday::Monday => "Lunes",
            time::Weekday::Tuesday => "Martes",
            time::Weekday::Wednesday => "Miércoles",
            time::Weekday::Thursday => "Jueves",
            time::Weekday::Friday => "Viernes",
            time::Weekday::Saturday => "Sábado",
            time::Weekday::Sunday => "Domingo",
        }
        .into(),
        _ => {
            const MONTHS: [&str; 12] = [
                "enero",
                "febrero",
                "marzo",
                "abril",
                "mayo",
                "junio",
                "julio",
                "agosto",
                "septiembre",
                "octubre",
                "noviembre",
                "diciembre",
            ];
            let m = MONTHS[(u8::from(date.month()) - 1) as usize];
            if date.year() == today.year() {
                format!("{} de {m}", date.day())
            } else {
                format!("{} de {m} de {}", date.day(), date.year())
            }
        }
    }
}

fn refresh_history(env: &Env, st: &mut LibraryState, filter: &str) {
    let today = to_local(ferrosurf_core::now_ms()).date();
    st.hist = env
        .profile
        .history
        .visits(filter, None, 300)
        .unwrap_or_default()
        .into_iter()
        .map(|v| {
            let dt = to_local(v.visit_ms);
            HistRow {
                day: day_label(dt.date(), today),
                time: format!("{:02}:{:02}", dt.hour(), dt.minute()),
                host: host_of(&v.url),
                title: if v.title.trim().is_empty() {
                    ferrosurf_core::util::pretty_url(&v.url)
                } else {
                    v.title
                },
                url: v.url,
            }
        })
        .collect();
    st.hist_key = filter.to_string();
    st.hist_fetched = Some(Instant::now());
    st.hist_dirty = false;
}

enum Clear {
    Hour,
    Today,
    All,
}

fn history_view(
    ui: &mut Ui,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
    st: &mut LibraryState,
    pal: &Palette,
) {
    let filter = st.search.trim().to_string();
    let stale = st
        .hist_fetched
        .is_none_or(|t| t.elapsed() > Duration::from_secs(3));
    if st.hist_dirty || stale || st.hist_key != filter {
        refresh_history(env, st, &filter);
    }
    ui.ctx().request_repaint_after(Duration::from_secs(3));

    let mut clear: Option<Clear> = None;
    ui.horizontal(|ui| {
        ui.menu_button(format!("{}  Borrar historial…", icon::TRASH), |ui| {
            if ui.button("Última hora").clicked() {
                clear = Some(Clear::Hour);
                ui.close();
            }
            if ui.button("Hoy").clicked() {
                clear = Some(Clear::Today);
                ui.close();
            }
            ui.separator();
            if ui
                .button(RichText::new("Todo el historial").color(pal.danger))
                .clicked()
            {
                clear = Some(Clear::All);
                ui.close();
            }
        });
        ui.label(
            RichText::new(format!("{} visitas", st.hist.len()))
                .small()
                .color(pal.text_dim),
        );
    });
    ui.add_space(4.0);

    if let Some(c) = clear {
        let now = ferrosurf_core::now_ms();
        let h = &env.profile.history;
        let res = match c {
            Clear::Hour => h.delete_range(now - 3_600_000, now + 1000),
            Clear::Today => {
                let start = to_local(now)
                    .replace_time(time::Time::MIDNIGHT)
                    .unix_timestamp()
                    * 1000;
                h.delete_range(start, now + 1000)
            }
            Clear::All => h.clear(),
        };
        match res {
            Ok(()) => env.shared.toast("Historial borrado"),
            Err(e) => env
                .shared
                .toast(&format!("No se pudo borrar el historial: {e}")),
        }
        st.hist_dirty = true;
    }

    let mut delete_url: Option<String> = None;
    let mut new_filter: Option<String> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if st.hist.is_empty() {
                if filter.is_empty() {
                    let sub = if env.profile.settings.privacy.remember_history {
                        "Las páginas que visites aparecerán aquí."
                    } else {
                        "El historial está desactivado en Ajustes > Privacidad."
                    };
                    empty_state(
                        ui,
                        pal,
                        icon::CLOCK_COUNTER_CLOCKWISE,
                        "El historial está vacío",
                        sub,
                    );
                } else {
                    empty_state(
                        ui,
                        pal,
                        icon::MAGNIFYING_GLASS,
                        "Sin resultados",
                        &format!("Nada en el historial coincide con «{filter}»."),
                    );
                }
                return;
            }
            let mut prev_day: Option<&str> = None;
            for row in &st.hist {
                if prev_day != Some(row.day.as_str()) {
                    ui.add_space(if prev_day.is_some() { 8.0 } else { 2.0 });
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                    ui.painter().text(
                        Pos2::new(rect.left() + 8.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        row.day.to_uppercase(),
                        FontId::proportional(11.0),
                        pal.text_dim,
                    );
                    prev_day = Some(row.day.as_str());
                }
                let (rect, resp) = row_rect(ui, HIST_ROW_H, Sense::click(), pal);
                if ui.is_rect_visible(rect) {
                    let cy = rect.center().y;
                    ui.painter().text(
                        Pos2::new(rect.left() + 8.0, cy),
                        egui::Align2::LEFT_CENTER,
                        &row.time,
                        FontId::monospace(11.5),
                        pal.text_dim,
                    );
                    let icon_rect =
                        Rect::from_center_size(Pos2::new(rect.left() + 64.0, cy), vec2(16.0, 16.0));
                    widgets::paint_site_icon(ui.painter(), icon_rect, None, &row.title, &row.url);
                    let left = icon_rect.right() + 10.0;
                    let right = rect.right() - 8.0;
                    paint_ellipsized(
                        ui,
                        Pos2::new(left, rect.top() + 4.5),
                        right,
                        &row.title,
                        FontId::proportional(13.5),
                        pal.text,
                    );
                    paint_ellipsized(
                        ui,
                        Pos2::new(left, rect.top() + 22.0),
                        right,
                        &row.host,
                        FontId::proportional(11.5),
                        pal.text_dim,
                    );
                }
                let resp = resp.on_hover_text(format!("{}\n{}", row.title, row.url));
                if let Some(d) = click_disposition(ui, &resp) {
                    open_url(cmds, &row.url, d);
                }
                resp.context_menu(|ui| {
                    if ui
                        .button(format!(
                            "{}  Abrir en pestaña nueva",
                            icon::ARROW_SQUARE_OUT
                        ))
                        .clicked()
                    {
                        open_url(cmds, &row.url, Disposition::NewForegroundTab);
                        ui.close();
                    }
                    if ui
                        .button(format!("{}  Más de este sitio", icon::MAGNIFYING_GLASS))
                        .clicked()
                    {
                        new_filter = Some(row.host.clone());
                        ui.close();
                    }
                    if ui
                        .button(format!("{}  Copiar enlace", icon::LINK_SIMPLE))
                        .clicked()
                    {
                        ui.ctx().copy_text(row.url.clone());
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button(
                            RichText::new(format!("{}  Eliminar del historial", icon::TRASH))
                                .color(pal.danger),
                        )
                        .clicked()
                    {
                        delete_url = Some(row.url.clone());
                        ui.close();
                    }
                });
            }
            ui.add_space(12.0);
        });

    if let Some(url) = delete_url {
        if let Err(e) = env.profile.history.delete_url(&url) {
            env.shared.toast(&format!("No se pudo eliminar: {e}"));
        }
        st.hist_dirty = true;
    }
    if let Some(f) = new_filter {
        st.search = f;
    }
}

// ---------------------------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------------------------

fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        return format!("{b} B");
    }
    let s = format!("{v:.1}");
    let s = s.strip_suffix(".0").unwrap_or(&s).replace('.', ",");
    format!("{s} {}", UNITS[i])
}

fn fmt_eta(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs.div_ceil(60))
    } else {
        format!("{} h {} min", secs / 3600, (secs % 3600) / 60)
    }
}

fn file_icon(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => icon::FILE_PDF,
        "zip" | "rar" | "7z" | "gz" | "tar" | "xz" | "bz2" | "zst" => icon::FILE_ZIP,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico" => {
            icon::FILE_IMAGE
        }
        "mp4" | "mkv" | "webm" | "mov" | "avi" => icon::FILE_VIDEO,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "opus" => icon::FILE_AUDIO,
        "txt" | "md" | "log" => icon::FILE_TEXT,
        "rs" | "js" | "py" | "html" | "css" | "json" | "c" | "cpp" | "h" | "sh" | "toml"
        | "xml" => icon::FILE_CODE,
        "doc" | "docx" | "odt" | "rtf" => icon::FILE_DOC,
        "xls" | "xlsx" | "csv" | "ods" => icon::FILE_XLS,
        "ppt" | "pptx" | "odp" => icon::FILE_PPT,
        "exe" | "msi" | "deb" | "rpm" | "dmg" | "appimage" | "apk" | "pkg" => icon::APP_WINDOW,
        _ => icon::FILE,
    }
}

fn state_text(it: &DownloadItem, file_exists: bool) -> String {
    let got = fmt_bytes(it.received_bytes);
    match &it.state {
        DownloadState::Queued => "En cola…".into(),
        DownloadState::InProgress => {
            let mut s = match it.total_bytes {
                Some(t) if t > 0 => format!("{got} de {}", fmt_bytes(t)),
                _ => got,
            };
            if it.bytes_per_sec > 1.0 {
                s.push_str(&format!(" · {}/s", fmt_bytes(it.bytes_per_sec as u64)));
                if let Some(t) = it.total_bytes.filter(|t| *t > it.received_bytes) {
                    let eta = ((t - it.received_bytes) as f64 / it.bytes_per_sec).ceil() as u64;
                    s.push_str(&format!(" · quedan {}", fmt_eta(eta)));
                }
            }
            s
        }
        DownloadState::Paused => match it.total_bytes {
            Some(t) if t > 0 => format!("En pausa · {got} de {}", fmt_bytes(t)),
            _ => format!("En pausa · {got}"),
        },
        DownloadState::Completed => {
            if file_exists {
                format!(
                    "Completada · {}",
                    fmt_bytes(it.total_bytes.unwrap_or(it.received_bytes))
                )
            } else {
                "El archivo se movió o se eliminó".into()
            }
        }
        DownloadState::Failed(e) => format!("Error: {e}"),
        DownloadState::Cancelled => "Cancelada".into(),
    }
}

fn downloads_view(ui: &mut Ui, env: &mut Env, st: &mut LibraryState, pal: &Palette) {
    env.shared.downloads_attention = false;
    let mut items = env.profile.downloads.items();
    items.sort_by(|a, b| b.started_ms.cmp(&a.started_ms).then(b.id.cmp(&a.id)));
    let active = items
        .iter()
        .filter(|i| i.state.is_active() || i.state == DownloadState::Paused)
        .count();
    let any_finished = items.iter().any(|i| i.state.is_finished());
    if items.iter().any(|i| i.state.is_active()) {
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
    let q = st.search.trim().to_lowercase();
    if !q.is_empty() {
        items.retain(|i| {
            i.file_name.to_lowercase().contains(&q) || i.url.to_lowercase().contains(&q)
        });
    }

    ui.horizontal(|ui| {
        ui.add_enabled_ui(any_finished, |ui| {
            if widgets::subtle_button(ui, &format!("{}  Limpiar completadas", icon::BROOM))
                .clicked()
            {
                env.profile.downloads.clear_finished();
            }
        });
        if active > 0 {
            ui.label(
                RichText::new(format!("{active} en curso"))
                    .small()
                    .color(pal.accent),
            );
        }
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if items.is_empty() {
                if q.is_empty() {
                    empty_state(
                        ui,
                        pal,
                        icon::DOWNLOAD_SIMPLE,
                        "Sin descargas",
                        "Los archivos que descargues aparecerán aquí.",
                    );
                } else {
                    empty_state(
                        ui,
                        pal,
                        icon::MAGNIFYING_GLASS,
                        "Sin resultados",
                        &format!("Ninguna descarga coincide con «{}».", st.search.trim()),
                    );
                }
                return;
            }
            for it in &items {
                download_card(ui, env, it, pal);
                ui.add_space(6.0);
            }
            ui.add_space(12.0);
        });
}

fn download_card(ui: &mut Ui, env: &mut Env, it: &DownloadItem, pal: &Palette) {
    let finished_ok = it.state == DownloadState::Completed;
    let exists = !finished_ok || it.path.exists();
    egui::Frame::new()
        .fill(pal.field)
        .corner_radius(10)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
                let tint = match &it.state {
                    DownloadState::Failed(_) | DownloadState::Cancelled => pal.danger,
                    _ => pal.accent,
                };
                ui.painter().rect_filled(r, 9, tint.gamma_multiply(0.16));
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    file_icon(&it.file_name),
                    FontId::proportional(21.0),
                    tint,
                );

                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(&it.file_name).strong()).truncate());
                    let color = match &it.state {
                        DownloadState::Failed(_) => pal.danger,
                        _ if !exists => pal.text_dim,
                        _ => pal.text_dim,
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(state_text(it, exists)).small().color(color),
                        )
                        .truncate(),
                    );
                    if matches!(
                        it.state,
                        DownloadState::InProgress | DownloadState::Paused | DownloadState::Queued
                    ) {
                        let frac = match it.total_bytes {
                            Some(t) if t > 0 => {
                                (it.received_bytes as f32 / t as f32).clamp(0.0, 1.0)
                            }
                            _ => 0.0,
                        };
                        let known = it.total_bytes.is_some_and(|t| t > 0);
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_height(6.0)
                                .corner_radius(3)
                                .fill(if it.state == DownloadState::Paused {
                                    pal.text_dim
                                } else {
                                    pal.accent
                                })
                                .animate(!known && it.state != DownloadState::Paused),
                        );
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        download_buttons(ui, env, it, exists);
                    });
                });
            });
        });
}

fn download_buttons(ui: &mut Ui, env: &mut Env, it: &DownloadItem, exists: bool) {
    let dm = &env.profile.downloads;
    match &it.state {
        DownloadState::Queued | DownloadState::InProgress => {
            if widgets::icon_button(ui, icon::PAUSE, "Pausar", true).clicked() {
                dm.pause(it.id);
            }
            if widgets::icon_button(ui, icon::X, "Cancelar", true).clicked() {
                dm.cancel(it.id);
            }
        }
        DownloadState::Paused => {
            if widgets::icon_button(ui, icon::PLAY, "Reanudar", true).clicked() {
                dm.resume(it.id);
            }
            if widgets::icon_button(ui, icon::X, "Cancelar", true).clicked() {
                dm.cancel(it.id);
            }
        }
        DownloadState::Completed => {
            if widgets::icon_button(ui, icon::ARROW_SQUARE_OUT, "Abrir archivo", exists).clicked() {
                if let Err(e) = DownloadManager::open_file(&it.path) {
                    env.shared
                        .toast(&format!("No se pudo abrir el archivo: {e}"));
                }
            }
            if widgets::icon_button(ui, icon::FOLDER_OPEN, "Mostrar en carpeta", true).clicked() {
                if let Err(e) = DownloadManager::show_in_folder(&it.path) {
                    env.shared
                        .toast(&format!("No se pudo mostrar la carpeta: {e}"));
                }
            }
            if widgets::icon_button(ui, icon::TRASH, "Quitar de la lista", true).clicked() {
                env.profile.downloads.remove(it.id);
            }
        }
        DownloadState::Failed(_) | DownloadState::Cancelled => {
            if widgets::icon_button(ui, icon::ARROW_CLOCKWISE, "Reintentar", true).clicked() {
                dm.retry(it.id);
            }
            if widgets::icon_button(ui, icon::TRASH, "Quitar de la lista", true).clicked() {
                env.profile.downloads.remove(it.id);
            }
        }
    }
}
