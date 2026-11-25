//! `ferro://` internal pages (new tab, about, error) and `ferro-extension://` resources,
//! served to Servo through custom protocol handlers.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use ferrosurf_core::util::html_escape;
use parking_lot::RwLock;
use servo::ServoUrl;
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, ProtocolRegistry, Request, ResourceFetchTiming,
    Response, ResponseBody,
};

const NEWTAB: &str = include_str!("../resources/pages/newtab.html");
const ABOUT: &str = include_str!("../resources/pages/about.html");
const ERROR: &str = include_str!("../resources/pages/error.html");
const LOGO_SVG: &str = include_str!("../resources/icons/ferrosurf.svg");

/// Data the UI thread publishes for internal pages (read from Servo's network thread).
#[derive(Clone, Default)]
pub struct PageContext {
    pub dark: bool,
    pub accent: String,
    pub search_name: String,
    pub search_url: String,
    pub top_sites: Vec<(String, String)>,
    pub bookmarks: Vec<(String, String)>,
    pub profile_name: String,
    pub data_dir: String,
    pub mcp_url: String,
    /// Extension id → root folder, for `ferro-extension://`.
    pub extensions: Vec<(String, PathBuf)>,
    /// A new tab page provided by an extension.
    pub newtab_override: Option<PathBuf>,
}

pub type SharedPageContext = Arc<RwLock<PageContext>>;

/// Escape a value placed inside a double-quoted JS string literal.
fn js_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('<', "\\u003c").replace('\n', "\\n")
}

fn json_list(items: &[(String, String)]) -> String {
    let v: Vec<_> = items
        .iter()
        .map(|(t, u)| serde_json::json!({"title": t, "url": u}))
        .collect();
    serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()).replace("</", "<\\/")
}

fn common(page: &str, ctx: &PageContext) -> String {
    page.replace("{{THEME}}", if ctx.dark { "dark" } else { "light" })
        .replace("{{ACCENT}}", &html_escape(&ctx.accent))
}

pub fn render_newtab(ctx: &PageContext) -> String {
    common(NEWTAB, ctx)
        .replace("{{SEARCH_NAME}}", &js_str(&ctx.search_name))
        .replace("{{SEARCH_URL}}", &js_str(&ctx.search_url))
        .replace("{{GREETING_NAME}}", &js_str(&ctx.profile_name))
        .replace("{{TOP_SITES_JSON}}", &json_list(&ctx.top_sites))
        .replace("{{BOOKMARKS_JSON}}", &json_list(&ctx.bookmarks))
}

pub fn render_about(ctx: &PageContext) -> String {
    common(ABOUT, ctx)
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
        .replace("{{ENGINE}}", "Servo 0.8")
        .replace("{{PLATFORM}}", &format!("{} {}", std::env::consts::OS, std::env::consts::ARCH))
        .replace("{{PROFILE}}", &html_escape(&ctx.profile_name))
        .replace("{{DATA_DIR}}", &html_escape(&ctx.data_dir))
        .replace(
            "{{MCP_URL}}",
            &html_escape(if ctx.mcp_url.is_empty() { "(desactivado)" } else { &ctx.mcp_url }),
        )
}

pub fn render_error(ctx: &PageContext, title: &str, message: &str, url: &str) -> String {
    common(ERROR, ctx)
        .replace("{{TITLE}}", &html_escape(title))
        .replace("{{MESSAGE}}", &html_escape(message))
        .replace("{{URL}}", &html_escape(url))
}

fn response(url: ServoUrl, request: &Request, body: Vec<u8>, mime: &str, status: u16) -> Response {
    let mut response = Response::new(url, ResourceFetchTiming::new(request.timing_type()));
    *response.body.lock() = ResponseBody::Done(body);
    if let Ok(v) = http::HeaderValue::from_str(mime) {
        response.headers.insert(http::header::CONTENT_TYPE, v);
    }
    response.headers.insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    response.status = if status == 200 {
        HttpStatus::default()
    } else {
        HttpStatus::new_raw(status, Vec::new())
    };
    response
}

struct FerroHandler {
    ctx: SharedPageContext,
}

impl ProtocolHandler for FerroHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let ctx = self.ctx.read().clone();
        let page = url.as_url().host_str().unwrap_or("").to_string();
        let query: std::collections::HashMap<String, String> =
            url.as_url().query_pairs().into_owned().collect();
        let (body, mime, status): (Vec<u8>, &str, u16) = match page.as_str() {
            "newtab" | "home" => {
                if let Some(path) = &ctx.newtab_override {
                    match std::fs::read(path) {
                        Ok(b) => (b, "text/html; charset=utf-8", 200),
                        Err(_) => (render_newtab(&ctx).into_bytes(), "text/html; charset=utf-8", 200),
                    }
                } else {
                    (render_newtab(&ctx).into_bytes(), "text/html; charset=utf-8", 200)
                }
            },
            "about" | "version" => (render_about(&ctx).into_bytes(), "text/html; charset=utf-8", 200),
            "error" => (
                render_error(
                    &ctx,
                    query.get("title").map(String::as_str).unwrap_or("No se puede acceder a esta página"),
                    query.get("message").map(String::as_str).unwrap_or(""),
                    query.get("url").map(String::as_str).unwrap_or(""),
                )
                .into_bytes(),
                "text/html; charset=utf-8",
                200,
            ),
            "logo.svg" | "logo" => (LOGO_SVG.as_bytes().to_vec(), "image/svg+xml", 200),
            "blank" => (b"<!doctype html><title></title>".to_vec(), "text/html", 200),
            _ => (
                render_error(&ctx, "Página interna desconocida", "Esta página de FerroSurf no existe.", url.as_str())
                    .into_bytes(),
                "text/html; charset=utf-8",
                404,
            ),
        };
        let resp = response(url, request, body, mime, status);
        Box::pin(std::future::ready(resp))
    }

    fn is_fetchable(&self) -> bool {
        true
    }

    fn is_secure(&self) -> bool {
        true
    }
}

/// Serves files of installed extensions: `ferro-extension://<id>/<path>`.
struct ExtensionHandler {
    ctx: SharedPageContext,
}

pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

impl ProtocolHandler for ExtensionHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let id = url.as_url().host_str().unwrap_or("").to_string();
        let rel = url.as_url().path().trim_start_matches('/').to_string();
        let root = self
            .ctx
            .read()
            .extensions
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| r.clone());
        let found = root.and_then(|root| {
            if rel.split('/').any(|seg| seg == ".." || seg.contains('\\')) {
                return None;
            }
            let path = root.join(&rel);
            let canon_root = root.canonicalize().ok()?;
            let canon = path.canonicalize().ok()?;
            if !canon.starts_with(&canon_root) {
                return None;
            }
            std::fs::read(&canon).ok().map(|b| (b, mime_for(&rel)))
        });
        let resp = match found {
            Some((body, mime)) => response(url, request, body, mime, 200),
            None => response(url, request, b"Not found".to_vec(), "text/plain", 404),
        };
        Box::pin(std::future::ready(resp))
    }

    fn is_fetchable(&self) -> bool {
        true
    }

    fn is_secure(&self) -> bool {
        true
    }
}

pub fn protocol_registry(ctx: SharedPageContext) -> ProtocolRegistry {
    let mut registry = ProtocolRegistry::default();
    if registry.register("ferro", FerroHandler { ctx: ctx.clone() }).is_err() {
        log::error!("could not register ferro:// handler");
    }
    if registry.register("ferro-extension", ExtensionHandler { ctx }).is_err() {
        log::error!("could not register ferro-extension:// handler");
    }
    registry
}

/// URL of the internal error page for a failed navigation.
pub fn error_url(title: &str, message: &str, failed: &str) -> url::Url {
    let mut u = url::Url::parse("ferro://error/").unwrap();
    u.query_pairs_mut()
        .append_pair("title", title)
        .append_pair("message", message)
        .append_pair("url", failed);
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtab_placeholders_filled() {
        let ctx = PageContext {
            dark: true,
            accent: "#ff6a3d".into(),
            search_name: "Duck\"Duck".into(),
            search_url: "https://duckduckgo.com/?q={searchTerms}".into(),
            top_sites: vec![("</script>".into(), "https://x.org".into())],
            ..Default::default()
        };
        let html = render_newtab(&ctx);
        assert!(!html.contains("{{"));
        assert!(!html.contains("</script>\""));
        assert!(html.contains("Duck\\\"Duck"));
        assert!(!render_about(&ctx).contains("{{"));
        assert!(!render_error(&ctx, "a", "b", "c").contains("{{"));
    }
}
