//! Servo → FerroSurf callbacks. Every callback is turned into an [`EngineEvent`] queued on
//! a shared queue; the app drains it right after `Servo::spin_event_loop`, so no app state
//! is borrowed while Servo runs.

use std::cell::RefCell;
use std::collections::VecDeque;

use servo::{
    AllowOrDenyRequest, AuthenticationRequest, ConsoleLogLevel, CreateNewWebViewRequest, Cursor,
    EmbedderControl, EmbedderControlId, LoadStatus, NavigationRequest, Notification,
    PermissionRequest, ServoDelegate, ServoError, TraversalId, WebResourceLoad, WebView,
    WebViewDelegate,
};
use url::Url;

#[allow(dead_code)] // payloads kept for completeness of the event model
pub enum EngineEvent {
    NewFrame(WebView),
    UrlChanged(WebView, Url),
    TitleChanged(WebView, Option<String>),
    StatusText(WebView, Option<String>),
    LoadStatus(WebView, LoadStatus),
    Cursor(WebView, Cursor),
    Favicon(WebView),
    History(WebView, Vec<Url>, usize),
    Closed(WebView),
    Crashed(WebView, String),
    Fullscreen(WebView, bool),
    Navigation(WebView, NavigationRequest),
    Unload(WebView, AllowOrDenyRequest),
    CreateNew(WebView, CreateNewWebViewRequest),
    Permission(WebView, PermissionRequest),
    Authentication(WebView, AuthenticationRequest),
    ShowControl(WebView, EmbedderControl),
    HideControl(WebView, EmbedderControlId),
    WebResource(Option<WebView>, WebResourceLoad),
    Notification(Option<WebView>, Notification),
    Console(Option<WebView>, ConsoleLogLevel, String),
    ResizeTo(WebView),
    DevtoolsStarted(u16, String),
    DevtoolsConnection(AllowOrDenyRequest),
    Error(String),
}

#[derive(Default)]
pub struct Delegate {
    queue: RefCell<VecDeque<EngineEvent>>,
}

impl Delegate {
    fn push(&self, e: EngineEvent) {
        self.queue.borrow_mut().push_back(e);
    }

    pub fn drain(&self) -> Vec<EngineEvent> {
        self.queue.borrow_mut().drain(..).collect()
    }
}

impl WebViewDelegate for Delegate {
    fn notify_url_changed(&self, webview: WebView, url: Url) {
        self.push(EngineEvent::UrlChanged(webview, url));
    }
    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        self.push(EngineEvent::TitleChanged(webview, title));
    }
    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        self.push(EngineEvent::StatusText(webview, status));
    }
    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.push(EngineEvent::LoadStatus(webview, status));
    }
    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        self.push(EngineEvent::Cursor(webview, cursor));
    }
    fn notify_favicon_changed(&self, webview: WebView) {
        self.push(EngineEvent::Favicon(webview));
    }
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.push(EngineEvent::NewFrame(webview));
    }
    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        self.push(EngineEvent::History(webview, entries, current));
    }
    fn notify_traversal_complete(&self, _webview: WebView, _traversal_id: TraversalId) {}
    fn notify_closed(&self, webview: WebView) {
        self.push(EngineEvent::Closed(webview));
    }
    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        self.push(EngineEvent::Crashed(webview, reason));
    }
    fn notify_fullscreen_state_changed(&self, webview: WebView, is_fullscreen: bool) {
        self.push(EngineEvent::Fullscreen(webview, is_fullscreen));
    }
    fn request_navigation(&self, webview: WebView, navigation_request: NavigationRequest) {
        self.push(EngineEvent::Navigation(webview, navigation_request));
    }
    fn request_unload(&self, webview: WebView, unload_request: AllowOrDenyRequest) {
        self.push(EngineEvent::Unload(webview, unload_request));
    }
    fn request_resize_to(&self, webview: WebView, _size: servo::DeviceIntSize) {
        self.push(EngineEvent::ResizeTo(webview));
    }
    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        self.push(EngineEvent::CreateNew(parent, request));
    }
    fn request_permission(&self, webview: WebView, request: PermissionRequest) {
        self.push(EngineEvent::Permission(webview, request));
    }
    fn request_authentication(&self, webview: WebView, request: AuthenticationRequest) {
        self.push(EngineEvent::Authentication(webview, request));
    }
    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        self.push(EngineEvent::ShowControl(webview, control));
    }
    fn hide_embedder_control(&self, webview: WebView, id: EmbedderControlId) {
        self.push(EngineEvent::HideControl(webview, id));
    }
    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        self.push(EngineEvent::WebResource(Some(webview), load));
    }
    fn show_notification(&self, webview: WebView, notification: Notification) {
        self.push(EngineEvent::Notification(Some(webview), notification));
    }
    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        self.push(EngineEvent::Console(Some(webview), level, message));
    }
}

impl ServoDelegate for Delegate {
    fn notify_error(&self, error: ServoError) {
        self.push(EngineEvent::Error(format!("{error:?}")));
    }
    fn notify_devtools_server_started(&self, port: u16, token: String) {
        self.push(EngineEvent::DevtoolsStarted(port, token));
    }
    fn request_devtools_connection(&self, request: AllowOrDenyRequest) {
        self.push(EngineEvent::DevtoolsConnection(request));
    }
    fn load_web_resource(&self, load: WebResourceLoad) {
        self.push(EngineEvent::WebResource(None, load));
    }
    fn show_notification(&self, notification: Notification) {
        self.push(EngineEvent::Notification(None, notification));
    }
    fn show_console_message(&self, level: ConsoleLogLevel, message: String) {
        self.push(EngineEvent::Console(None, level, message));
    }
}
