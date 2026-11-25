//! Find in page (implemented in JS since Servo has no native find API).

use egui::{Id, RichText, Vec2};
use egui_phosphor::regular as icon;

use super::Palette;
use crate::window::{BrowserWindow, Cmd};

/// Highlight all matches of `query` and scroll to the next/previous one.
/// Returns "current/total" as a string.
pub fn find_script(query: &str, forward: bool) -> String {
    let q = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(function(q, forward) {{
  const KEY = '__ferroFind';
  let st = window[KEY];
  if (!st || st.q !== q) {{
    // Clear previous marks.
    document.querySelectorAll('mark[data-ferro-find]').forEach(m => {{
      const p = m.parentNode; p.replaceChild(document.createTextNode(m.textContent), m); p.normalize();
    }});
    st = window[KEY] = {{ q: q, marks: [], idx: -1 }};
    if (!q) return '0/0';
    const lq = q.toLowerCase();
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {{
      acceptNode(n) {{
        const p = n.parentElement;
        if (!p || /^(SCRIPT|STYLE|NOSCRIPT|TEXTAREA|INPUT)$/.test(p.tagName)) return NodeFilter.FILTER_REJECT;
        return n.nodeValue.toLowerCase().includes(lq) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
      }}
    }});
    const nodes = []; let n; while ((n = walker.nextNode())) nodes.push(n);
    for (const node of nodes) {{
      let text = node.nodeValue, lower = text.toLowerCase(), pos = 0, i;
      const frag = document.createDocumentFragment();
      while ((i = lower.indexOf(lq, pos)) !== -1) {{
        frag.appendChild(document.createTextNode(text.slice(pos, i)));
        const m = document.createElement('mark');
        m.setAttribute('data-ferro-find', '');
        m.style.cssText = 'background:#ffd54a;color:#111;border-radius:2px;padding:0';
        m.textContent = text.slice(i, i + q.length);
        frag.appendChild(m); st.marks.push(m); pos = i + q.length;
      }}
      frag.appendChild(document.createTextNode(text.slice(pos)));
      node.parentNode.replaceChild(frag, node);
    }}
  }}
  if (!st.marks.length) return '0/0';
  if (st.idx >= 0) st.marks[st.idx].style.background = '#ffd54a';
  st.idx = forward ? (st.idx + 1) % st.marks.length : (st.idx - 1 + st.marks.length) % st.marks.length;
  const cur = st.marks[st.idx];
  cur.style.background = '#ff8a3d';
  cur.scrollIntoView({{ block: 'center', inline: 'nearest' }});
  return (st.idx + 1) + '/' + st.marks.length;
}})({q}, {forward})"#
    )
}

pub const CLEAR_SCRIPT: &str = r#"(function(){
  document.querySelectorAll('mark[data-ferro-find]').forEach(m => {
    const p = m.parentNode; p.replaceChild(document.createTextNode(m.textContent), m); p.normalize();
  });
  window.__ferroFind = null;
})()"#;

pub fn find_bar(ctx: &egui::Context, w: &mut BrowserWindow, cmds: &mut Vec<Cmd>) {
    let pal = Palette::get(ctx);
    let content = w.content_rect_px;
    let ppp = ctx.pixels_per_point();
    let pos = egui::pos2(content.right() / ppp - 380.0, content.top() / ppp + 8.0);
    let mut close = false;
    let area = egui::Area::new(Id::new("find-bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(pal.toolbar)
                .stroke(egui::Stroke::new(1.0, pal.border))
                .corner_radius(10)
                .shadow(ctx.global_style().visuals.popup_shadow)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::MAGNIFYING_GLASS).color(pal.text_dim));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut w.chrome.find.query)
                                .hint_text("Buscar en la página")
                                .desired_width(180.0),
                        );
                        if w.chrome.find.focus {
                            w.chrome.find.focus = false;
                            resp.request_focus();
                        }
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let shift = ui.input(|i| i.modifiers.shift);
                        if resp.changed() {
                            w.chrome.find.last_sent = w.chrome.find.query.clone();
                            cmds.push(Cmd::FindInPage { query: w.chrome.find.query.clone(), forward: true });
                        }
                        if enter {
                            cmds.push(Cmd::FindInPage { query: w.chrome.find.query.clone(), forward: !shift });
                            resp.request_focus();
                        }
                        ui.label(RichText::new(&w.chrome.find.result).small().color(pal.text_dim));
                        ui.allocate_space(Vec2::new(4.0, 0.0));
                        if ui.button(icon::CARET_UP).on_hover_text("Anterior (Shift+Enter)").clicked() {
                            cmds.push(Cmd::FindInPage { query: w.chrome.find.query.clone(), forward: false });
                        }
                        if ui.button(icon::CARET_DOWN).on_hover_text("Siguiente (Enter)").clicked() {
                            cmds.push(Cmd::FindInPage { query: w.chrome.find.query.clone(), forward: true });
                        }
                        if ui.button(icon::X).clicked() {
                            close = true;
                        }
                    });
                });
        });
    w.chrome.overlay_rects.push(area.response.rect);
    if close || !w.chrome.find.open {
        w.chrome.find.open = false;
        w.chrome.find.query.clear();
        w.chrome.find.result.clear();
        cmds.push(Cmd::ClearFind);
    }
}
