//! Built-in developer tools panel (F12): Elements, Console, Network, Storage and Performance.
//!
//! Everything is driven through `Cmd::DevtoolsEval` (async JS evaluation in the active page)
//! plus the data the tab already records (`console`, `network`). Nothing here blocks: scripts
//! are queued, their result slots are polled once per frame, and snapshots are cached until
//! the user (or a page change) asks for a refresh.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, FontId, Id, Key, Layout, Modifiers, Rect, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Ui, Vec2,
};
use egui_phosphor::regular as icon;
use serde_json::Value;

use crate::tab::{ConsoleEntry, TabId};
use crate::ui::theme::Palette;
use crate::window::{BrowserWindow, Cmd, Disposition, Env};

/// Where an async JS evaluation result lands (polled by the panel each frame).
pub type EvalSlot = Arc<parking_lot::Mutex<Option<Result<serde_json::Value, String>>>>;

const ROW_H: f32 = 17.0;
const INPUT_H: f32 = 40.0;
const PENDING_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// Page-side scripts
// ---------------------------------------------------------------------------------------------

/// JS serializer injected in front of console evaluations: DOM nodes become `<tag#id.class>`,
/// functions `ƒ name()`, cycles `[Circular]`, with depth and size limits.
const JS_SER: &str = r#"
function __ferroSer(v){
  var seen=[];
  function d(v,depth){
    if(v===undefined) return '«undefined»';
    if(v===null) return null;
    var t=typeof v;
    if(t==='number') return isFinite(v)?v:String(v);
    if(t==='string') return v.length>4000?v.slice(0,4000)+'…':v;
    if(t==='boolean') return v;
    if(t==='bigint') return v.toString()+'n';
    if(t==='symbol') return v.toString();
    if(t==='function') return 'ƒ '+(v.name||'')+'()';
    if(typeof Node!=='undefined' && v instanceof Node){
      if(v.nodeType===1){
        var c=(v.classList&&v.classList.length)?'.'+Array.prototype.join.call(v.classList,'.'):'';
        return '<'+v.tagName.toLowerCase()+(v.id?'#'+v.id:'')+c+'>';
      }
      return '#'+v.nodeName;
    }
    if(seen.indexOf(v)>=0) return '[Circular]';
    if(depth>6) return Array.isArray(v)?'Array('+v.length+')':'{…}';
    seen.push(v);
    var out;
    try{
      if(v instanceof Error){ out={name:v.name,message:v.message,stack:String(v.stack||'')}; }
      else if(v instanceof Date){ out=isNaN(v.getTime())?'Invalid Date':v.toISOString(); }
      else if(v instanceof RegExp){ out=String(v); }
      else if(Array.isArray(v)){
        out=[]; var n=Math.min(v.length,100);
        for(var i=0;i<n;i++) out.push(d(v[i],depth+1));
        if(v.length>n) out.push('… '+(v.length-n)+' más');
      } else {
        out={}; var ks=Object.keys(v); var m=Math.min(ks.length,100);
        for(var j=0;j<m;j++){ try{ out[ks[j]]=d(v[ks[j]],depth+1); }catch(e){ out[ks[j]]='[Error]'; } }
        if(ks.length>m) out['…']=(ks.length-m)+' más';
      }
    }catch(e){ out='[No serializable]'; }
    seen.pop();
    return out;
  }
  return d(v,0);
}
"#;

const JS_RESOLVE: &str = "var P=__PATH__;var n=document.documentElement;\
    for(var i=0;i<P.length;i++){n=n&&n.childNodes[P[i]];}";

const JS_SNAPSHOT: &str = r#"(function(){
  var count=0,MAXN=6000;
  function skip(n){return n.nodeType===1&&n.id&&n.id.indexOf('__ferro_dt_')===0;}
  function walk(n,i,d){
    count++;
    if(n.nodeType===3){var t=n.nodeValue.trim();if(!t)return null;
      return {i:i,k:3,x:t.length>200?t.slice(0,200)+'…':t};}
    if(n.nodeType===8){return {i:i,k:8,x:(n.nodeValue||'').slice(0,200)};}
    if(n.nodeType!==1||skip(n))return null;
    var o={i:i,k:1,t:n.tagName.toLowerCase(),a:[]};
    for(var j=0;j<n.attributes.length;j++){var at=n.attributes[j];
      o.a.push([at.name,at.value.length>300?at.value.slice(0,300)+'…':at.value]);}
    var cn=n.childNodes;
    if(d>=12||count>MAXN){o.m=cn.length;return o;}
    o.c=[];var lim=Math.min(cn.length,200);
    for(var k=0;k<lim;k++){var c=walk(cn[k],k,d+1);if(c)o.c.push(c);}
    if(cn.length>lim)o.m=cn.length-lim;
    return o;
  }
  var r=walk(document.documentElement,0,0);
  return JSON.stringify({root:r});
})()"#;

const JS_DETAIL: &str = r#"(function(){
  __RESOLVE__
  if(!n)return JSON.stringify({gone:true});
  if(n.nodeType!==1)return JSON.stringify({text:n.nodeValue||''});
  var cs=getComputedStyle(n),st=[];
  for(var i=0;i<cs.length&&i<600;i++){var k=cs[i];st.push([k,cs.getPropertyValue(k)]);}
  var a=[];for(var j=0;j<n.attributes.length;j++){a.push([n.attributes[j].name,n.attributes[j].value]);}
  function q(p,s){return ['Top','Right','Bottom','Left'].map(function(x){return parseFloat(cs[p+x+s])||0;});}
  var r=n.getBoundingClientRect();
  return JSON.stringify({attrs:a,styles:st,margin:q('margin',''),border:q('border','Width'),
    padding:q('padding',''),rect:[r.x,r.y,r.width,r.height]});
})()"#;

const JS_HIGHLIGHT: &str = r#"(function(){
  __RESOLVE__
  var o=document.getElementById('__ferro_dt_ov');
  if(!n||n.nodeType!==1){if(o)o.remove();return '1';}
  if(!o){
    o=document.createElement('div');o.id='__ferro_dt_ov';
    o.style.cssText='position:fixed;pointer-events:none;z-index:2147483647;'+
      'background:rgba(66,133,244,.35);border:1px solid rgba(66,133,244,.95);box-sizing:border-box;';
    var l=document.createElement('div');
    l.style.cssText='position:absolute;left:-1px;background:#1a73e8;color:#fff;'+
      'font:11px monospace;padding:1px 5px;white-space:nowrap;';
    o.appendChild(l);document.documentElement.appendChild(o);
  }
  var r=n.getBoundingClientRect();
  o.style.left=r.left+'px';o.style.top=r.top+'px';
  o.style.width=r.width+'px';o.style.height=r.height+'px';
  var l=o.firstChild;
  l.textContent=n.tagName.toLowerCase()+(n.id?'#'+n.id:'')+'  '+Math.round(r.width)+' × '+Math.round(r.height);
  if(r.top<20){l.style.top='auto';l.style.bottom='-18px';}else{l.style.bottom='auto';l.style.top='-18px';}
  window.__ferroHlT=Date.now();
  if(!window.__ferroHlWd){window.__ferroHlWd=setInterval(function(){
    if(Date.now()-window.__ferroHlT>2500){var e=document.getElementById('__ferro_dt_ov');
      if(e)e.remove();clearInterval(window.__ferroHlWd);window.__ferroHlWd=0;}},1000);}
  return '1';
})()"#;

const JS_UNHIGHLIGHT: &str = "(function(){var o=document.getElementById('__ferro_dt_ov');\
    if(o)o.remove();if(window.__ferroPickOff)window.__ferroPickOff();return '1';})()";

const JS_PICK_START: &str = r#"(function(){
  if(window.__ferroPickOff)window.__ferroPickOff();
  window.__ferroPicked=null;window.__ferroPickT=Date.now();
  var ov=document.createElement('div');ov.id='__ferro_dt_pick';
  ov.style.cssText='position:fixed;pointer-events:none;z-index:2147483647;'+
    'background:rgba(66,133,244,.35);border:1px solid rgba(66,133,244,.95);box-sizing:border-box;';
  document.documentElement.appendChild(ov);
  function pathOf(el){var p=[];while(el&&el!==document.documentElement){var par=el.parentNode;
    if(!par)return [];p.unshift(Array.prototype.indexOf.call(par.childNodes,el));el=par;}return p;}
  function mv(e){var el=e.target;if(!el||!el.getBoundingClientRect)return;
    var r=el.getBoundingClientRect();ov.style.left=r.left+'px';ov.style.top=r.top+'px';
    ov.style.width=r.width+'px';ov.style.height=r.height+'px';}
  function ck(e){e.preventDefault();e.stopPropagation();e.stopImmediatePropagation();
    window.__ferroPicked=pathOf(e.target);off();}
  function off(){document.removeEventListener('mousemove',mv,true);
    document.removeEventListener('click',ck,true);clearInterval(wd);ov.remove();
    window.__ferroPickOff=null;}
  var wd=setInterval(function(){if(Date.now()-window.__ferroPickT>3000)off();},1000);
  window.__ferroPickOff=off;
  document.addEventListener('mousemove',mv,true);document.addEventListener('click',ck,true);
  return '1';
})()"#;

const JS_PICK_POLL: &str = "(function(){window.__ferroPickT=Date.now();\
    return JSON.stringify(window.__ferroPicked||null);})()";


const JS_STORAGE: &str = r#"(function(){
  function g(n){try{return window[n];}catch(e){return null;}}
  function dump(s){if(!s)return null;try{var a=[];for(var i=0;i<s.length;i++){var k=s.key(i);
    a.push([k,s.getItem(k)]);}return a;}catch(e){return null;}}
  return JSON.stringify({local:dump(g('localStorage')),session:dump(g('sessionStorage'))});
})()"#;

const JS_METRICS: &str = r#"(function(){
  if(!window.__ferroFps){var s=window.__ferroFps={frames:0,t:performance.now(),fps:0};
    (function loop(){s.frames++;var n=performance.now();
      if(n-s.t>=1000){s.fps=Math.round(s.frames*1000/(n-s.t));s.frames=0;s.t=n;}
      requestAnimationFrame(loop);})();}
  var m=performance.memory;
  return JSON.stringify({dom:document.getElementsByTagName('*').length,
    heap:m?m.usedJSHeapSize:null,heapTotal:m?m.totalJSHeapSize:null,fps:window.__ferroFps.fps,
    ready:document.readyState,res:performance.getEntriesByType('resource').length});
})()"#;

const JS_PERF: &str = r#"(function(){
  var nav=performance.getEntriesByType('navigation')[0]||null;
  var res=performance.getEntriesByType('resource').slice(-1500).map(function(e){
    return [e.name,e.duration,e.transferSize||0];});
  return JSON.stringify({nav:nav?{dcl:nav.domContentLoadedEventEnd,load:nav.loadEventEnd,
    ttfb:nav.responseStart,size:nav.transferSize||0,dur:nav.duration,name:nav.name}:null,res:res});
})()"#;

fn path_json(path: &[usize]) -> String {
    serde_json::to_string(path).unwrap_or_else(|_| "[]".into())
}

fn js_with_path(template: &str, path: &[usize]) -> String {
    template
        .replace("__RESOLVE__", JS_RESOLVE)
        .replace("__PATH__", &path_json(path))
}

/// Wrap user code so statements and expressions both work and errors are returned, not thrown.
fn eval_wrapper(code: &str) -> String {
    let lit = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(function(){{{JS_SER}\
         try{{var r=(0,eval)({lit});return JSON.stringify({{ok:__ferroSer(r)}});}}\
         catch(e){{return JSON.stringify({{__error:String((e&&e.message)||e),\
         stack:String((e&&e.stack)||'')}});}}}})()"
    )
}

fn edit_script(path: &[usize], body: &str) -> String {
    format!(
        "(function(){{{}try{{if(!n)return JSON.stringify({{__error:'Nodo no encontrado'}});{body}\
         return JSON.stringify({{ok:true}});}}catch(e){{return JSON.stringify({{__error:String(e)}});}}}})()",
        JS_RESOLVE.replace("__PATH__", &path_json(path))
    )
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

// ---------------------------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------------------------

/// Last path segment of a URL (falls back to host, then to the URL itself).
pub(crate) fn url_name(url: &str) -> String {
    if url.starts_with("data:") {
        return "data:…".into();
    }
    match url::Url::parse(url) {
        Ok(u) => {
            let seg = u
                .path_segments()
                .and_then(|mut s| s.rfind(|p| !p.is_empty()))
                .map(str::to_string);
            seg.or_else(|| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.to_string())
        }
        Err(_) => url
            .rsplit('/')
            .find(|p| !p.is_empty())
            .unwrap_or(url)
            .to_string(),
    }
}

/// Host part of a URL, or an empty string.
pub(crate) fn url_domain(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum NetFilter {
    #[default]
    All,
    Doc,
    Js,
    Css,
    Img,
    Font,
    Xhr,
    Media,
    Other,
}

impl NetFilter {
    const ALL: [NetFilter; 9] = [
        NetFilter::All,
        NetFilter::Doc,
        NetFilter::Js,
        NetFilter::Css,
        NetFilter::Img,
        NetFilter::Font,
        NetFilter::Xhr,
        NetFilter::Media,
        NetFilter::Other,
    ];

    fn label(self) -> &'static str {
        match self {
            NetFilter::All => "Todo",
            NetFilter::Doc => "Doc",
            NetFilter::Js => "JS",
            NetFilter::Css => "CSS",
            NetFilter::Img => "Img",
            NetFilter::Font => "Fuentes",
            NetFilter::Xhr => "XHR",
            NetFilter::Media => "Medios",
            NetFilter::Other => "Otros",
        }
    }

    /// Category of a resource kind as recorded by the delegate.
    pub(crate) fn of_kind(kind: &str) -> NetFilter {
        match kind {
            "mainframe" | "subframe" | "document" => NetFilter::Doc,
            "script" => NetFilter::Js,
            "stylesheet" => NetFilter::Css,
            "image" => NetFilter::Img,
            "font" => NetFilter::Font,
            "xhr" | "fetch" => NetFilter::Xhr,
            "media" => NetFilter::Media,
            _ => NetFilter::Other,
        }
    }

    pub(crate) fn matches(self, kind: &str) -> bool {
        self == NetFilter::All || NetFilter::of_kind(kind) == self
    }
}

pub(crate) fn net_matches(url: &str, kind: &str, text: &str, filter: NetFilter) -> bool {
    filter.matches(kind) && (text.is_empty() || url.to_lowercase().contains(&text.to_lowercase()))
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "mainframe" => "doc",
        "subframe" => "iframe",
        "stylesheet" => "css",
        "websocket" => "ws",
        other => other,
    }
}

/// Toggle group of a console level (errors, warnings, info, logs); `None` for REPL entries.
pub(crate) fn level_group(level: &str) -> Option<usize> {
    match level {
        "error" => Some(0),
        "warn" => Some(1),
        "info" => Some(2),
        "log" | "debug" | "trace" => Some(3),
        _ => None,
    }
}

pub(crate) fn console_visible(level: &str, message: &str, filter: &str, hidden: [bool; 4]) -> bool {
    match level_group(level) {
        Some(g) => {
            !hidden[g]
                && (filter.is_empty() || message.to_lowercase().contains(&filter.to_lowercase()))
        }
        None => true,
    }
}

/// Marker the page-side serializer uses for `undefined`.
const UNDEFINED_MARK: &str = "«undefined»";

/// One-line display of a scalar (containers are summarized).
pub(crate) fn scalar_display(v: &Value) -> String {
    match v {
        Value::String(s) if s == UNDEFINED_MARK => "undefined".into(),
        Value::String(s) => format!("\"{s}\""),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("Array({})", a.len()),
        Value::Object(o) => format!("{{…}} ({})", o.len()),
    }
}

/// Compact single-line preview of a JSON value, truncated to `max` characters.
pub(crate) fn preview(v: &Value, max: usize) -> String {
    let s = match v {
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => scalar_display(v),
        _ => v.to_string(),
    };
    truncate_chars(&s, max)
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

pub(crate) fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

pub(crate) fn fmt_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let f = b as f64;
    if f >= K * K {
        format!("{:.1} MB", f / (K * K))
    } else if f >= K {
        format!("{:.1} kB", f / K)
    } else {
        format!("{b} B")
    }
}

/// Box-model number: zero shows as a dash, integers without decimals.
pub(crate) fn fmt_px(v: f32) -> String {
    if v.abs() < 0.005 {
        "-".into()
    } else if (v - v.round()).abs() < 0.005 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

fn fmt_clock(t: SystemTime) -> String {
    let mut dt = time::OffsetDateTime::from(t);
    if let Ok(off) = time::UtcOffset::current_local_offset() {
        dt = dt.to_offset(off);
    }
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.millisecond()
    )
}

/// Turn a JS evaluation result (which is normally a JSON string) into a `Value`.
fn parse_result(res: Result<Value, String>) -> Result<Value, String> {
    match res? {
        Value::String(s) => Ok(serde_json::from_str(&s).unwrap_or(Value::String(s))),
        v => Ok(v),
    }
}

fn pairs(v: &Value, key: &str) -> Vec<(String, String)> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some((
                        p.first()?.as_str()?.to_string(),
                        p.get(1).and_then(Value::as_str).unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn arr4(v: &Value, key: &str) -> [f32; 4] {
    let mut out = [0.0; 4];
    if let Some(a) = v.get(key).and_then(Value::as_array) {
        for (i, x) in a.iter().take(4).enumerate() {
            out[i] = x.as_f64().unwrap_or(0.0) as f32;
        }
    }
    out
}

fn style_matches(name: &str, value: &str, filter: &str) -> bool {
    filter.is_empty() || {
        let f = filter.to_lowercase();
        name.to_lowercase().contains(&f) || value.to_lowercase().contains(&f)
    }
}

// ---------------------------------------------------------------------------------------------
// DOM model
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum NodeKind {
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
}

#[derive(Clone, Debug)]
struct DomNode {
    /// Index in the parent's `childNodes` (stable path component).
    idx: usize,
    kind: NodeKind,
    children: Vec<DomNode>,
    /// Children not included in the snapshot (depth/size limits).
    more: usize,
}

impl DomNode {
    fn has_kids(&self) -> bool {
        matches!(self.kind, NodeKind::Element { .. })
            && (!self.children.is_empty() || self.more > 0)
    }
}

fn parse_dom(v: &Value) -> Option<DomNode> {
    let idx = v.get("i")?.as_u64()? as usize;
    let text = || v.get("x").and_then(Value::as_str).unwrap_or("").to_string();
    let kind = match v.get("k")?.as_u64()? {
        1 => NodeKind::Element {
            tag: v.get("t")?.as_str()?.to_string(),
            attrs: pairs(v, "a"),
        },
        3 => NodeKind::Text(text()),
        8 => NodeKind::Comment(text()),
        _ => return None,
    };
    let children = v
        .get("c")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_dom).collect())
        .unwrap_or_default();
    let more = v.get("m").and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(DomNode {
        idx,
        kind,
        children,
        more,
    })
}

struct Row<'a> {
    depth: usize,
    node: &'a DomNode,
    path: Vec<usize>,
}

fn flatten<'a>(
    node: &'a DomNode,
    depth: usize,
    path: &mut Vec<usize>,
    expanded: &HashSet<Vec<usize>>,
    out: &mut Vec<Row<'a>>,
) {
    out.push(Row {
        depth,
        node,
        path: path.clone(),
    });
    if node.has_kids() && expanded.contains(path.as_slice()) {
        for c in &node.children {
            path.push(c.idx);
            flatten(c, depth + 1, path, expanded, out);
            path.pop();
        }
    }
}

#[derive(Default)]
struct Detail {
    gone: bool,
    is_element: bool,
    text: String,
    attrs: Vec<(String, String)>,
    styles: Vec<(String, String)>,
    margin: [f32; 4],
    border: [f32; 4],
    padding: [f32; 4],
    rect: [f32; 4],
}

fn parse_detail(v: &Value) -> Detail {
    Detail {
        gone: v.get("gone").is_some(),
        is_element: v.get("attrs").is_some(),
        text: v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        attrs: pairs(v, "attrs"),
        styles: pairs(v, "styles"),
        margin: arr4(v, "margin"),
        border: arr4(v, "border"),
        padding: arr4(v, "padding"),
        rect: arr4(v, "rect"),
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Section {
    Elements,
    #[default]
    Console,
    Network,
    Storage,
    Performance,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Console,
    Dom,
    Detail,
    PickPoll,
    Storage,
    Metrics,
    Perf,
    Edit,
    /// Fire and forget (highlight, picker setup, storage mutations...).
    Misc,
}

struct Pending {
    kind: Kind,
    slot: EvalSlot,
    since: Instant,
}

struct StorageItem {
    session: bool,
    key: String,
    value: String,
    edit: String,
}

#[derive(Default)]
struct NavTiming {
    dcl: f64,
    load: f64,
    ttfb: f64,
    size: u64,
    dur: f64,
}

#[derive(Default)]
struct PerfData {
    nav: Option<NavTiming>,
    /// url -> (duration ms, transfer size)
    res: HashMap<String, (f64, u64)>,
    total_size: u64,
}

#[derive(Default)]
struct Metrics {
    dom: u64,
    heap: Option<u64>,
    heap_total: Option<u64>,
    fps: f64,
    ready: String,
    res: u64,
}

struct CookieRow {
    name: String,
    value: String,
    domain: String,
    path: String,
    expires: String,
    flags: String,
}

#[derive(Default)]
pub struct DevtoolsState {
    section: Section,
    page_key: Option<(TabId, String)>,
    pending: Vec<Pending>,
    // console
    filter: String,
    hidden: [bool; 4],
    input: String,
    history: Vec<String>,
    hist_pos: Option<usize>,
    focus_input: bool,
    // elements
    dom: Option<DomNode>,
    dom_error: Option<String>,
    dom_fetched: bool,
    dom_fetched_loading: bool,
    expanded: HashSet<Vec<usize>>,
    selected: Option<Vec<usize>>,
    detail: Option<Detail>,
    detail_requested: bool,
    style_filter: String,
    picking: bool,
    last_pick_poll: Option<Instant>,
    last_hl: Option<Vec<usize>>,
    hl_sent: Option<Instant>,
    scroll_to_selected: bool,
    text_edit: String,
    attr_edits: Vec<(String, String)>,
    new_attr: (String, String),
    // network
    net_filter: String,
    net_type: NetFilter,
    net_sel: Option<usize>,
    perf: PerfData,
    perf_at: Option<Instant>,
    perf_loading: bool,
    // storage
    cookies: Vec<CookieRow>,
    cookies_at: Option<Instant>,
    storage: Vec<StorageItem>,
    storage_error: Option<String>,
    storage_fetched: bool,
    new_key: String,
    new_val: String,
    // performance
    metrics: Option<Metrics>,
    metrics_at: Option<Instant>,
    fps_hist: VecDeque<f32>,
}

impl DevtoolsState {
    /// Forget everything tied to the previous page, keeping user preferences.
    fn reset_page(&mut self) {
        let keep = DevtoolsState {
            section: self.section,
            filter: std::mem::take(&mut self.filter),
            hidden: self.hidden,
            input: std::mem::take(&mut self.input),
            history: std::mem::take(&mut self.history),
            style_filter: std::mem::take(&mut self.style_filter),
            net_filter: std::mem::take(&mut self.net_filter),
            net_type: self.net_type,
            ..Default::default()
        };
        *self = keep;
    }

    fn send(&mut self, cmds: &mut Vec<Cmd>, kind: Kind, script: String) {
        let slot: EvalSlot = Arc::new(parking_lot::Mutex::new(None));
        cmds.push(Cmd::DevtoolsEval {
            script,
            slot: slot.clone(),
        });
        self.pending.push(Pending {
            kind,
            slot,
            since: Instant::now(),
        });
    }

    fn busy(&self, kind: Kind) -> bool {
        self.pending.iter().any(|p| p.kind == kind)
    }

    fn poll(&mut self, w: &mut BrowserWindow) {
        let mut done = Vec::new();
        self.pending.retain(|p| match p.slot.lock().take() {
            Some(res) => {
                done.push((p.kind, res));
                false
            }
            None => p.since.elapsed() < PENDING_TIMEOUT,
        });
        for (kind, res) in done {
            self.handle(kind, parse_result(res), w);
        }
    }

    fn handle(&mut self, kind: Kind, res: Result<Value, String>, w: &mut BrowserWindow) {
        match kind {
            Kind::Console => {
                let Some(tab) = w.active_tab_mut() else {
                    return;
                };
                match res {
                    Ok(v) => {
                        if let Some(err) = v.get("__error").and_then(Value::as_str) {
                            let stack = v.get("stack").and_then(Value::as_str).unwrap_or("");
                            let msg = if stack.trim().is_empty() {
                                format!("Uncaught {err}")
                            } else {
                                format!("Uncaught {err}\n{}", stack.trim_end())
                            };
                            tab.push_console("error", msg);
                        } else {
                            let ok = v.get("ok").cloned().unwrap_or(Value::Null);
                            tab.push_console("result", ok.to_string());
                        }
                    }
                    Err(e) => tab.push_console("error", e),
                }
            }
            Kind::Dom => match res {
                Ok(v) => {
                    self.dom = v.get("root").and_then(parse_dom);
                    self.dom_error = if self.dom.is_none() {
                        Some("No se pudo leer el DOM de la página".into())
                    } else {
                        None
                    };
                    if self.expanded.is_empty()
                        && let Some(root) = &self.dom
                    {
                        self.expanded.insert(Vec::new());
                        for c in &root.children {
                            self.expanded.insert(vec![c.idx]);
                        }
                    }
                    // Refresh the detail of the selected node after any DOM refresh.
                    self.detail_requested = false;
                }
                Err(e) => self.dom_error = Some(e),
            },
            Kind::Detail => {
                if let Ok(v) = res {
                    let d = parse_detail(&v);
                    self.attr_edits = d.attrs.clone();
                    self.text_edit = d.text.clone();
                    self.detail = Some(d);
                }
            }
            Kind::PickPoll => {
                if let Ok(Value::Array(items)) = res {
                    let path: Vec<usize> = items
                        .iter()
                        .filter_map(|x| x.as_u64().map(|n| n as usize))
                        .collect();
                    self.picking = false;
                    for i in 0..path.len() {
                        self.expanded.insert(path[..i].to_vec());
                    }
                    self.selected = Some(path);
                    self.detail = None;
                    self.detail_requested = false;
                    self.scroll_to_selected = true;
                    self.dom_fetched = false;
                }
            }
            Kind::Storage => match res {
                Ok(v) => {
                    self.storage.clear();
                    self.storage_error = None;
                    for (field, session) in [("local", false), ("session", true)] {
                        if let Some(a) = v.get(field).and_then(Value::as_array) {
                            for (key, value) in a.iter().filter_map(|p| {
                                let p = p.as_array()?;
                                Some((
                                    p.first()?.as_str()?.to_string(),
                                    p.get(1).and_then(Value::as_str).unwrap_or("").to_string(),
                                ))
                            }) {
                                self.storage.push(StorageItem {
                                    session,
                                    key,
                                    edit: value.clone(),
                                    value,
                                });
                            }
                        }
                    }
                }
                Err(e) => self.storage_error = Some(e),
            },
            Kind::Metrics => {
                if let Ok(v) = res {
                    let m = Metrics {
                        dom: v.get("dom").and_then(Value::as_u64).unwrap_or(0),
                        heap: v.get("heap").and_then(Value::as_u64),
                        heap_total: v.get("heapTotal").and_then(Value::as_u64),
                        fps: v.get("fps").and_then(Value::as_f64).unwrap_or(0.0),
                        ready: v
                            .get("ready")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        res: v.get("res").and_then(Value::as_u64).unwrap_or(0),
                    };
                    if self.fps_hist.len() >= 60 {
                        self.fps_hist.pop_front();
                    }
                    self.fps_hist.push_back(m.fps as f32);
                    self.metrics = Some(m);
                }
            }
            Kind::Perf => {
                if let Ok(v) = res {
                    let mut p = PerfData::default();
                    if let Some(n) = v.get("nav").filter(|n| !n.is_null()) {
                        let f = |k: &str| n.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                        p.nav = Some(NavTiming {
                            dcl: f("dcl"),
                            load: f("load"),
                            ttfb: f("ttfb"),
                            size: f("size") as u64,
                            dur: f("dur"),
                        });
                    }
                    if let Some(a) = v.get("res").and_then(Value::as_array) {
                        for r in a {
                            if let Some(r) = r.as_array()
                                && let Some(name) = r.first().and_then(Value::as_str)
                            {
                                let d = r.get(1).and_then(Value::as_f64).unwrap_or(0.0);
                                let s = r.get(2).and_then(Value::as_f64).unwrap_or(0.0) as u64;
                                p.total_size += s;
                                p.res.insert(name.to_string(), (d, s));
                            }
                        }
                    }
                    self.perf = p;
                }
            }
            Kind::Edit => {
                if let Ok(v) = &res
                    && v.get("__error").is_none()
                {
                    self.dom_fetched = false;
                    self.detail_requested = false;
                }
            }
            Kind::Misc => {}
        }
    }

    fn highlight(&mut self, cmds: &mut Vec<Cmd>, want: Option<Vec<usize>>) {
        let script = match &want {
            Some(p) => js_with_path(JS_HIGHLIGHT, p),
            None => JS_UNHIGHLIGHT.to_string(),
        };
        self.send(cmds, Kind::Misc, script);
        self.last_hl = want;
        self.hl_sent = Some(Instant::now());
    }

    fn clear_page_effects(&mut self, cmds: &mut Vec<Cmd>) {
        if self.last_hl.is_some() || self.picking {
            self.send(cmds, Kind::Misc, JS_UNHIGHLIGHT.to_string());
        }
        self.last_hl = None;
        self.picking = false;
    }

    fn select(&mut self, path: Vec<usize>) {
        self.selected = Some(path);
        self.detail = None;
        self.detail_requested = false;
    }
}

// ---------------------------------------------------------------------------------------------
// Small widgets
// ---------------------------------------------------------------------------------------------

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn small_icon(ui: &mut Ui, glyph: &str, tooltip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame(false))
        .on_hover_text(tooltip)
}

fn warn_color() -> Color32 {
    Color32::from_rgb(0xe5, 0xb4, 0x3c)
}

fn info_color() -> Color32 {
    Color32::from_rgb(0x5d, 0xb0, 0xd7)
}

struct SyntaxColors {
    tag: Color32,
    attr: Color32,
    value: Color32,
    comment: Color32,
}

fn syntax(pal: &Palette) -> SyntaxColors {
    if pal.dark {
        SyntaxColors {
            tag: Color32::from_rgb(0x5d, 0xb0, 0xd7),
            attr: Color32::from_rgb(0x9b, 0xbb, 0xdc),
            value: Color32::from_rgb(0xf2, 0x97, 0x66),
            comment: Color32::from_rgb(0x6a, 0x99, 0x55),
        }
    } else {
        SyntaxColors {
            tag: Color32::from_rgb(0x88, 0x12, 0x80),
            attr: Color32::from_rgb(0x99, 0x45, 0x00),
            value: Color32::from_rgb(0x1a, 0x1a, 0xa6),
            comment: Color32::from_rgb(0x23, 0x6e, 0x25),
        }
    }
}

/// One table row painted manually (cheap, fixed height, clipped cells).
fn table_row(
    ui: &mut Ui,
    pal: &Palette,
    fractions: &[f32],
    cells: &[(String, Color32)],
    selected: bool,
    header: bool,
) -> egui::Response {
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H + 2.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.35));
    } else if header {
        painter.rect_filled(rect, 0.0, pal.field);
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, pal.tab_hover);
    }
    let mut x = rect.left() + 6.0;
    for (i, (text, color)) in cells.iter().enumerate() {
        let w = width * fractions.get(i).copied().unwrap_or(0.1);
        let cell =
            Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(w - 6.0, rect.height()));
        painter.with_clip_rect(cell).text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            if header {
                FontId::proportional(11.0)
            } else {
                mono(11.0)
            },
            *color,
        );
        x += w;
    }
    resp
}

/// Collapsible JSON tree (egui `CollapsingHeader`), objects and arrays expandable.
fn json_tree(ui: &mut Ui, key: Option<&str>, v: &Value, pal: &Palette, depth: usize) {
    let sc = syntax(pal);
    let key_txt = key.map(|k| format!("{k}: ")).unwrap_or_default();
    match v {
        Value::Object(_) | Value::Array(_) => {
            let summary = match v {
                Value::Array(a) => format!("Array({})", a.len()),
                Value::Object(o) => format!("{{{}}}", o.len()),
                _ => unreachable!(),
            };
            let title = RichText::new(format!("{key_txt}{summary}"))
                .font(mono(11.0))
                .color(pal.text);
            egui::CollapsingHeader::new(title)
                .id_salt((key.unwrap_or("·"), depth))
                .default_open(depth == 0)
                .show(ui, |ui| match v {
                    Value::Object(o) => {
                        for (k, child) in o {
                            json_tree(ui, Some(k), child, pal, depth + 1);
                        }
                    }
                    Value::Array(a) => {
                        for (i, child) in a.iter().enumerate() {
                            json_tree(ui, Some(&i.to_string()), child, pal, depth + 1);
                        }
                    }
                    _ => {}
                });
        }
        _ => {
            let color = match v {
                Value::String(s) if s == UNDEFINED_MARK => pal.text_dim,
                Value::String(_) => sc.value,
                Value::Number(_) => info_color(),
                Value::Bool(_) => Color32::from_rgb(0xc5, 0x86, 0xc0),
                _ => pal.text_dim,
            };
            let mut job = LayoutJob::default();
            job.append(&key_txt, 0.0, TextFormat::simple(mono(11.0), sc.attr));
            job.append(
                &scalar_display(v),
                0.0,
                TextFormat::simple(mono(11.0), color),
            );
            ui.add(egui::Label::new(job).selectable(true));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------------------------

pub fn panel(ui: &mut egui::Ui, w: &mut BrowserWindow, env: &mut Env, cmds: &mut Vec<Cmd>) {
    // The state lives in `w.chrome`; take it out so the tab can be borrowed alongside.
    let mut st = std::mem::take(&mut w.chrome.devtools);
    run(&mut st, ui, w, env, cmds);
    w.chrome.devtools = st;
}

fn run(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    env: &mut Env,
    cmds: &mut Vec<Cmd>,
) {
    let pal = Palette::get(ui.ctx());
    let key = match w.active_tab() {
        Some(t) => (t.id, t.url_string()),
        None => {
            ui.add_space(12.0);
            ui.label(RichText::new("No hay ninguna pestaña activa").color(pal.text_dim));
            return;
        }
    };
    if st.page_key.as_ref() != Some(&key) {
        st.reset_page();
        st.page_key = Some(key);
    }
    st.poll(w);

    let mut refresh = false;
    let mut close = false;
    let prev = st.section;

    ui.spacing_mut().item_spacing = Vec2::new(6.0, 3.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        for (s, label) in [
            (Section::Elements, "Elementos"),
            (Section::Console, "Consola"),
            (Section::Network, "Red"),
            (Section::Storage, "Almacenamiento"),
            (Section::Performance, "Rendimiento"),
        ] {
            let sel = st.section == s;
            let resp = ui.add(
                egui::Label::new(RichText::new(label).size(12.0).color(if sel {
                    pal.text
                } else {
                    pal.text_dim
                }))
                .sense(Sense::click()),
            );
            if sel {
                ui.painter().hline(
                    resp.rect.x_range(),
                    resp.rect.bottom() + 2.0,
                    Stroke::new(2.0, pal.accent),
                );
            }
            if resp.clicked() {
                st.section = s;
            }
            ui.add_space(8.0);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            if small_icon(ui, icon::X, "Cerrar herramientas (F12)").clicked() {
                close = true;
            }
            ui.label(
                RichText::new(icon::SQUARE_HALF_BOTTOM)
                    .size(14.0)
                    .color(pal.text_dim),
            )
            .on_hover_text("Anclado abajo — arrastra el borde superior para redimensionar");
            if small_icon(ui, icon::ARROW_CLOCKWISE, "Actualizar").clicked() {
                refresh = true;
            }
        });
    });
    ui.add_space(2.0);
    ui.separator();

    if prev != st.section && prev == Section::Elements {
        st.clear_page_effects(cmds);
    }
    if close {
        st.clear_page_effects(cmds);
        w.chrome.devtools_open = false;
        return;
    }
    if refresh {
        match st.section {
            Section::Elements => {
                st.dom_fetched = false;
                st.detail_requested = false;
            }
            Section::Console => {}
            Section::Network => st.perf_at = None,
            Section::Storage => {
                st.storage_fetched = false;
                st.cookies_at = None;
            }
            Section::Performance => st.metrics_at = None,
        }
    }

    match st.section {
        Section::Console => console_section(st, ui, w, cmds, &pal),
        Section::Elements => elements_section(st, ui, w, cmds, &pal),
        Section::Network => network_section(st, ui, w, cmds, env, &pal),
        Section::Storage => storage_section(st, ui, w, cmds, env, &pal),
        Section::Performance => performance_section(st, ui, w, cmds, env, &pal),
    }

    if !st.pending.is_empty() {
        ui.ctx().request_repaint_after(Duration::from_millis(120));
    }
}

// ---------------------------------------------------------------------------------------------
// Console
// ---------------------------------------------------------------------------------------------

fn console_row(ui: &mut Ui, pal: &Palette, e: &ConsoleEntry, salt: usize) {
    let (glyph, color, tint) = match e.level.as_str() {
        "error" => (
            icon::X_CIRCLE,
            pal.danger,
            Some(pal.danger.gamma_multiply(0.14)),
        ),
        "warn" => (
            icon::WARNING,
            warn_color(),
            Some(warn_color().gamma_multiply(0.12)),
        ),
        "info" => (icon::INFO, info_color(), None),
        "input" => ("›", pal.accent, None),
        "result" => ("‹", pal.text_dim, None),
        "trace" | "debug" => ("·", pal.text_dim, None),
        _ => ("·", pal.text_dim, None),
    };
    let text_color = match e.level.as_str() {
        "error" => pal.danger,
        "warn" => warn_color(),
        "input" => pal.accent,
        "trace" | "debug" => pal.text_dim,
        _ => pal.text,
    };
    let mut frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 0));
    if let Some(t) = tint {
        frame = frame.fill(t);
    }
    let nanos = e
        .time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = (salt, nanos);
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).color(color).size(12.0));
            ui.label(
                RichText::new(fmt_clock(e.time))
                    .font(mono(10.0))
                    .color(pal.text_dim),
            );
            let msg = e.message.as_str();
            if e.level == "result" {
                match serde_json::from_str::<Value>(msg) {
                    Ok(v @ (Value::Object(_) | Value::Array(_))) => {
                        let title = RichText::new(preview(&v, 120))
                            .font(mono(11.0))
                            .color(pal.text);
                        egui::CollapsingHeader::new(title)
                            .id_salt(id)
                            .show(ui, |ui| json_tree(ui, None, &v, pal, 0));
                    }
                    Ok(v) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(scalar_display(&v))
                                    .font(mono(11.0))
                                    .color(pal.text),
                            )
                            .selectable(true),
                        );
                    }
                    Err(_) => {
                        ui.label(RichText::new(msg).font(mono(11.0)).color(pal.text));
                    }
                }
            } else if msg.contains('\n') || msg.chars().count() > 160 {
                let first = msg.lines().next().unwrap_or("");
                let title = RichText::new(truncate_chars(first, 160))
                    .font(mono(11.0))
                    .color(text_color);
                egui::CollapsingHeader::new(title)
                    .id_salt(id)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(msg).font(mono(11.0)).color(text_color))
                                .selectable(true),
                        );
                    });
            } else {
                ui.add(
                    egui::Label::new(RichText::new(msg).font(mono(11.0)).color(text_color))
                        .selectable(true)
                        .truncate(),
                );
            }
        });
    });
}

fn console_section(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    cmds: &mut Vec<Cmd>,
    pal: &Palette,
) {
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        if small_icon(ui, icon::PROHIBIT, "Limpiar consola").clicked() {
            clear = true;
        }
        ui.add(
            TextEdit::singleline(&mut st.filter)
                .hint_text("Filtrar")
                .desired_width(170.0)
                .font(mono(11.0)),
        );
        for (i, label) in ["Errores", "Avisos", "Info", "Logs"].iter().enumerate() {
            let mut on = !st.hidden[i];
            if ui
                .toggle_value(&mut on, RichText::new(*label).size(11.0))
                .changed()
            {
                st.hidden[i] = !on;
            }
        }
    });
    if clear && let Some(t) = w.active_tab_mut() {
        t.console.clear();
    }

    let list_h = (ui.available_height() - INPUT_H).max(30.0);
    if let Some(tab) = w.active_tab() {
        let idxs: Vec<usize> = tab
            .console
            .iter()
            .enumerate()
            .filter(|(_, e)| console_visible(&e.level, &e.message, &st.filter, st.hidden))
            .map(|(i, _)| i)
            .collect();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ScrollArea::vertical()
                .id_salt("dt_console_list")
                .max_height(list_h)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, ROW_H, idxs.len(), |ui, range| {
                    for r in range {
                        let i = idxs[r];
                        console_row(ui, pal, &tab.console[i], i);
                    }
                });
        });
    }

    // REPL
    ui.separator();
    let id = Id::new("ferro_dt_repl");
    let focused = ui.memory(|m| m.has_focus(id));
    let mut submit = false;
    let mut hist_move = 0i32;
    if focused {
        let single_line = !st.input.contains('\n');
        ui.input_mut(|i| {
            if !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter) {
                submit = true;
            }
            if single_line {
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    hist_move = -1;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    hist_move = 1;
                }
            }
        });
    }
    if hist_move != 0 && !st.history.is_empty() {
        let last = st.history.len() - 1;
        if hist_move < 0 {
            let p = st.hist_pos.map_or(last, |p| p.saturating_sub(1));
            st.hist_pos = Some(p);
            st.input = st.history[p].clone();
        } else if let Some(p) = st.hist_pos {
            if p < last {
                st.hist_pos = Some(p + 1);
                st.input = st.history[p + 1].clone();
            } else {
                st.hist_pos = None;
                st.input.clear();
            }
        }
    }
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new("›").color(pal.accent).font(mono(13.0)));
        ui.add(
            TextEdit::multiline(&mut st.input)
                .id(id)
                .font(mono(11.5))
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .hint_text("Ejecutar JavaScript…  (Enter ejecuta, Mayús+Enter nueva línea)"),
        );
    });
    if st.focus_input {
        ui.memory_mut(|m| m.request_focus(id));
        st.focus_input = false;
    }
    if submit {
        let code = st.input.trim().to_string();
        if !code.is_empty() {
            if let Some(t) = w.active_tab_mut() {
                t.push_console("input", code.clone());
            }
            if st.history.last() != Some(&code) {
                st.history.push(code.clone());
            }
            st.hist_pos = None;
            st.input.clear();
            st.send(cmds, Kind::Console, eval_wrapper(&code));
        }
        st.focus_input = true;
        ui.ctx().request_repaint();
    }
}

// ---------------------------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------------------------

fn row_job(row: &Row, expanded: bool, pal: &Palette) -> LayoutJob {
    let sc = syntax(pal);
    let f = mono(11.5);
    let mut job = LayoutJob::default();
    let mut put = |t: &str, c: Color32| job.append(t, 0.0, TextFormat::simple(f.clone(), c));
    match &row.node.kind {
        NodeKind::Element { tag, attrs } => {
            put("<", pal.text_dim);
            put(tag, sc.tag);
            for (k, v) in attrs {
                put(" ", pal.text_dim);
                put(k, sc.attr);
                put("=", pal.text_dim);
                put(&format!("\"{}\"", truncate_chars(v, 70)), sc.value);
            }
            put(">", pal.text_dim);
            if row.node.has_kids() && !expanded {
                put("…", pal.text_dim);
                put("</", pal.text_dim);
                put(tag, sc.tag);
                put(">", pal.text_dim);
            }
            if row.node.more > 0 {
                put(&format!("  (+{} más)", row.node.more), pal.text_dim);
            }
        }
        NodeKind::Text(t) => {
            put(&format!("\"{t}\""), pal.text);
        }
        NodeKind::Comment(t) => {
            put(&format!("<!--{t}-->"), sc.comment);
        }
    }
    job
}

fn elements_section(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    cmds: &mut Vec<Cmd>,
    pal: &Palette,
) {
    let loading = w.active_tab().is_some_and(|t| t.is_loading());
    // Fetch the DOM once per page, once more when loading finishes, and on explicit refresh.
    if (!st.dom_fetched || (st.dom_fetched_loading && !loading)) && !st.busy(Kind::Dom) {
        st.dom_fetched = true;
        st.dom_fetched_loading = loading;
        st.send(cmds, Kind::Dom, JS_SNAPSHOT.to_string());
    }
    if let Some(sel) = st.selected.clone()
        && !st.detail_requested
        && !st.busy(Kind::Detail)
    {
        st.detail_requested = true;
        st.send(cmds, Kind::Detail, js_with_path(JS_DETAIL, &sel));
    }
    if st.picking {
        let due = st
            .last_pick_poll
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(300));
        if due && !st.busy(Kind::PickPoll) {
            st.last_pick_poll = Some(Instant::now());
            st.send(cmds, Kind::PickPoll, JS_PICK_POLL.to_string());
        }
        ui.ctx().request_repaint_after(Duration::from_millis(300));
    }

    let total = ui.available_size();
    let side_w = (total.x * 0.38).clamp(240.0, 480.0).min(total.x * 0.6);
    let tree_w = (total.x - side_w - 10.0).max(100.0);
    let mut hovered: Option<Vec<usize>> = None;
    let mut to_select: Option<Vec<usize>> = None;
    let mut toggle: Option<Vec<usize>> = None;
    let mut start_pick = false;
    let mut stop_pick = false;

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(tree_w, total.y),
            Layout::top_down(Align::Min),
            |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let label =
                        RichText::new(format!("{} Seleccionar elemento", icon::CURSOR_CLICK))
                            .size(11.0);
                    let mut on = st.picking;
                    if ui.toggle_value(&mut on, label).clicked() {
                        if on {
                            start_pick = true;
                        } else {
                            stop_pick = true;
                        }
                    }
                    if let Some(e) = &st.dom_error {
                        ui.label(
                            RichText::new(truncate_chars(e, 80))
                                .size(11.0)
                                .color(pal.danger),
                        );
                    }
                });
                let h = (ui.available_height() - 2.0).max(40.0);
                let Some(root) = &st.dom else {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(if st.busy(Kind::Dom) {
                            "Cargando DOM…"
                        } else {
                            "Sin datos del DOM"
                        })
                        .color(pal.text_dim),
                    );
                    return;
                };
                let mut rows = Vec::new();
                flatten(root, 0, &mut Vec::new(), &st.expanded, &mut rows);
                let mut sa = ScrollArea::vertical()
                    .id_salt("dt_dom_tree")
                    .max_height(h)
                    .auto_shrink([false, false]);
                if st.scroll_to_selected {
                    if let Some(sel) = &st.selected
                        && let Some(i) = rows.iter().position(|r| &r.path == sel)
                    {
                        let off =
                            (i as f32 * (ROW_H + ui.spacing().item_spacing.y) - h / 2.0).max(0.0);
                        sa = sa.vertical_scroll_offset(off);
                    }
                    st.scroll_to_selected = false;
                }
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    sa.show_rows(ui, ROW_H, rows.len(), |ui, range| {
                        for r in &rows[range] {
                            let (rect, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width().max(tree_w), ROW_H),
                                Sense::click(),
                            );
                            let painter = ui.painter_at(rect);
                            let is_sel = st.selected.as_ref() == Some(&r.path);
                            if is_sel {
                                painter.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.35));
                            } else if resp.hovered() {
                                painter.rect_filled(rect, 0.0, pal.tab_hover);
                            }
                            let x = rect.left() + 4.0 + r.depth as f32 * 12.0;
                            let open = st.expanded.contains(&r.path);
                            if r.node.has_kids() {
                                painter.text(
                                    egui::pos2(x, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    if open {
                                        icon::CARET_DOWN
                                    } else {
                                        icon::CARET_RIGHT
                                    },
                                    FontId::proportional(10.0),
                                    pal.text_dim,
                                );
                            }
                            let galley = painter.layout_job(row_job(r, open, pal));
                            painter.galley(
                                egui::pos2(x + 13.0, rect.center().y - galley.size().y / 2.0),
                                galley,
                                pal.text,
                            );
                            if resp.hovered() {
                                hovered = Some(r.path.clone());
                            }
                            if resp.clicked() {
                                let on_arrow =
                                    resp.interact_pointer_pos().is_some_and(|p| p.x < x + 13.0);
                                if on_arrow && r.node.has_kids() {
                                    toggle = Some(r.path.clone());
                                } else {
                                    to_select = Some(r.path.clone());
                                }
                            }
                            if resp.double_clicked() && r.node.has_kids() {
                                toggle = Some(r.path.clone());
                            }
                        }
                    });
                });
            },
        );
        ui.separator();
        ui.allocate_ui_with_layout(
            Vec2::new(side_w, total.y),
            Layout::top_down(Align::Min),
            |ui| detail_pane(st, ui, cmds, pal),
        );
    });

    if let Some(p) = toggle
        && !st.expanded.remove(&p)
    {
        st.expanded.insert(p);
    }
    if let Some(p) = to_select {
        st.select(p);
    }
    if start_pick {
        st.picking = true;
        st.last_pick_poll = None;
        st.send(cmds, Kind::Misc, JS_PICK_START.to_string());
    }
    if stop_pick {
        st.picking = false;
        st.send(cmds, Kind::Misc, JS_UNHIGHLIGHT.to_string());
    }
    if st.picking {
        return;
    }

    // Page highlight: hovered row wins over the selection; re-sent periodically as keep-alive.
    let want = hovered.or_else(|| st.selected.clone());
    let stale = st
        .hl_sent
        .is_none_or(|t| t.elapsed() >= Duration::from_millis(1000));
    let changed = want != st.last_hl;
    let throttled = st
        .hl_sent
        .is_some_and(|t| t.elapsed() < Duration::from_millis(80));
    if (changed && !throttled) || (stale && want.is_some()) {
        st.highlight(cmds, want);
    } else if changed {
        ui.ctx().request_repaint_after(Duration::from_millis(90));
    }
    if st.last_hl.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(1000));
    }
}

fn box_model(ui: &mut Ui, d: &Detail) {
    let size = Vec2::new(250.0, 150.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let dark = Color32::from_rgb(0x20, 0x20, 0x20);
    let layers = [
        (Color32::from_rgb(0xf9, 0xcc, 0x9d), "margin", d.margin),
        (Color32::from_rgb(0xfd, 0xdd, 0x9b), "border", d.border),
        (Color32::from_rgb(0xc3, 0xcf, 0x8a), "padding", d.padding),
    ];
    let mut r = rect;
    for (color, name, v) in layers {
        painter.rect_filled(r, 2.0, color);
        painter.text(
            r.left_top() + Vec2::new(5.0, 1.0),
            Align2::LEFT_TOP,
            name,
            FontId::proportional(9.0),
            dark,
        );
        let c = r.center();
        let font = mono(10.0);
        painter.text(
            egui::pos2(c.x, r.top() + 12.0),
            Align2::CENTER_CENTER,
            fmt_px(v[0]),
            font.clone(),
            dark,
        );
        painter.text(
            egui::pos2(r.right() - 8.0, c.y),
            Align2::CENTER_CENTER,
            fmt_px(v[1]),
            font.clone(),
            dark,
        );
        painter.text(
            egui::pos2(c.x, r.bottom() - 10.0),
            Align2::CENTER_CENTER,
            fmt_px(v[2]),
            font.clone(),
            dark,
        );
        painter.text(
            egui::pos2(r.left() + 8.0, c.y),
            Align2::CENTER_CENTER,
            fmt_px(v[3]),
            font,
            dark,
        );
        r = r.shrink2(Vec2::new(26.0, 22.0));
    }
    painter.rect_filled(r, 2.0, Color32::from_rgb(0x8c, 0xb6, 0xc0));
    let cw = (d.rect[2] - d.padding[1] - d.padding[3] - d.border[1] - d.border[3]).max(0.0);
    let ch = (d.rect[3] - d.padding[0] - d.padding[2] - d.border[0] - d.border[2]).max(0.0);
    painter.text(
        r.center(),
        Align2::CENTER_CENTER,
        format!(
            "{} × {}",
            fmt_px(cw).replace('-', "0"),
            fmt_px(ch).replace('-', "0")
        ),
        mono(10.5),
        dark,
    );
}

fn detail_pane(st: &mut DevtoolsState, ui: &mut Ui, cmds: &mut Vec<Cmd>, pal: &Palette) {
    let Some(sel) = st.selected.clone() else {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Selecciona un nodo del árbol para ver sus atributos y estilos")
                .size(11.0)
                .color(pal.text_dim),
        );
        return;
    };
    if st.detail.is_none() {
        ui.add_space(8.0);
        ui.label(RichText::new("Cargando…").color(pal.text_dim));
        return;
    }
    let h = ui.available_height().max(40.0);
    let mut edits: Vec<String> = Vec::new();
    {
        let st_ref = &mut *st;
        let Some(d) = st_ref.detail.as_ref() else {
            return;
        };
        if d.gone {
            ui.label(RichText::new("El nodo ya no existe. Actualiza el árbol.").color(pal.danger));
            return;
        }
        let is_element = d.is_element;
        let rect = d.rect;
        let styles: Vec<(String, String)> = d
            .styles
            .iter()
            .filter(|(k, v)| !v.is_empty() && style_matches(k, v, &st_ref.style_filter))
            .cloned()
            .collect();
        let box_d = Detail {
            margin: d.margin,
            border: d.border,
            padding: d.padding,
            rect: d.rect,
            ..Default::default()
        };
        ScrollArea::vertical()
            .id_salt("dt_detail")
            .max_height(h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !is_element {
                    ui.label(RichText::new("Texto").strong().size(11.5));
                    ui.add(
                        TextEdit::multiline(&mut st_ref.text_edit)
                            .font(mono(11.5))
                            .desired_width(f32::INFINITY),
                    );
                    if ui.button("Aplicar").clicked() {
                        edits.push(edit_script(
                            &sel,
                            &format!("n.nodeValue={};", js_str(&st_ref.text_edit)),
                        ));
                    }
                    return;
                }
                ui.label(RichText::new("Atributos").strong().size(11.5));
                let mut remove: Option<usize> = None;
                for (i, (k, v)) in st_ref.attr_edits.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [80.0, 16.0],
                            egui::Label::new(
                                RichText::new(truncate_chars(k, 14))
                                    .font(mono(11.0))
                                    .color(syntax(pal).attr),
                            ),
                        )
                        .on_hover_text(k.as_str());
                        let resp = ui.add(
                            TextEdit::singleline(v)
                                .font(mono(11.0))
                                .desired_width((ui.available_width() - 28.0).max(60.0)),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            edits.push(edit_script(
                                &sel,
                                &format!("n.setAttribute({},{});", js_str(k), js_str(v)),
                            ));
                        }
                        if small_icon(ui, icon::X, "Eliminar atributo").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    let k = st_ref.attr_edits[i].0.clone();
                    edits.push(edit_script(
                        &sel,
                        &format!("n.removeAttribute({});", js_str(&k)),
                    ));
                }
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut st_ref.new_attr.0)
                            .hint_text("atributo")
                            .font(mono(11.0))
                            .desired_width(80.0),
                    );
                    ui.add(
                        TextEdit::singleline(&mut st_ref.new_attr.1)
                            .hint_text("valor")
                            .font(mono(11.0))
                            .desired_width((ui.available_width() - 28.0).max(60.0)),
                    );
                    if small_icon(ui, icon::PLUS, "Añadir atributo").clicked()
                        && !st_ref.new_attr.0.trim().is_empty()
                    {
                        edits.push(edit_script(
                            &sel,
                            &format!(
                                "n.setAttribute({},{});",
                                js_str(st_ref.new_attr.0.trim()),
                                js_str(&st_ref.new_attr.1)
                            ),
                        ));
                        st_ref.new_attr = Default::default();
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Modelo de caja").strong().size(11.5));
                box_model(ui, &box_d);
                ui.label(
                    RichText::new(format!(
                        "Posición {}, {}  ·  {} × {} px",
                        fmt_px(rect[0]).replace('-', "0"),
                        fmt_px(rect[1]).replace('-', "0"),
                        fmt_px(rect[2]).replace('-', "0"),
                        fmt_px(rect[3]).replace('-', "0")
                    ))
                    .font(mono(10.5))
                    .color(pal.text_dim),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Estilos calculados").strong().size(11.5));
                ui.add(
                    TextEdit::singleline(&mut st_ref.style_filter)
                        .hint_text("Filtrar propiedades")
                        .font(mono(11.0))
                        .desired_width(f32::INFINITY),
                );
                for (k, v) in &styles {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{k}:"))
                                .font(mono(10.5))
                                .color(syntax(pal).attr),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(v).font(mono(10.5)).color(syntax(pal).value),
                            )
                            .truncate(),
                        );
                    });
                }
            });
    }
    for script in edits {
        st.send(cmds, Kind::Edit, script);
    }
}

// ---------------------------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------------------------

fn network_section(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    cmds: &mut Vec<Cmd>,
    env: &mut Env,
    pal: &Palette,
) {
    let loading = w.active_tab().is_some_and(|t| t.is_loading());
    let due = st.perf_at.is_none_or(|t| {
        (loading && t.elapsed() >= Duration::from_secs(2)) || (!loading && st.perf_loading)
    });
    if due && !st.busy(Kind::Perf) {
        st.perf_at = Some(Instant::now());
        st.perf_loading = loading;
        st.send(cmds, Kind::Perf, JS_PERF.to_string());
    }
    if loading {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }

    let mut clear = false;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        if small_icon(ui, icon::PROHIBIT, "Limpiar").clicked() {
            clear = true;
        }
        ui.add(
            TextEdit::singleline(&mut st.net_filter)
                .hint_text("Filtrar URL")
                .desired_width(160.0)
                .font(mono(11.0)),
        );
        for f in NetFilter::ALL {
            if ui
                .selectable_label(st.net_type == f, RichText::new(f.label()).size(11.0))
                .clicked()
            {
                st.net_type = f;
            }
        }
    });
    if clear && let Some(t) = w.active_tab_mut() {
        t.network.clear();
        st.net_sel = None;
    }

    // Load-performance mini summary.
    if let Some(n) = &st.perf.nav {
        let mut parts = vec![
            format!("TTFB {}", fmt_ms(n.ttfb)),
            format!("DOMContentLoaded {}", fmt_ms(n.dcl)),
        ];
        if n.load > 0.0 {
            parts.push(format!("carga {}", fmt_ms(n.load)));
        }
        parts.push(format!("{} recursos", st.perf.res.len()));
        parts.push(fmt_bytes(st.perf.total_size + n.size));
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Rendimiento de carga: {}", parts.join(" · ")))
                    .size(11.0)
                    .color(pal.text_dim),
            );
        });
    }

    let Some(tab) = w.active_tab() else {
        return;
    };
    let first = tab.network.iter().map(|e| e.time).min();
    let idxs: Vec<usize> = tab
        .network
        .iter()
        .enumerate()
        .filter(|(_, e)| net_matches(&e.url, &e.kind, &st.net_filter, st.net_type))
        .map(|(i, _)| i)
        .collect();
    let blocked = tab.network.iter().filter(|e| e.blocked).count();
    let total_n = tab.network.len();

    const FR: [f32; 8] = [0.26, 0.07, 0.07, 0.2, 0.09, 0.1, 0.1, 0.11];
    let head = |s: &str| (s.to_string(), pal.text_dim);
    table_row(
        ui,
        pal,
        &FR,
        &[
            head("Nombre"),
            head("Tipo"),
            head("Método"),
            head("Dominio"),
            head("Estado"),
            head("Tamaño"),
            head("Duración"),
            head("Tiempo"),
        ],
        false,
        true,
    );
    let detail_h = if st.net_sel.is_some() { 54.0 } else { 0.0 };
    let list_h = (ui.available_height() - 22.0 - detail_h).max(30.0);
    let mut clicked: Option<usize> = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ScrollArea::vertical()
            .id_salt("dt_net_list")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H + 2.0, idxs.len(), |ui, range| {
                for r in range {
                    let i = idxs[r];
                    let e = &tab.network[i];
                    let (dur, size) = if e.main_frame {
                        st.perf
                            .nav
                            .as_ref()
                            .map(|n| (Some(n.dur), Some(n.size)))
                            .unwrap_or((None, None))
                    } else {
                        st.perf
                            .res
                            .get(&e.url)
                            .map(|(d, s)| (Some(*d), Some(*s)))
                            .unwrap_or((None, None))
                    };
                    let rel = first
                        .and_then(|f| e.time.duration_since(f).ok())
                        .map(|d| d.as_secs_f64() * 1000.0)
                        .unwrap_or(0.0);
                    let (status, scolor) = if e.blocked {
                        ("bloqueado".to_string(), pal.danger)
                    } else {
                        ("✓".to_string(), pal.ok)
                    };
                    let name_color = if e.blocked { pal.danger } else { pal.text };
                    let cells = [
                        (url_name(&e.url), name_color),
                        (kind_label(&e.kind).to_string(), pal.text_dim),
                        (e.method.clone(), pal.text_dim),
                        (url_domain(&e.url), pal.text_dim),
                        (status, scolor),
                        (
                            size.filter(|s| *s > 0)
                                .map(fmt_bytes)
                                .unwrap_or_else(|| "—".into()),
                            pal.text_dim,
                        ),
                        (dur.map(fmt_ms).unwrap_or_else(|| "—".into()), pal.text_dim),
                        (fmt_ms(rel), pal.text_dim),
                    ];
                    if table_row(ui, pal, &FR, &cells, st.net_sel == Some(i), false).clicked() {
                        clicked = Some(i);
                    }
                }
            });
    });
    if let Some(i) = clicked {
        st.net_sel = if st.net_sel == Some(i) { None } else { Some(i) };
    }

    if let Some(e) = st.net_sel.and_then(|i| tab.network.get(i)) {
        let url = e.url.clone();
        let line = format!(
            "{} · {} · {}",
            e.method,
            kind_label(&e.kind),
            if e.blocked {
                "bloqueado por el bloqueador de contenido"
            } else {
                "permitido"
            }
        );
        egui::Frame::new()
            .fill(pal.field)
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&url).font(mono(11.0)).color(pal.text))
                            .selectable(true)
                            .truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(line).size(11.0).color(pal.text_dim));
                    if small_icon(ui, icon::COPY, "Copiar URL").clicked() {
                        ui.ctx().copy_text(url.clone());
                        env.shared.toast("URL copiada");
                    }
                    if small_icon(ui, icon::ARROW_SQUARE_OUT, "Abrir en una pestaña nueva")
                        .clicked()
                        && let Ok(u) = url::Url::parse(&url)
                    {
                        cmds.push(Cmd::OpenUrl {
                            url: u,
                            disposition: Disposition::NewForegroundTab,
                        });
                    }
                });
            });
    }
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{total_n} peticiones · {blocked} bloqueadas{}",
                if idxs.len() != total_n {
                    format!(" · {} mostradas", idxs.len())
                } else {
                    String::new()
                }
            ))
            .size(11.0)
            .color(pal.text_dim),
        );
    });
}

// ---------------------------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------------------------

fn load_cookies(st: &mut DevtoolsState, env: &Env, url: Option<url::Url>) {
    st.cookies_at = Some(Instant::now());
    st.cookies.clear();
    let Some(url) = url else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    for c in env
        .servo
        .site_data_manager()
        .cookies_for_url(url, servo::CookieSource::HTTP)
    {
        let mut flags = Vec::new();
        if c.http_only().unwrap_or(false) {
            flags.push("HttpOnly");
        }
        if c.secure().unwrap_or(false) {
            flags.push("Secure");
        }
        st.cookies.push(CookieRow {
            name: c.name().to_string(),
            value: c.value().to_string(),
            domain: c.domain().unwrap_or("").to_string(),
            path: c.path().unwrap_or("/").to_string(),
            expires: c
                .expires_datetime()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "Sesión".into()),
            flags: flags.join(" "),
        });
    }
}

fn storage_section(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    cmds: &mut Vec<Cmd>,
    env: &mut Env,
    pal: &Palette,
) {
    if st.cookies_at.is_none() {
        let url = w.active_tab().and_then(|t| t.url.clone());
        load_cookies(st, env, url);
    }
    if !st.storage_fetched && !st.busy(Kind::Storage) {
        st.storage_fetched = true;
        st.send(cmds, Kind::Storage, JS_STORAGE.to_string());
    }
    let mut ops: Vec<String> = Vec::new();
    ScrollArea::vertical()
        .id_salt("dt_storage")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(
                RichText::new(format!("Cookies ({})", st.cookies.len()))
                    .strong()
                    .size(12.0),
            )
            .default_open(true)
            .show(ui, |ui| {
                const FR: [f32; 6] = [0.18, 0.34, 0.16, 0.08, 0.16, 0.08];
                let head = |s: &str| (s.to_string(), pal.text_dim);
                table_row(
                    ui,
                    pal,
                    &FR,
                    &[
                        head("Nombre"),
                        head("Valor"),
                        head("Dominio"),
                        head("Ruta"),
                        head("Expira"),
                        head("Flags"),
                    ],
                    false,
                    true,
                );
                if st.cookies.is_empty() {
                    ui.label(
                        RichText::new("Sin cookies para esta página")
                            .size(11.0)
                            .color(pal.text_dim),
                    );
                }
                for c in &st.cookies {
                    table_row(
                        ui,
                        pal,
                        &FR,
                        &[
                            (c.name.clone(), pal.text),
                            (c.value.clone(), pal.text_dim),
                            (c.domain.clone(), pal.text_dim),
                            (c.path.clone(), pal.text_dim),
                            (c.expires.clone(), pal.text_dim),
                            (c.flags.clone(), pal.text_dim),
                        ],
                        false,
                        false,
                    );
                }
            });
            if let Some(e) = &st.storage_error {
                ui.label(
                    RichText::new(truncate_chars(e, 120))
                        .size(11.0)
                        .color(pal.danger),
                );
            }
            for (session, title, area) in [
                (false, "Local storage", "localStorage"),
                (true, "Session storage", "sessionStorage"),
            ] {
                let count = st.storage.iter().filter(|i| i.session == session).count();
                egui::CollapsingHeader::new(
                    RichText::new(format!("{title} ({count})"))
                        .strong()
                        .size(12.0),
                )
                .id_salt(area)
                .default_open(true)
                .show(ui, |ui| {
                    let mut remove: Option<String> = None;
                    for item in st.storage.iter_mut().filter(|i| i.session == session) {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [140.0, 16.0],
                                egui::Label::new(
                                    RichText::new(truncate_chars(&item.key, 22))
                                        .font(mono(11.0))
                                        .color(syntax(pal).attr),
                                ),
                            )
                            .on_hover_text(item.key.as_str());
                            let resp = ui.add(
                                TextEdit::singleline(&mut item.edit)
                                    .font(mono(11.0))
                                    .desired_width((ui.available_width() - 30.0).max(80.0)),
                            );
                            if resp.lost_focus()
                                && ui.input(|i| i.key_pressed(Key::Enter))
                                && item.edit != item.value
                            {
                                ops.push(format!(
                                    "{area}.setItem({},{})",
                                    js_str(&item.key),
                                    js_str(&item.edit)
                                ));
                                item.value = item.edit.clone();
                            }
                            if small_icon(ui, icon::TRASH, "Eliminar clave").clicked() {
                                remove = Some(item.key.clone());
                            }
                        });
                    }
                    if let Some(k) = remove {
                        ops.push(format!("{area}.removeItem({})", js_str(&k)));
                    }
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut st.new_key)
                                .hint_text("clave")
                                .font(mono(11.0))
                                .desired_width(140.0),
                        );
                        ui.add(
                            TextEdit::singleline(&mut st.new_val)
                                .hint_text("valor")
                                .font(mono(11.0))
                                .desired_width(200.0),
                        );
                        if ui.small_button("Añadir").clicked() && !st.new_key.is_empty() {
                            ops.push(format!(
                                "{area}.setItem({},{})",
                                js_str(&st.new_key),
                                js_str(&st.new_val)
                            ));
                            st.new_key.clear();
                            st.new_val.clear();
                        }
                        if ui.small_button("Borrar todo").clicked() {
                            ops.push(format!("{area}.clear()"));
                        }
                    });
                });
            }
        });
    for op in ops {
        let script =
            format!("(function(){{try{{{op};return '1';}}catch(e){{return String(e);}}}})()");
        st.send(cmds, Kind::Misc, script);
        st.storage_fetched = false;
    }
}

// ---------------------------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------------------------

fn performance_section(
    st: &mut DevtoolsState,
    ui: &mut Ui,
    w: &mut BrowserWindow,
    cmds: &mut Vec<Cmd>,
    env: &mut Env,
    pal: &Palette,
) {
    let due = st
        .metrics_at
        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
    if due && !st.busy(Kind::Metrics) {
        st.metrics_at = Some(Instant::now());
        st.send(cmds, Kind::Metrics, JS_METRICS.to_string());
    }
    ui.ctx().request_repaint_after(Duration::from_millis(1000));

    let mut open_adv = false;
    ScrollArea::vertical()
        .id_salt("dt_perf")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(6.0);
            egui::Grid::new("dt_perf_grid")
                .num_columns(2)
                .spacing([18.0, 4.0])
                .show(ui, |ui| {
                    let m = st.metrics.as_ref();
                    let val = |s: String| RichText::new(s).font(mono(12.0)).color(pal.text);
                    let rows: [(&str, String); 6] = [
                        ("Nodos DOM", m.map_or("—".into(), |m| m.dom.to_string())),
                        (
                            "Memoria JS usada",
                            m.and_then(|m| m.heap)
                                .map_or("no disponible".into(), fmt_bytes),
                        ),
                        (
                            "Memoria JS total",
                            m.and_then(|m| m.heap_total)
                                .map_or("no disponible".into(), fmt_bytes),
                        ),
                        (
                            "FPS (estimado)",
                            m.map_or("—".into(), |m| format!("{:.0}", m.fps)),
                        ),
                        (
                            "Estado del documento",
                            m.map_or("—".into(), |m| m.ready.clone()),
                        ),
                        (
                            "Recursos cargados",
                            m.map_or("—".into(), |m| m.res.to_string()),
                        ),
                    ];
                    for (k, v) in rows {
                        ui.label(RichText::new(k).size(11.5).color(pal.text_dim));
                        ui.label(val(v));
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new("FPS (último minuto)")
                    .size(11.0)
                    .color(pal.text_dim),
            );
            let (rect, _) = ui.allocate_exact_size(Vec2::new(240.0, 40.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, pal.field);
            if st.fps_hist.len() > 1 {
                let max = st.fps_hist.iter().cloned().fold(60.0_f32, f32::max);
                let n = (st.fps_hist.len() - 1) as f32;
                let pts: Vec<egui::Pos2> = st
                    .fps_hist
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        egui::pos2(
                            rect.left() + rect.width() * i as f32 / n,
                            rect.bottom() - 2.0 - (rect.height() - 4.0) * v / max,
                        )
                    })
                    .collect();
                ui.painter()
                    .add(egui::Shape::line(pts, Stroke::new(1.5, pal.accent)));
            }
            ui.add_space(14.0);
            ui.separator();
            ui.label(
                RichText::new("Herramientas de desarrollo completas")
                    .strong()
                    .size(12.0),
            );
            if env.profile.settings.enable_devtools {
                ui.label(
                    RichText::new(format!(
                        "Servo expone su servidor de depuración remota. En Firefox abre \
                         about:debugging → Configurar → añade localhost:{} y conecta.",
                        env.profile.settings.devtools_port
                    ))
                    .size(11.5)
                    .color(pal.text_dim),
                );
            } else {
                ui.label(
                    RichText::new(
                        "Para usar el depurador completo de Firefox con Servo, activa el \
                         servidor de depuración remota en Ajustes → Avanzado.",
                    )
                    .size(11.5)
                    .color(pal.text_dim),
                );
                if ui.button("Abrir Ajustes → Avanzado").clicked() {
                    open_adv = true;
                }
            }
        });
    if open_adv {
        w.chrome.settings = Some(crate::ui::SettingsPage::Advanced);
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn url_parts() {
        assert_eq!(url_name("https://example.com/a/b/app.js?v=1"), "app.js");
        assert_eq!(url_name("https://example.com/"), "example.com");
        assert_eq!(url_name("https://example.com/dir/"), "dir");
        assert_eq!(url_name("data:image/png;base64,AAAA"), "data:…");
        assert_eq!(url_name("not a url/last"), "last");
        assert_eq!(
            url_domain("https://cdn.example.com:8080/x"),
            "cdn.example.com"
        );
        assert_eq!(url_domain("nonsense"), "");
    }

    #[test]
    fn net_filtering() {
        assert_eq!(NetFilter::of_kind("mainframe"), NetFilter::Doc);
        assert_eq!(NetFilter::of_kind("stylesheet"), NetFilter::Css);
        assert_eq!(NetFilter::of_kind("ping"), NetFilter::Other);
        assert!(net_matches(
            "https://a.com/x.js",
            "script",
            "",
            NetFilter::All
        ));
        assert!(net_matches(
            "https://a.com/X.js",
            "script",
            "x.JS",
            NetFilter::Js
        ));
        assert!(!net_matches(
            "https://a.com/x.js",
            "script",
            "",
            NetFilter::Css
        ));
        assert!(!net_matches(
            "https://a.com/x.js",
            "script",
            "zzz",
            NetFilter::All
        ));
    }

    #[test]
    fn console_filtering() {
        let none = [false; 4];
        assert!(console_visible("error", "Boom", "boom", none));
        assert!(!console_visible(
            "error",
            "Boom",
            "",
            [true, false, false, false]
        ));
        assert!(!console_visible("log", "hello", "bye", none));
        assert!(console_visible("input", "anything", "zzz", [true; 4]));
        assert_eq!(level_group("debug"), Some(3));
        assert_eq!(level_group("result"), None);
    }

    #[test]
    fn value_formatting() {
        assert_eq!(scalar_display(&json!("hi")), "\"hi\"");
        assert_eq!(scalar_display(&json!("«undefined»")), "undefined");
        assert_eq!(scalar_display(&json!(null)), "null");
        assert_eq!(scalar_display(&json!([1, 2])), "Array(2)");
        assert_eq!(preview(&json!({"a": 1}), 50), "{\"a\":1}");
        assert_eq!(preview(&json!({"abcdef": 1}), 4), "{\"ab…");
        assert_eq!(fmt_ms(2.5), "2.5 ms");
        assert_eq!(fmt_ms(120.0), "120 ms");
        assert_eq!(fmt_ms(2500.0), "2.50 s");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 kB");
        assert_eq!(fmt_px(0.0), "-");
        assert_eq!(fmt_px(8.0), "8");
        assert_eq!(fmt_px(8.5), "8.50");
    }

    #[test]
    fn result_parsing() {
        let v = parse_result(Ok(Value::String("{\"ok\":1}".into()))).unwrap();
        assert_eq!(v["ok"], 1);
        let s = parse_result(Ok(Value::String("plain".into()))).unwrap();
        assert_eq!(s, Value::String("plain".into()));
        assert!(parse_result(Err("x".into())).is_err());
    }

    #[test]
    fn dom_parse_and_flatten() {
        let v = json!({"i":0,"k":1,"t":"html","a":[["lang","es"]],"c":[
            {"i":1,"k":1,"t":"body","a":[],"c":[{"i":0,"k":3,"x":"hola"}]},
            {"i":3,"k":8,"x":" c "}],"m":2});
        let root = parse_dom(&v).unwrap();
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.more, 2);
        let mut exp = HashSet::new();
        let mut rows = Vec::new();
        flatten(&root, 0, &mut Vec::new(), &exp, &mut rows);
        assert_eq!(rows.len(), 1);
        exp.insert(Vec::new());
        exp.insert(vec![1]);
        let mut rows = Vec::new();
        flatten(&root, 0, &mut Vec::new(), &exp, &mut rows);
        let paths: Vec<_> = rows.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, vec![vec![], vec![1], vec![1, 0], vec![3]]);
    }

    #[test]
    fn scripts_embed_path_and_code() {
        let s = js_with_path(JS_DETAIL, &[0, 2]);
        assert!(s.contains("var P=[0,2];"));
        assert!(!s.contains("__PATH__") && !s.contains("__RESOLVE__"));
        let e = eval_wrapper("1+\"x\"");
        assert!(e.contains("(0,eval)(\"1+\\\"x\\\"\")"));
    }

    #[test]
    fn style_filter() {
        assert!(style_matches("background-color", "red", "COLOR"));
        assert!(style_matches("x", "Red", "red"));
        assert!(!style_matches("x", "y", "z"));
        assert!(style_matches("x", "y", ""));
    }
}
