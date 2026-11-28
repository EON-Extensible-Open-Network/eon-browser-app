// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The browser: tabs on Servo, with `eon-browser-core`'s policy applied to every request
//! and every navigation, and the bar (the "chrome") kept up to date.
//!
//! Two kinds of view, two delegates:
//!
//! - **Pages** ([`Browser`] is their delegate). Every request goes through the shield,
//!   every navigation through link cleaning and HTTPS-only. They cannot reach the bar's
//!   bridge (`ui.eon.invalid`).
//! - **The bar** ([`Chrome`]). It loads only its own files and talks to the browser
//!   through `https://ui.eon.invalid/cmd`. Nothing it does reaches the network.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use eon_browser_core::lists::Lists;
use eon_browser_core::omnibox::{self, Target};
use eon_browser_core::profile::{Profile, Session, Tab as SavedTab, WindowState};
use eon_browser_core::search::{self, SearchEngine};
use eon_browser_core::shield::{self, Kind, Request, Shield, Verdict};
use eon_browser_core::upgrade::Upgrade;
use eon_browser_core::{fingerprint, navigate};
use euclid::Scale;
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::Serialize;
use servo::{
    CreateNewWebViewRequest, Cursor, JSValue, LoadStatus, NavigationRequest, PermissionRequest,
    RenderingContext, Servo, UserContentManager, UserScript, WebResourceLoad, WebResourceResponse,
    WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

use crate::input::Shortcut;
use crate::pages;
use crate::paths::Settings;

pub const UI_HOST: &str = "ui.eon.invalid";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A rectangle of the window in CSS pixels, from its top left. A width or height of 0
/// means "to the window's edge".
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, serde::Deserialize)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What the browser needs from whatever shows it: a window, or nothing (the self-test).
pub trait Host {
    fn redraw(&self);
    fn set_cursor(&self, cursor: Cursor);
    fn set_title(&self, title: &str);
    /// Where the page goes, and the overlay above it while open (the interface decides).
    fn set_layout(&self, page: Area, overlay: Option<Area>);
    fn focus_page(&self);
    fn focus_overlay(&self) {}
    fn focus_chrome(&self) {}
    /// Size the views again (a woken tab has a new one).
    fn relayout(&self) {}
    fn open_private_window(&self) {}
}

/// Pages served instead of the network, for the self-test: `host + path` → (type, body).
#[derive(Default)]
pub struct Fixtures(pub HashMap<String, (&'static str, Vec<u8>)>);

impl Fixtures {
    fn get(&self, url: &Url) -> Option<&(&'static str, Vec<u8>)> {
        self.0
            .get(&format!("{}{}", url.host_str().unwrap_or(""), url.path()))
    }
}

/// How the browser is started.
pub struct Options {
    pub private: bool,
    pub data: PathBuf,
    pub fixtures: Option<Fixtures>,
    /// Rules to use instead of the stored lists (the self-test).
    pub rules: Option<String>,
    /// The route process, if it started (madde 50k).
    pub route: Option<std::sync::Arc<crate::route_client::RouteClient>>,
}

struct Tab {
    /// `None` while the tab sleeps: its page is unloaded and the memory given back.
    view: RefCell<Option<WebView>>,
    /// Where a sleeping tab was, and its title, to show it and to wake it.
    saved_url: RefCell<Option<Url>>,
    saved_title: RefCell<Option<String>>,
    /// When the person last had this tab in front of them.
    last_active: Cell<std::time::Instant>,
    blocked: Cell<u32>,
    /// The `http://` address this load was upgraded from, to offer HTTP if HTTPS fails.
    upgraded_from: RefCell<Option<Url>>,
    /// What the last navigation's cleaning did, for the bar.
    note: RefCell<Option<String>>,
    /// A harmful-site warning the person chose to pass, once.
    allow_once: RefCell<Option<Url>>,
    /// The document a sub-resource belongs to, recorded when its main-frame request
    /// arrives. `WebView::url` can still name the previous page while the new one's first
    /// requests are already out (seen on macOS), and a request must never be judged
    /// against the wrong page.
    top: RefCell<Option<Url>>,
    /// The site's icon, as a key into `Browser::favicons`. Kept while the tab sleeps.
    favicon: Cell<Option<u64>>,
}

impl Tab {
    fn view(&self) -> Option<WebView> {
        self.view.borrow().clone()
    }
    fn is(&self, other: &WebView) -> bool {
        self.view
            .borrow()
            .as_ref()
            .is_some_and(|v| v.id() == other.id())
    }
    fn url(&self) -> Option<Url> {
        self.view()
            .and_then(|v| v.url())
            .or_else(|| self.saved_url.borrow().clone())
    }
    fn title(&self) -> Option<String> {
        self.view()
            .and_then(|v| v.page_title())
            .or_else(|| self.saved_title.borrow().clone())
    }
    fn asleep(&self) -> bool {
        self.view.borrow().is_none()
    }
}

pub struct Browser {
    servo: Servo,
    page_ctx: Rc<dyn RenderingContext>,
    scale: Cell<f32>,
    ucm: Rc<UserContentManager>,
    ui: RefCell<Option<WebView>>,
    /// Suggestions and the command bar, above the page.
    overlay: RefCell<Option<WebView>>,
    page_area: Cell<Area>,
    overlay_area: Cell<Option<Area>>,
    tabs: RefCell<Vec<Tab>>,
    active: Cell<usize>,
    profile: Profile,
    shield: RefCell<Shield>,
    extensions: RefCell<crate::extensions::Extensions>,
    settings: RefCell<Settings>,
    engines: Vec<SearchEngine>,
    private: bool,
    data: PathBuf,
    host: Box<dyn Host>,
    fixtures: Option<Fixtures>,
    me: RefCell<Weak<Browser>>,
    ui_dirty: Cell<bool>,
    route: Option<std::sync::Arc<crate::route_client::RouteClient>>,
    rules_override: Option<String>,
    /// Site icons for the tab strip as PNG, served to the interface at
    /// `ui.eon.invalid/favicon/<key>.png`; the key changes with the icon.
    favicons: RefCell<std::collections::HashMap<u64, Vec<u8>>>,
    next_favicon: Cell<u64>,
    monitor: RefCell<crate::monitor::Monitor>,
    sample: Cell<crate::monitor::Sample>,
    last_housekeeping: Cell<std::time::Instant>,
    /// Scripts to run in a page on the next turn: Servo cannot start an evaluation from
    /// inside another one's callback.
    deferred: RefCell<Vec<(WebView, String)>>,
    /// Every address a page asked the network for (the self-test reads it).
    pub requests: RefCell<Vec<String>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A JavaScript statement that adds `css` to the page as a stylesheet.
pub fn inject_css(css: &str) -> String {
    let lit = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(()=>{{const s=document.createElement('style');s.dataset.eon='shield';s.textContent={lit};\
         (document.head||document.documentElement).appendChild(s);}})()"
    )
}

/// Answer an intercepted request with `body`.
pub fn respond(load: WebResourceLoad, status: StatusCode, mime: &str, body: Vec<u8>) {
    let url = load.request().url.clone().into_url();
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(mime) {
        headers.insert(http::header::CONTENT_TYPE, v);
    }
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    let mut intercepted = load.intercept(
        WebResourceResponse::new(url)
            .status_code(status)
            .headers(headers),
    );
    intercepted.send_body_data(body);
    intercepted.finish();
}

/// Refuse an intercepted request (it fails like a network error).
pub fn refuse(load: WebResourceLoad) {
    let url = load.request().url.clone().into_url();
    load.intercept(WebResourceResponse::new(url)).cancel();
}

/// The lists (seeded from the release), the extensions' rules, compiled.
fn build_shield(data: &std::path::Path, rules: Option<&str>) -> Shield {
    if let Some(r) = rules {
        // The self-test's own rules, with whatever its extensions added.
        let mut all = r.to_string();
        if let Ok(l) = Lists::load(&data.join("lists")) {
            for (_, text) in l.extension_rules().unwrap_or_default() {
                all.push('\n');
                all.push_str(&text);
            }
        }
        return Shield::from_rules(&all);
    }
    let lists = Lists::load(&data.join("lists")).map(|mut l| {
        for (id, text) in crate::seed::SEED {
            // A snapshot that fails the checks is skipped, not fatal.
            let _ = l.seed(id, text, crate::seed::SEED_AT);
        }
        l
    });
    match lists {
        Ok(l) => Shield::build_cached(&l, &data.join("cache"))
            .or_else(|_| Shield::build(&l))
            .unwrap_or_else(|_| Shield::from_rules("")),
        Err(_) => Shield::from_rules(""),
    }
}

impl Browser {
    pub fn new(
        servo: Servo,
        page_ctx: Rc<dyn RenderingContext>,
        scale: f32,
        host: Box<dyn Host>,
        options: Options,
    ) -> Rc<Browser> {
        let profile = if options.private {
            Profile::in_memory()
        } else {
            Profile::open(&options.data.join("profile")).or_else(|_| Profile::in_memory())
        };
        // In-memory always opens; the error case is unreachable in practice, and a
        // browser without history is still a browser.
        let profile = match profile {
            Ok(p) => p,
            Err(e) => {
                eprintln!("eon-browser: no profile ({e}); continuing without one");
                match Profile::in_memory() {
                    Ok(p) => p,
                    Err(_) => std::process::exit(2),
                }
            }
        };
        let ucm = Rc::new(UserContentManager::new(&servo));
        ucm.add_script(Rc::new(UserScript::new(
            fingerprint::page_script().to_string(),
            None,
        )));
        let browser = Rc::new(Browser {
            shield: RefCell::new(build_shield(&options.data, options.rules.as_deref())),
            extensions: RefCell::new(crate::extensions::Extensions::load(&options.data)),
            rules_override: options.rules.clone(),
            favicons: RefCell::new(std::collections::HashMap::new()),
            next_favicon: Cell::new(1),
            monitor: RefCell::new(crate::monitor::Monitor::default()),
            sample: Cell::new(crate::monitor::Sample::default()),
            last_housekeeping: Cell::new(std::time::Instant::now()),
            settings: RefCell::new(Settings::load(&options.data)),
            engines: search::catalogue(),
            servo,
            page_ctx,
            scale: Cell::new(scale),
            ucm,
            ui: RefCell::new(None),
            overlay: RefCell::new(None),
            page_area: Cell::new(Area::default()),
            overlay_area: Cell::new(None),
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(0),
            profile,
            private: options.private,
            data: options.data,
            host,
            fixtures: options.fixtures,
            me: RefCell::new(Weak::new()),
            ui_dirty: Cell::new(false),
            route: options.route,
            deferred: RefCell::new(Vec::new()),
            requests: RefCell::new(Vec::new()),
        });
        *browser.me.borrow_mut() = Rc::downgrade(&browser);
        browser
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    fn rc(&self) -> Option<Rc<Browser>> {
        self.me.borrow().upgrade()
    }

    /// Attach the bar, drawn in `ctx`.
    pub fn attach_chrome(&self, ctx: Rc<dyn RenderingContext>) {
        let Some(me) = self.rc() else { return };
        let delegate = Rc::new(Chrome {
            browser: Rc::downgrade(&me),
        });
        let view = WebViewBuilder::new(&self.servo, ctx)
            .url(chrome_url("/index.html"))
            .hidpi_scale_factor(Scale::new(self.scale.get()))
            .delegate(delegate)
            .build();
        *self.ui.borrow_mut() = Some(view);
    }

    pub fn chrome_view(&self) -> Option<WebView> {
        self.ui.borrow().clone()
    }

    /// Attach the overlay (suggestions, command bar), drawn in `ctx`.
    pub fn attach_overlay(&self, ctx: Rc<dyn RenderingContext>) {
        let Some(me) = self.rc() else { return };
        let delegate = Rc::new(Chrome {
            browser: Rc::downgrade(&me),
        });
        let view = WebViewBuilder::new(&self.servo, ctx)
            .url(chrome_url("/overlay.html"))
            .hidpi_scale_factor(Scale::new(self.scale.get()))
            .delegate(delegate)
            .build();
        *self.overlay.borrow_mut() = Some(view);
    }

    pub fn overlay_view(&self) -> Option<WebView> {
        self.overlay.borrow().clone()
    }

    /// Close the suggestions or the command bar.
    pub fn close_overlay(&self) {
        self.overlay_area.set(None);
        self.host.set_layout(self.page_area.get(), None);
        if let Some(ui) = self.chrome_view() {
            ui.evaluate_javascript("window.eonOverlayClosed && eonOverlayClosed()", |_| {});
        }
    }

    /// Ask the interface to open the main menu (`Alt+F`).
    pub fn open_menu(&self) {
        if let Some(ui) = self.chrome_view() {
            ui.evaluate_javascript("window.eonMenu && eonMenu()", |_| {});
        }
    }

    /// A site icon for the interface, PNG, by key.
    pub fn favicon_png(&self, key: u64) -> Option<Vec<u8>> {
        self.favicons.borrow().get(&key).cloned()
    }

    /// The page's icon changed: keep a small PNG of it for the tab strip.
    fn set_favicon(&self, view: &WebView) {
        let png = view.favicon().and_then(|img| crate::favicon::to_png(&img));
        let Some(png) = png else { return };
        let key = self.next_favicon.get();
        self.next_favicon.set(key + 1);
        let old = self
            .with_tab(view, |t| t.favicon.replace(Some(key)))
            .flatten();
        let mut icons = self.favicons.borrow_mut();
        if let Some(old) = old {
            icons.remove(&old);
        }
        icons.insert(key, png);
        drop(icons);
        self.mark_ui();
    }

    /// Ask the interface to open the command bar (`Ctrl+K`).
    pub fn open_palette(&self) {
        if let Some(ui) = self.chrome_view() {
            ui.evaluate_javascript("window.eonPalette && eonPalette()", |_| {});
        }
    }

    pub fn active_view(&self) -> Option<WebView> {
        self.tabs
            .borrow()
            .get(self.active.get())
            .and_then(Tab::view)
    }

    /// Close every view. Each tab holds the browser as its delegate, so the browser (and
    /// Servo inside it) can only be dropped once the views are gone; dropping Servo then
    /// shuts the engine down in order instead of at process exit, where SpiderMonkey
    /// aborts on a mutex its threads still hold.
    pub fn shutdown(&self) {
        self.save_session();
        self.deferred.borrow_mut().clear();
        let tabs = std::mem::take(&mut *self.tabs.borrow_mut());
        drop(tabs);
        let ui = self.ui.borrow_mut().take();
        drop(ui);
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.borrow().len()
    }

    /// Open the tabs of the last session, or `start`, or a new tab.
    /// The route the person chose last time, sent to the route process at start.
    pub fn restore_route(&self) {
        let (kind, address) = {
            let s = self.settings.borrow();
            (s.route.clone(), s.route_address.clone())
        };
        if let (Some(r), false) = (&self.route, kind.is_empty() || kind == "direct") {
            r.set_route(&kind, &address);
        }
        self.apply_limits();
    }

    pub fn open_start(&self, start: Option<Url>) {
        if let Some(url) = start {
            self.new_tab(url, true);
            return;
        }
        if !self.private {
            if let Ok(Some(session)) = self.profile.load_session() {
                if let Some(w) = session.windows.first() {
                    for t in &w.tabs {
                        if let Ok(u) = Url::parse(&t.url) {
                            self.new_tab(u, false);
                        }
                    }
                    if self.tab_count() > 0 {
                        self.select(w.active.min(self.tab_count() - 1));
                        return;
                    }
                }
            }
        }
        self.new_tab(pages::url("/newtab"), true);
    }

    /// A navigation the person started: through cleaning and HTTPS first.
    fn prepare(&self, url: &Url) -> Url {
        if !matches!(url.scheme(), "http" | "https") {
            return url.clone();
        }
        let allowed = self.profile.site(url).is_ok_and(|s| s.http_allowed);
        navigate(url, allowed).url
    }

    fn build_view(&self, url: Url) -> Option<WebView> {
        let me = self.rc()?;
        Some(
            WebViewBuilder::new(&self.servo, self.page_ctx.clone())
                .url(url)
                .hidpi_scale_factor(Scale::new(self.scale.get()))
                .delegate(me as Rc<dyn WebViewDelegate>)
                .user_content_manager(self.ucm.clone())
                .build(),
        )
    }

    pub fn new_tab(&self, url: Url, activate: bool) {
        let url = self.prepare(&url);
        if let Some(view) = self.build_view(url) {
            self.add_tab(view, activate);
        }
    }

    /// Put a background tab to sleep: its page is unloaded, its address and title kept.
    /// Returns whether it was awake.
    pub fn sleep_tab(&self, index: usize) -> bool {
        if index == self.active.get() {
            return false;
        }
        let taken = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(index) else {
                return false;
            };
            let Some(v) = t.view() else {
                return false;
            };
            *t.saved_url.borrow_mut() = self.document_url(&v).or_else(|| v.url());
            *t.saved_title.borrow_mut() = v.page_title();
            let taken = t.view.borrow_mut().take();
            taken
        };
        drop(taken);
        self.mark_ui();
        true
    }

    /// Whether a tab sleeps, and where it is (for the self-test).
    pub fn tab_info(&self, index: usize) -> Option<(bool, Option<Url>)> {
        self.tabs.borrow().get(index).map(|t| (t.asleep(), t.url()))
    }

    /// Wake a sleeping tab: its page loads again where it was.
    fn wake(&self, index: usize) {
        let url = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(index) else { return };
            if !t.asleep() {
                return;
            }
            let url = t.saved_url.borrow().clone();
            url
        };
        let Some(url) = url else { return };
        let Some(view) = self.build_view(url) else {
            return;
        };
        if let Some(t) = self.tabs.borrow().get(index) {
            *t.view.borrow_mut() = Some(view);
        }
    }

    /// Every two seconds: read the monitor, put long-unused tabs to sleep, and enforce the
    /// RAM limit by putting the least recently used background tabs to sleep, one a pass.
    fn housekeeping(&self) {
        if self.last_housekeeping.get().elapsed() < std::time::Duration::from_secs(2) {
            return;
        }
        self.last_housekeeping.set(std::time::Instant::now());
        let route_pid = self.route.as_ref().map(|r| r.pid());
        let bytes = self.route.as_ref().map(|r| r.bytes());
        let sample = self.monitor.borrow_mut().sample(route_pid, bytes);
        self.sample.set(sample);
        let (after, ram_limit) = {
            let s = self.settings.borrow();
            (s.sleep_after(), s.ram_limit_mb)
        };
        let candidates: Vec<(usize, std::time::Instant)> = self
            .tabs
            .borrow()
            .iter()
            .enumerate()
            .filter(|(i, t)| *i != self.active.get() && !t.asleep())
            .map(|(i, t)| (i, t.last_active.get()))
            .collect();
        if let Some(after) = after {
            for (i, at) in &candidates {
                if at.elapsed() > after {
                    self.sleep_tab(*i);
                }
            }
        }
        if ram_limit > 0 && sample.ram_mb > ram_limit {
            if let Some((i, _)) = candidates.iter().min_by_key(|(_, at)| *at) {
                self.sleep_tab(*i);
            }
        }
        self.mark_ui();
    }

    /// Apply the limiter settings to the route process.
    pub fn apply_limits(&self) {
        let (down, up) = {
            let s = self.settings.borrow();
            (s.net_down_kbps, s.net_up_kbps)
        };
        if let Some(r) = &self.route {
            r.set_limits(down, up);
        }
    }

    fn add_tab(&self, view: WebView, activate: bool) {
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.push(Tab {
                view: RefCell::new(Some(view)),
                saved_url: RefCell::new(None),
                saved_title: RefCell::new(None),
                last_active: Cell::new(std::time::Instant::now()),
                blocked: Cell::new(0),
                upgraded_from: RefCell::new(None),
                note: RefCell::new(None),
                allow_once: RefCell::new(None),
                top: RefCell::new(None),
                favicon: Cell::new(None),
            });
            tabs.len() - 1
        };
        if activate || index == 0 {
            self.select(index);
        } else {
            if let Some(v) = self.tabs.borrow()[index].view() {
                v.hide();
            }
            self.mark_ui();
        }
        self.save_session();
    }

    pub fn select(&self, index: usize) {
        if index >= self.tab_count() {
            return;
        }
        self.wake(index);
        {
            let tabs = self.tabs.borrow();
            let now = std::time::Instant::now();
            if let Some(previous) = tabs.get(self.active.get()) {
                previous.last_active.set(now);
            }
            for (i, t) in tabs.iter().enumerate() {
                let Some(v) = t.view() else { continue };
                if i == index {
                    t.last_active.set(now);
                    v.show();
                    v.set_focused(true);
                } else {
                    v.hide();
                    v.set_focused(false);
                }
            }
        }
        self.active.set(index);
        self.update_title();
        self.mark_ui();
        self.host.relayout();
        self.host.redraw();
    }

    pub fn close(&self, index: usize) {
        let removed = {
            let mut tabs = self.tabs.borrow_mut();
            (index < tabs.len()).then(|| tabs.remove(index))
        };
        if let Some(key) = removed.as_ref().and_then(|t| t.favicon.get()) {
            self.favicons.borrow_mut().remove(&key);
        }
        drop(removed);
        let n = self.tab_count();
        if n == 0 {
            self.new_tab(pages::url("/newtab"), true);
            return;
        }
        let active = self.active.get();
        let next = if index < active || active >= n {
            active.saturating_sub(1)
        } else {
            active
        };
        self.select(next.min(n - 1));
        self.save_session();
    }

    /// What the person typed in the bar.
    pub fn go(&self, text: &str) {
        let chosen = self.settings.borrow().search_engine.clone();
        let url = match omnibox::classify(text, &self.engines, chosen.as_deref()) {
            None => return,
            Some(Target::Url(u)) => u,
            Some(Target::Search { engine, query }) => {
                match search::by_id(&self.engines, &engine).and_then(|e| e.results_url(&query)) {
                    Some(u) => u,
                    None => return,
                }
            }
            Some(Target::Internal(page)) => match page.as_str() {
                "blank" => Url::parse("about:blank").unwrap_or_else(|_| pages::url("/newtab")),
                p => pages::url(&format!("/{p}")),
            },
            Some(Target::NeedsEngine { query }) => {
                let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
                pages::url(&format!("/welcome?q={q}"))
            }
        };
        self.load_in_active(url);
    }

    fn load_in_active(&self, url: Url) {
        let url = self.prepare(&url);
        match self.active_view() {
            Some(v) => {
                v.load(url);
                self.host.focus_page();
            }
            None => self.new_tab(url, true),
        }
    }

    pub fn shortcut(&self, s: Shortcut) {
        let view = self.active_view();
        match s {
            Shortcut::NewTab => {
                self.new_tab(pages::url("/newtab"), true);
                self.focus_address();
            }
            Shortcut::CloseTab => self.close(self.active.get()),
            Shortcut::NextTab => {
                let n = self.tab_count().max(1);
                self.select((self.active.get() + 1) % n);
            }
            Shortcut::PreviousTab => {
                let n = self.tab_count().max(1);
                self.select((self.active.get() + n - 1) % n);
            }
            Shortcut::FocusAddress => self.focus_address(),
            Shortcut::Back => {
                if let Some(v) = view.filter(WebView::can_go_back) {
                    v.go_back(1);
                }
            }
            Shortcut::Forward => {
                if let Some(v) = view.filter(WebView::can_go_forward) {
                    v.go_forward(1);
                }
            }
            Shortcut::Reload => {
                if let Some(v) = view {
                    v.reload();
                }
            }
            Shortcut::ZoomIn | Shortcut::ZoomOut | Shortcut::ZoomReset => {
                if let Some(v) = view {
                    let z = match s {
                        Shortcut::ZoomIn => (v.page_zoom() + 0.1).min(5.0),
                        Shortcut::ZoomOut => (v.page_zoom() - 0.1).max(0.3),
                        _ => 1.0,
                    };
                    v.set_page_zoom(z);
                }
            }
            Shortcut::NewPrivateWindow => self.host.open_private_window(),
            Shortcut::NewIdentity => self.new_identity(),
            Shortcut::Bookmark => self.toggle_bookmark(),
            Shortcut::Palette => self.open_palette(),
            Shortcut::Menu => self.open_menu(),
            Shortcut::History => self.new_tab(pages::url("/history"), true),
        }
    }

    /// Tor Browser's New identity: every site gets a new circuit (so a new exit and a new
    /// address), every cookie and stored site data is cleared, and the tabs reload.
    pub fn new_identity(&self) {
        if let Some(r) = &self.route {
            r.new_identity();
        }
        let data = self.servo.site_data_manager();
        data.clear_cookies(None);
        let sites: Vec<String> = data
            .site_data(servo::StorageType::all())
            .iter()
            .map(servo::SiteData::name)
            .collect();
        let names: Vec<&str> = sites.iter().map(String::as_str).collect();
        data.clear_site_data(&names, servo::StorageType::all());
        for t in self.tabs.borrow().iter() {
            if let Some(v) = t.view() {
                v.reload();
            }
        }
        self.with_tab_active_note("New identity: new circuits, cookies cleared");
    }

    fn with_tab_active_note(&self, note: &str) {
        if let Some(t) = self.tabs.borrow().get(self.active.get()) {
            *t.note.borrow_mut() = Some(note.to_string());
        }
        self.mark_ui();
    }

    /// Bookmark the page in the active tab, or remove its bookmark.
    pub fn toggle_bookmark(&self) {
        let Some(view) = self.active_view() else {
            return;
        };
        let Some(url) = self.document_url(&view) else {
            return;
        };
        if !matches!(url.scheme(), "http" | "https") {
            return;
        }
        match self.profile.bookmark_for(&url) {
            Ok(Some(id)) => {
                let _ = self.profile.remove_bookmark(id);
            }
            Ok(None) => {
                let title = view.page_title().unwrap_or_else(|| display(&url));
                let _ = self.profile.add_bookmark(None, &title, &url, now());
            }
            Err(_) => {}
        }
        self.mark_ui();
    }

    pub fn focus_address(&self) {
        if let Some(ui) = self.chrome_view() {
            ui.set_focused(true);
            ui.evaluate_javascript("window.eonFocusAddress && eonFocusAddress()", |_| {});
        }
    }

    fn toggle_shields(&self) {
        let Some(view) = self.active_view() else {
            return;
        };
        let Some(url) = view.url() else { return };
        if let Ok(mut s) = self.profile.site(&url) {
            s.shields = !s.shields;
            let _ = self.profile.set_site(&url, &s);
        }
        view.reload();
    }

    fn tab_index(&self, view: &WebView) -> Option<usize> {
        self.tabs.borrow().iter().position(|t| t.is(view))
    }

    fn with_tab<R>(&self, view: &WebView, f: impl FnOnce(&Tab) -> R) -> Option<R> {
        let tabs = self.tabs.borrow();
        tabs.iter().find(|t| t.is(view)).map(f)
    }

    fn update_title(&self) {
        let title = self
            .active_view()
            .and_then(|v| v.page_title())
            .filter(|t| !t.is_empty());
        let suffix = if self.private {
            "EON Browser (private)"
        } else {
            "EON Browser"
        };
        match title {
            Some(t) => self.host.set_title(&format!("{t} — {suffix}")),
            None => self.host.set_title(suffix),
        }
    }

    fn mark_ui(&self) {
        self.ui_dirty.set(true);
    }

    /// Send the bar the current state, if anything changed. Called after each turn of the
    /// event loop, so a page blocking two hundred requests updates the bar once.
    pub fn flush_ui(&self) {
        self.housekeeping();
        if self.route.as_ref().is_some_and(|r| r.take_changed()) {
            self.ui_dirty.set(true);
        }
        let deferred = std::mem::take(&mut *self.deferred.borrow_mut());
        for (view, script) in deferred {
            view.evaluate_javascript(script, |_| {});
        }
        if !self.ui_dirty.replace(false) {
            return;
        }
        let Some(ui) = self.chrome_view() else { return };
        let json = serde_json::to_string(&self.ui_state()).unwrap_or_else(|_| "null".into());
        ui.evaluate_javascript(format!("window.eonState && eonState({json})"), |_| {});
    }

    fn ui_state(&self) -> UiState {
        let tabs = self.tabs.borrow();
        let active = self.active.get().min(tabs.len().saturating_sub(1));
        let t = tabs.get(active);
        let url = t.and_then(Tab::url);
        let own = url
            .as_ref()
            .is_some_and(|u| u.host_str() == Some(pages::HOST));
        let shields = url
            .as_ref()
            .filter(|_| !own)
            .and_then(|u| self.profile.site(u).ok())
            .is_none_or(|s| s.shields);
        UiState {
            tabs: tabs
                .iter()
                .map(|t| UiTab {
                    title: t.title().unwrap_or_default(),
                    url: t.url().map(|u| display(&u)).unwrap_or_default(),
                    loading: t
                        .view()
                        .is_some_and(|v| v.load_status() != LoadStatus::Complete),
                    asleep: t.asleep(),
                    icon: t.favicon.get().map(|k| format!("favicon/{k}.png")),
                })
                .collect(),
            active,
            display: url
                .as_ref()
                .filter(|_| !own)
                .map(display)
                .unwrap_or_default(),
            can_back: t.and_then(Tab::view).is_some_and(|v| v.can_go_back()),
            can_forward: t.and_then(Tab::view).is_some_and(|v| v.can_go_forward()),
            blocked: t.map_or(0, |t| t.blocked.get()),
            shields,
            security: match url.as_ref() {
                Some(_) if own => "page",
                Some(u) if u.scheme() == "https" => "secure",
                Some(u) if u.scheme() == "http" => "open",
                _ => "page",
            },
            note: t.and_then(|t| t.note.borrow().clone()),
            private: self.private,
            route: self
                .route
                .as_ref()
                .map_or_else(|| "none".to_string(), |r| r.status().kind),
            tor: self
                .route
                .as_ref()
                .map_or_else(|| "off".to_string(), |r| r.status().tor),
            bookmarked: url
                .as_ref()
                .is_some_and(|u| matches!(self.profile.bookmark_for(u), Ok(Some(_)))),
            monitor: self.sample.get(),
            limits: {
                let s = self.settings.borrow();
                UiLimits {
                    sleep_after_minutes: s.sleep_after_minutes.unwrap_or(15),
                    ram_limit_mb: s.ram_limit_mb,
                    net_down_kbps: s.net_down_kbps,
                    net_up_kbps: s.net_up_kbps,
                }
            },
            route_address: self.settings.borrow().route_address.clone(),
        }
    }

    fn suggestions(&self, text: &str) -> Vec<UiSuggestion> {
        let mut out = Vec::new();
        let chosen = self.settings.borrow().search_engine.clone();
        match omnibox::classify(text, &self.engines, chosen.as_deref()) {
            Some(Target::Url(u)) => out.push(UiSuggestion {
                kind: "url",
                title: display(&u),
                detail: "Open".into(),
                go: text.to_string(),
            }),
            Some(Target::Search { engine, query }) => out.push(UiSuggestion {
                kind: "search",
                title: query,
                detail: search::by_id(&self.engines, &engine)
                    .map_or_else(String::new, |e| format!("Search with {}", e.name)),
                go: text.to_string(),
            }),
            Some(Target::NeedsEngine { query }) => out.push(UiSuggestion {
                kind: "search",
                title: query,
                detail: "Choose a search engine".into(),
                go: text.to_string(),
            }),
            Some(Target::Internal(p)) => out.push(UiSuggestion {
                kind: "url",
                title: format!("eon:{p}"),
                detail: "EON Browser page".into(),
                go: text.to_string(),
            }),
            None => {}
        }
        if let Ok(found) = self.profile.suggest(text, 6) {
            for s in found {
                out.push(UiSuggestion {
                    kind: "history",
                    title: s.title.clone().unwrap_or_else(|| s.url.clone()),
                    detail: s.url.clone(),
                    go: s.url,
                });
            }
        }
        out
    }

    fn save_session(&self) {
        if self.private {
            return;
        }
        let tabs = self.tabs.borrow();
        let session = Session {
            windows: vec![WindowState {
                tabs: tabs
                    .iter()
                    .filter_map(Tab::url)
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                    .map(|u| SavedTab {
                        url: u.to_string(),
                        ..SavedTab::default()
                    })
                    .collect(),
                active: self.active.get(),
            }],
        };
        let _ = self.profile.save_session(&session, now());
    }

    /// Act on an own page's link (`/choose`, `/proceed`, `/allow-http`).
    fn act(&self, view: &WebView, url: &Url) {
        let q = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        match url.path() {
            "/choose" => {
                let id = q("engine");
                if search::by_id(&self.engines, &id).is_some() {
                    self.settings.borrow_mut().search_engine = Some(id);
                    if !self.private {
                        let _ = self.settings.borrow().save(&self.data);
                    }
                }
                let query = q("q");
                if query.is_empty() {
                    view.load(pages::url("/newtab"));
                } else {
                    self.go(&query);
                }
            }
            "/proceed" => {
                if let Ok(target) = Url::parse(&q("url")) {
                    self.with_tab(view, |t| *t.allow_once.borrow_mut() = Some(target.clone()));
                    view.load(target);
                }
            }
            "/set-route" => {
                let kind = q("kind");
                let address = q("address");
                if let Some(r) = &self.route {
                    r.set_route(&kind, &address);
                }
                {
                    let mut s = self.settings.borrow_mut();
                    s.route = kind;
                    s.route_address = address;
                }
                if !self.private {
                    let _ = self.settings.borrow().save(&self.data);
                }
                view.load(pages::url("/route"));
            }
            "/ext-toggle" | "/ext-remove" => {
                let id = q("id");
                let result = Lists::load(&self.data.join("lists"))
                    .map_err(|e| e.to_string())
                    .and_then(|mut l| {
                        let mut ext = self.extensions.borrow_mut();
                        if url.path() == "/ext-remove" {
                            ext.remove(&id, &mut l)
                        } else {
                            let on = !ext.list.iter().any(|i| i.id == id && i.enabled);
                            ext.set_enabled(&id, on, &mut l)
                        }
                    });
                if result.is_ok() {
                    self.rebuild_shield();
                }
                view.load(pages::url("/extensions"));
            }
            "/clear-history" => {
                let since = match q("since").as_str() {
                    "hour" => now().saturating_sub(3600),
                    "today" => now() - now() % 86_400,
                    _ => 0,
                };
                let _ = self.profile.clear_history(since);
                view.load(pages::url("/history"));
            }
            "/welcomed" => {
                self.settings.borrow_mut().welcomed = true;
                if !self.private {
                    let _ = self.settings.borrow().save(&self.data);
                }
                view.load(pages::url("/newtab"));
            }
            "/unbookmark" => {
                if let Ok(id) = q("id").parse::<i64>() {
                    let _ = self.profile.remove_bookmark(id);
                }
                view.load(pages::url("/bookmarks"));
            }
            "/new-identity" => {
                self.new_identity();
                view.load(pages::url("/route"));
            }
            "/allow-http" => {
                if let Ok(target) = Url::parse(&q("url")) {
                    if let Ok(mut s) = self.profile.site(&target) {
                        s.http_allowed = true;
                        let _ = self.profile.set_site(&target, &s);
                    }
                    view.load(target);
                }
            }
            _ => {}
        }
    }

    /// Recompile the shield after the lists or an extension's rules changed.
    fn rebuild_shield(&self) {
        let shield = build_shield(&self.data, self.rules_override.as_deref());
        *self.shield.borrow_mut() = shield;
    }

    /// Run the enabled extensions' content scripts and styles for `url` at `stage`.
    fn run_extensions(&self, view: &WebView, url: &Url, stage: crate::extensions::Stage) {
        if !matches!(url.scheme(), "http" | "https") || url.host_str() == Some(pages::HOST) {
            return;
        }
        for script in self.extensions.borrow().scripts_for(url, stage) {
            view.evaluate_javascript(script, |_| {});
        }
    }

    /// The address of the document a view shows now: recorded when its main-frame request
    /// arrived, falling back to `WebView::url`.
    fn document_url(&self, view: &WebView) -> Option<Url> {
        self.with_tab(view, |t| t.top.borrow().clone())
            .flatten()
            .or_else(|| view.url())
    }

    fn after_load(&self, view: &WebView) {
        let Some(url) = self.document_url(view) else {
            return;
        };
        self.run_extensions(view, &url, crate::extensions::Stage::End);
        // HTTPS-only: an upgraded load that failed offers HTTP, for this site, by choice.
        let upgraded = self
            .with_tab(view, |t| t.upgraded_from.borrow_mut().take())
            .flatten();
        if let Some(original) = upgraded {
            let failed = view.page_title().as_deref() == Some("Error loading page");
            if failed && url.scheme() == "https" {
                let q: String =
                    url::form_urlencoded::byte_serialize(original.as_str().as_bytes()).collect();
                view.load(pages::url(&format!("/https-failed?url={q}")));
                return;
            }
        }
        if !matches!(url.scheme(), "http" | "https") || url.host_str() == Some(pages::HOST) {
            return;
        }
        if !self.private {
            let _ = self
                .profile
                .visit(&url, view.page_title().as_deref(), now());
        }
        self.save_session();
        // Element hiding, second step: generic rules for the classes and ids on the page.
        let on = self.profile.site(&url).map_or(true, |s| s.shields);
        if on {
            let cosmetic = self.shield.borrow().cosmetic(url.as_str());
            if !cosmetic.generichide {
                let weak = self.me.borrow().clone();
                let v = view.clone();
                view.evaluate_javascript(shield::COLLECT_CLASSES_AND_IDS, move |r| {
                    let Some(b) = weak.upgrade() else { return };
                    let (classes, ids) = names(r);
                    let css = b.shield.borrow().generic_css(&classes, &ids, &cosmetic);
                    if !css.is_empty() {
                        b.deferred.borrow_mut().push((v, inject_css(&css)));
                    }
                });
            }
        }
    }

    /// Answer an own page.
    fn serve_page(&self, load: WebResourceLoad, url: &Url) {
        if url.path() == "/fonts/Geist-Variable.woff2" {
            respond(load, StatusCode::OK, "font/woff2", crate::ui::FONT.to_vec());
            return;
        }
        if url.path() == "/history" {
            let q = url
                .query_pairs()
                .find(|(k, _)| k == "q")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            let html = if q.trim().is_empty() {
                pages::history(
                    &self.profile.history(300, None).unwrap_or_default(),
                    self.private,
                )
            } else {
                pages::history_search(&q, &self.profile.suggest(&q, 100).unwrap_or_default())
            };
            respond(
                load,
                StatusCode::OK,
                "text/html; charset=utf-8",
                html.into_bytes(),
            );
            return;
        }
        if url.path() == "/bookmarks" {
            let html = pages::bookmarks(&self.profile.bookmarks(None).unwrap_or_default());
            respond(
                load,
                StatusCode::OK,
                "text/html; charset=utf-8",
                html.into_bytes(),
            );
            return;
        }
        if url.path() == "/extensions" {
            let html = pages::extensions(&self.extensions.borrow().list);
            respond(
                load,
                StatusCode::OK,
                "text/html; charset=utf-8",
                html.into_bytes(),
            );
            return;
        }
        if url.path() == "/route" {
            let status = self.route.as_ref().map(|r| r.status());
            let html = pages::route(status.as_ref());
            respond(
                load,
                StatusCode::OK,
                "text/html; charset=utf-8",
                html.into_bytes(),
            );
            return;
        }
        let chosen = self.settings.borrow().search_engine.clone();
        let welcomed = self.settings.borrow().welcomed;
        match pages::render(url, &self.engines, chosen.as_deref(), VERSION, welcomed) {
            Some(html) => respond(
                load,
                StatusCode::OK,
                "text/html; charset=utf-8",
                html.into_bytes(),
            ),
            None => respond(
                load,
                StatusCode::NOT_FOUND,
                "text/plain",
                b"Not found".to_vec(),
            ),
        }
    }

    #[allow(clippy::too_many_lines)] // one match over the interface's commands, each short
    fn command(&self, c: &serde_json::Value) -> serde_json::Value {
        let op = c
            .get("op")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let tab = c
            .get("tab")
            .and_then(serde_json::Value::as_u64)
            .map(|t| t as usize);
        let text = c
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        match op {
            "ready" => return serde_json::to_value(self.ui_state()).unwrap_or_default(),
            "go" => self.go(text),
            "back" => self.shortcut(Shortcut::Back),
            "forward" => self.shortcut(Shortcut::Forward),
            "reload" => self.shortcut(Shortcut::Reload),
            "newTab" => self.shortcut(Shortcut::NewTab),
            "close" => self.close(tab.unwrap_or(self.active.get())),
            "select" => {
                if let Some(t) = tab {
                    self.select(t);
                }
            }
            "shields" => self.toggle_shields(),
            "suggest" => {
                return serde_json::json!({ "items": self.suggestions(text) });
            }
            "layout" => {
                if let Ok(a) =
                    serde_json::from_value::<Area>(c.get("page").cloned().unwrap_or_default())
                {
                    self.page_area.set(a);
                    self.host.set_layout(a, self.overlay_area.get());
                }
                return serde_json::json!({ "ok": true });
            }
            "overlay" => {
                let mode = c
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("none");
                let area =
                    serde_json::from_value::<Area>(c.get("area").cloned().unwrap_or_default()).ok();
                if mode == "none" || area.is_none() {
                    self.overlay_area.set(None);
                    self.host.set_layout(self.page_area.get(), None);
                    return serde_json::json!({ "ok": true });
                }
                self.overlay_area.set(area);
                self.host.set_layout(self.page_area.get(), area);
                if let Some(o) = self.overlay_view() {
                    let json = serde_json::to_string(c).unwrap_or_else(|_| "null".into());
                    self.deferred
                        .borrow_mut()
                        .push((o, format!("window.eonOverlay && eonOverlay({json})")));
                }
                if mode == "palette" || mode == "menu" {
                    self.host.focus_overlay();
                }
                return serde_json::json!({ "ok": true });
            }
            "closeOverlay" => {
                self.close_overlay();
                if c.get("focusPage").and_then(serde_json::Value::as_bool) == Some(true) {
                    self.host.focus_page();
                } else {
                    self.host.focus_chrome();
                }
            }
            "panel" => {
                let name = c
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if let Some(ui) = self.chrome_view() {
                    let lit = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
                    self.deferred
                        .borrow_mut()
                        .push((ui, format!("window.eonOpenPanel && eonOpenPanel({lit})")));
                }
            }
            "open" => {
                let page = c
                    .get("page")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("newtab");
                if page
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                {
                    self.new_tab(pages::url(&format!("/{page}")), true);
                }
            }
            "privateWindow" => self.host.open_private_window(),
            "palette" => self.open_palette(),
            "zoom" => match c.get("to").and_then(serde_json::Value::as_str) {
                Some("in") => self.shortcut(Shortcut::ZoomIn),
                Some("out") => self.shortcut(Shortcut::ZoomOut),
                _ => self.shortcut(Shortcut::ZoomReset),
            },
            "focusPage" => self.host.focus_page(),
            "routePage" => self.new_tab(pages::url("/route"), true),
            "bookmark" => self.toggle_bookmark(),
            "sleepTab" => {
                if let Some(t) = tab {
                    self.sleep_tab(t);
                }
            }
            "newIdentity" => self.new_identity(),
            "sleepOthers" => {
                for i in 0..self.tab_count() {
                    self.sleep_tab(i);
                }
            }
            "setRoute" => {
                let kind = c
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("direct");
                let address = c
                    .get("address")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if let Some(r) = &self.route {
                    r.set_route(kind, address);
                }
                {
                    let mut s = self.settings.borrow_mut();
                    s.route = kind.to_string();
                    s.route_address = address.to_string();
                }
                if !self.private {
                    let _ = self.settings.borrow().save(&self.data);
                }
            }
            "setLimits" => {
                let n = |k: &str| {
                    c.get(k)
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| v as u32)
                };
                {
                    let mut s = self.settings.borrow_mut();
                    if let Some(v) = n("sleepAfterMinutes") {
                        s.sleep_after_minutes = Some(v);
                    }
                    if let Some(v) = n("ramLimitMb") {
                        s.ram_limit_mb = v;
                    }
                    if let Some(v) = n("netDownKbps") {
                        s.net_down_kbps = v;
                    }
                    if let Some(v) = n("netUpKbps") {
                        s.net_up_kbps = v;
                    }
                }
                if !self.private {
                    let _ = self.settings.borrow().save(&self.data);
                }
                self.apply_limits();
            }
            _ => return serde_json::json!({ "ok": false }),
        }
        self.mark_ui();
        serde_json::json!({ "ok": true })
    }
}

/// `{classes: [...], ids: [...]}` from the page.
fn names(r: Result<JSValue, servo::JavaScriptEvaluationError>) -> (Vec<String>, Vec<String>) {
    let strings = |v: Option<&JSValue>| -> Vec<String> {
        match v {
            Some(JSValue::Array(items)) => items
                .iter()
                .filter_map(|i| match i {
                    JSValue::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    match r {
        Ok(JSValue::Object(map)) => (strings(map.get("classes")), strings(map.get("ids"))),
        _ => (Vec::new(), Vec::new()),
    }
}

/// An address as the bar shows it: without `https://`, and without a lone trailing `/`.
pub fn display(u: &Url) -> String {
    let s = u.as_str();
    let s = s.strip_prefix("https://").unwrap_or(s);
    match s.strip_suffix('/') {
        Some(t) if u.path() == "/" && u.query().is_none() => t.to_string(),
        _ => s.to_string(),
    }
}

fn chrome_url(path: &str) -> Url {
    Url::parse(&format!("https://{UI_HOST}{path}")).unwrap_or_else(|_| pages::url("/newtab"))
}

#[derive(Serialize)]
struct UiTab {
    title: String,
    url: String,
    loading: bool,
    asleep: bool,
    /// The site's icon, a path under the interface's own host.
    icon: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UiLimits {
    sleep_after_minutes: u32,
    ram_limit_mb: u32,
    net_down_kbps: u32,
    net_up_kbps: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // what the bar shows, field by field
struct UiState {
    tabs: Vec<UiTab>,
    active: usize,
    display: String,
    can_back: bool,
    can_forward: bool,
    blocked: u32,
    shields: bool,
    security: &'static str,
    note: Option<String>,
    private: bool,
    /// `direct`, `tor`, `proxy`, or `none` when the route process is unavailable.
    route: String,
    /// `off`, `connecting`, `ready`, `failed`.
    tor: String,
    /// The page in the active tab is bookmarked.
    bookmarked: bool,
    monitor: crate::monitor::Sample,
    limits: UiLimits,
    route_address: String,
}

#[derive(Serialize)]
struct UiSuggestion {
    kind: &'static str,
    title: String,
    detail: String,
    go: String,
}

impl WebViewDelegate for Browser {
    fn notify_favicon_changed(&self, view: WebView) {
        self.set_favicon(&view);
    }

    fn notify_new_frame_ready(&self, view: WebView) {
        if self.active_view().is_some_and(|a| a.id() == view.id()) {
            self.host.redraw();
        }
    }

    fn notify_url_changed(&self, _view: WebView, _url: Url) {
        self.mark_ui();
    }

    fn notify_page_title_changed(&self, view: WebView, title: Option<String>) {
        if let (Some(url), Some(t), false) = (view.url(), title, self.private) {
            let _ = self.profile.set_title(&url, &t);
        }
        if self.active_view().is_some_and(|a| a.id() == view.id()) {
            self.update_title();
        }
        self.mark_ui();
    }

    fn notify_load_status_changed(&self, view: WebView, status: LoadStatus) {
        match status {
            // The blocked count is reset by the main-frame request (load_web_resource),
            // which always precedes the page's own requests; `Started` can arrive after
            // the first of them were blocked (seen on macOS).
            LoadStatus::Started | LoadStatus::Stopped => {}
            LoadStatus::HeadParsed => {
                // Element hiding, first step: the rules written for this site, as early as
                // the page has a head to put them in. The document's own address, not
                // WebView::url, which can still name the previous page at this point.
                if let Some(url) = self.document_url(&view) {
                    let on = self.profile.site(&url).map_or(true, |s| s.shields);
                    if on && matches!(url.scheme(), "http" | "https") {
                        let css = self.shield.borrow().cosmetic(url.as_str()).css;
                        if !css.is_empty() {
                            view.evaluate_javascript(inject_css(&css), |_| {});
                        }
                    }
                    self.run_extensions(&view, &url, crate::extensions::Stage::Start);
                }
            }
            LoadStatus::Complete => self.after_load(&view),
        }
        self.mark_ui();
    }

    fn notify_history_changed(&self, _view: WebView, _entries: Vec<Url>, _current: usize) {
        self.mark_ui();
    }

    fn notify_cursor_changed(&self, view: WebView, cursor: Cursor) {
        if self.active_view().is_some_and(|a| a.id() == view.id()) {
            self.host.set_cursor(cursor);
        }
    }

    fn notify_closed(&self, view: WebView) {
        if let Some(i) = self.tab_index(&view) {
            self.close(i);
        }
    }

    fn notify_crashed(&self, view: WebView, reason: String, _backtrace: Option<String>) {
        self.with_tab(&view, |t| {
            *t.note.borrow_mut() = Some(format!("This page crashed: {reason}"));
        });
        self.mark_ui();
    }

    fn request_navigation(&self, view: WebView, request: NavigationRequest) {
        let url = request.url.clone();
        match url.host_str() {
            Some(UI_HOST) => {
                request.deny();
                return;
            }
            Some(pages::HOST) => {
                if pages::is_action(url.path()) {
                    let from_own = view
                        .url()
                        .is_some_and(|u| u.host_str() == Some(pages::HOST));
                    request.deny();
                    if from_own {
                        self.act(&view, &url);
                    }
                } else {
                    request.allow();
                }
                return;
            }
            _ => {}
        }
        if !matches!(url.scheme(), "http" | "https") {
            request.allow();
            return;
        }
        let settings = self.profile.site(&url).unwrap_or_default();
        let nav = navigate(&url, settings.http_allowed);
        if nav.changed {
            let note = nav.cleaned.as_ref().and_then(|c| {
                let n = c.removed.len();
                match (n, &c.unwrapped_from) {
                    (0, Some(_)) => Some("Redirect skipped".to_string()),
                    (0, None) => None,
                    (1, _) => Some("1 tracker removed from the link".to_string()),
                    (n, _) => Some(format!("{n} trackers removed from the link")),
                }
            });
            let upgraded_from = match nav.upgrade {
                Upgrade::Upgraded(_) => {
                    Some(nav.cleaned.as_ref().map_or(url.clone(), |c| c.url.clone()))
                }
                _ => None,
            };
            self.with_tab(&view, |t| {
                *t.note.borrow_mut() = note;
                *t.upgraded_from.borrow_mut() = upgraded_from;
            });
            request.deny();
            view.load(nav.url);
            self.mark_ui();
            return;
        }
        // A whole site named harmful by a list: warn, with a way through.
        let passed = self
            .with_tab(&view, |t| t.allow_once.borrow_mut().take())
            .flatten()
            .is_some_and(|u| u == url);
        if settings.shields && !passed {
            let verdict = self.shield.borrow().check_document(url.as_str());
            if let Verdict::Block { .. } = verdict {
                let q: String =
                    url::form_urlencoded::byte_serialize(url.as_str().as_bytes()).collect();
                request.deny();
                view.load(pages::url(&format!("/blocked?url={q}")));
                return;
            }
        }
        self.with_tab(&view, |t| {
            if t.upgraded_from
                .borrow()
                .as_ref()
                .is_some_and(|u| u.host_str() != url.host_str())
            {
                *t.upgraded_from.borrow_mut() = None;
            }
        });
        request.allow();
    }

    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        let Some(me) = self.rc() else { return };
        let view = request
            .builder(self.page_ctx.clone())
            .hidpi_scale_factor(Scale::new(self.scale.get()))
            .delegate(me as Rc<dyn WebViewDelegate>)
            .user_content_manager(self.ucm.clone())
            .build();
        self.add_tab(view, true);
    }

    fn request_permission(&self, _view: WebView, request: PermissionRequest) {
        // v0.0.0 asks nothing and grants nothing: camera, microphone, location and
        // notifications are refused until the permission prompt exists (plan v0.0.2).
        request.deny();
    }

    fn load_web_resource(&self, view: WebView, load: WebResourceLoad) {
        let req = load.request();
        let url = req.url.clone().into_url();
        let main_frame = req.is_for_main_frame;
        if main_frame {
            self.with_tab(&view, |t| {
                *t.top.borrow_mut() = Some(url.clone());
                t.blocked.set(0);
            });
        }
        match url.host_str() {
            Some(UI_HOST) => return refuse(load),
            Some(pages::HOST) => return self.serve_page(load, &url),
            Some(crate::extensions::HOST) => {
                return match self.extensions.borrow().serve(&url) {
                    Some((mime, body)) => respond(load, StatusCode::OK, mime, body),
                    None => respond(load, StatusCode::NOT_FOUND, "text/plain", Vec::new()),
                };
            }
            _ => {}
        }
        let dest = format!("{:?}", req.destination).to_ascii_lowercase();
        let page = self
            .with_tab(&view, |t| t.top.borrow().clone())
            .flatten()
            .or_else(|| view.url())
            .unwrap_or_else(|| url.clone());
        let on = self.profile.site(&page).map_or(true, |s| s.shields)
            && page.host_str() != Some(pages::HOST);
        if on && !main_frame {
            let kind = Kind::from_destination(if dest == "none" { "empty" } else { &dest });
            let verdict = self.shield.borrow().check(&Request {
                url: url.as_str(),
                source: page.as_str(),
                kind,
            });
            if verdict != Verdict::Allow {
                // For finding what a site needs that a list blocks; never on by default.
                if std::env::var_os("EON_BROWSER_LOG_BLOCKS").is_some() {
                    eprintln!("blocked {kind:?} {url} (page {page}) {verdict:?}");
                }
                self.with_tab(&view, |t| t.blocked.set(t.blocked.get() + 1));
                self.mark_ui();
                return refuse(load);
            }
        }
        if matches!(url.scheme(), "http" | "https") {
            self.requests.borrow_mut().push(url.to_string());
        }
        if let Some(fixtures) = &self.fixtures {
            match fixtures.get(&url) {
                Some((mime, body)) => respond(load, StatusCode::OK, mime, body.clone()),
                None => respond(
                    load,
                    StatusCode::NOT_FOUND,
                    "text/plain",
                    b"no fixture".to_vec(),
                ),
            }
        }
        // Otherwise `load` is dropped, and the request goes to the network as it is.
    }
}

/// The bar's delegate: it may load only its own files, and its requests to `/cmd` are
/// the commands it gives the browser.
struct Chrome {
    browser: Weak<Browser>,
}

impl WebViewDelegate for Chrome {
    fn notify_new_frame_ready(&self, _view: WebView) {
        if let Some(b) = self.browser.upgrade() {
            b.host.redraw();
        }
    }

    fn notify_cursor_changed(&self, _view: WebView, cursor: Cursor) {
        if let Some(b) = self.browser.upgrade() {
            b.host.set_cursor(cursor);
        }
    }

    fn request_navigation(&self, _view: WebView, request: NavigationRequest) {
        if request.url.host_str() == Some(UI_HOST) {
            request.allow();
        } else {
            request.deny();
        }
    }

    fn load_web_resource(&self, _view: WebView, load: WebResourceLoad) {
        let url = load.request().url.clone().into_url();
        if url.host_str() != Some(UI_HOST) {
            return refuse(load);
        }
        if let Some(key) = url
            .path()
            .strip_prefix("/favicon/")
            .and_then(|k| k.strip_suffix(".png"))
            .and_then(|k| k.parse::<u64>().ok())
        {
            return match self.browser.upgrade().and_then(|b| b.favicon_png(key)) {
                Some(png) => respond(load, StatusCode::OK, "image/png", png),
                None => respond(load, StatusCode::NOT_FOUND, "text/plain", Vec::new()),
            };
        }
        if url.path() == "/cmd" {
            let Some(b) = self.browser.upgrade() else {
                return refuse(load);
            };
            let c = url
                .query_pairs()
                .find(|(k, _)| k == "c")
                .and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok())
                .unwrap_or_default();
            let reply = b.command(&c);
            let body = serde_json::to_vec(&reply).unwrap_or_default();
            return respond(load, StatusCode::OK, "application/json", body);
        }
        match crate::ui::asset(url.path()) {
            Some((mime, body)) => respond(load, StatusCode::OK, mime, body.to_vec()),
            None => respond(
                load,
                StatusCode::NOT_FOUND,
                "text/plain",
                b"Not found".to_vec(),
            ),
        }
    }
}
