// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! `eon-browser --selftest <report>`: the real engine, offscreen, against pages the browser
//! serves itself — nothing leaves the computer. It checks what madde 50 promises, through
//! the same code a window uses: blocking, element hiding, link cleaning, HTTPS-only, the
//! user agent, Global Privacy Control, the bar, the own pages and their protections,
//! keyboard and wheel input. Screenshots of the bar and the page are written beside the
//! report. A non-zero exit fails a release.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use eon_browser_core::fingerprint::{self, Platform};
use servo::{
    Code, Cursor, DevicePoint, EventLoopWaker, InputEvent, JSValue, Key, KeyState, KeyboardEvent,
    Location, Modifiers, RenderingContext, SoftwareRenderingContext, WebView, WebViewPoint,
    WheelDelta, WheelEvent, WheelMode,
};
use url::Url;

use crate::browser::{Browser, Fixtures, Host, Options};

#[derive(Clone)]
struct Waker(Sender<()>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send(());
    }
}

#[derive(Default)]
struct Headless {
    redraw: Rc<Cell<bool>>,
    page: Rc<Cell<Option<crate::browser::Area>>>,
    overlay: Rc<Cell<Option<crate::browser::Area>>>,
}

impl Host for Headless {
    fn redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, _cursor: Cursor) {}
    fn set_title(&self, _title: &str) {}
    fn set_layout(&self, page: crate::browser::Area, overlay: Option<crate::browser::Area>) {
        self.page.set(Some(page));
        self.overlay.set(overlay);
    }
    fn focus_page(&self) {}
}

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0,
    0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

const SITE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Self-test site</title>
<link rel="icon" href="https://site.example/logo.png">
<style>body{font:16px sans-serif;margin:24px;background:#fff;color:#222} .tall{height:3000px}</style></head>
<body>
<h1>A page with trackers</h1>
<img id="ad" src="https://ads.tracker.example/banner.png" width="40" height="40">
<img id="ok" src="https://site.example/logo.png" width="40" height="40">
<div class="ad-slot">AD SLOT</div>
<div class="cookie-banner">We value your privacy</div>
<div class="ext-hide">hidden by an extension's style</div>
<img id="exttrk" src="https://ext-tracker.example/p.png" width="10" height="10">
<img id="extwar" src="https://ext.eon.invalid/eon-self-test-extension/img/open.png" width="10" height="10">
<img id="extpriv" src="https://ext.eon.invalid/eon-self-test-extension/private.png" width="10" height="10">
<p><input id="box"></p>
<p><a id="out" href="http://site.example/read?fbclid=abc&amp;keep=1">read</a>
<a id="evil" href="https://evil.example/">evil</a>
<a id="sneaky" href="https://page.eon.invalid/choose?engine=bing">sneaky</a></p>
<div class="tall"></div>
</body></html>"#;

const READ: &str = "<!doctype html><title>Read</title><p>The read page.</p>";

const RULES: &str =
    "||ads.tracker.example^\nsite.example##.ad-slot\n##.cookie-banner\n||evil.example^$document\n";

struct Run {
    browser: Rc<Browser>,
    rx: Receiver<()>,
    redraw: Rc<Cell<bool>>,
    _contexts: (
        Rc<SoftwareRenderingContext>,
        Rc<SoftwareRenderingContext>,
        Rc<SoftwareRenderingContext>,
    ),
    page_area: Rc<Cell<Option<crate::browser::Area>>>,
    overlay_area: Rc<Cell<Option<crate::browser::Area>>>,
}

impl Run {
    fn pump_until(&self, mut done: impl FnMut() -> bool, limit: Duration) -> bool {
        let start = Instant::now();
        loop {
            let _ = self.rx.recv_timeout(Duration::from_millis(10));
            self.browser.servo().spin_event_loop();
            self.browser.flush_ui();
            if self.redraw.replace(false) {
                if let Some(v) = self.browser.active_view() {
                    v.paint();
                }
                if let Some(v) = self.browser.chrome_view() {
                    v.paint();
                }
                if let Some(v) = self.browser.overlay_view() {
                    v.paint();
                }
            }
            if done() {
                return true;
            }
            if start.elapsed() > limit {
                return false;
            }
        }
    }

    fn settle(&self, ms: u64) {
        self.pump_until(|| false, Duration::from_millis(ms));
    }

    fn eval(&self, view: &WebView, script: &str) -> Option<JSValue> {
        let out: Rc<RefCell<Option<JSValue>>> = Rc::new(RefCell::new(None));
        let o = out.clone();
        let got = Rc::new(Cell::new(false));
        let g = got.clone();
        view.evaluate_javascript(script, move |r| {
            *o.borrow_mut() = r.ok();
            g.set(true);
        });
        self.pump_until(|| got.get(), Duration::from_secs(10));
        let value = out.borrow_mut().take();
        value
    }

    fn screenshot(&self, view: &WebView) -> Option<servo::RgbaImage> {
        let out: Rc<RefCell<Option<servo::RgbaImage>>> = Rc::new(RefCell::new(None));
        let done = Rc::new(Cell::new(false));
        let (o, d) = (out.clone(), done.clone());
        view.take_screenshot(None, move |r| {
            *o.borrow_mut() = r.ok();
            d.set(true);
        });
        self.pump_until(|| done.get(), Duration::from_secs(20));
        let image = out.borrow_mut().take();
        image
    }

    fn page(&self) -> Option<WebView> {
        self.browser.active_view()
    }

    fn url(&self) -> String {
        self.page()
            .and_then(|v| v.url())
            .map(|u| u.to_string())
            .unwrap_or_default()
    }

    fn wait_url(&self, want: &str) -> bool {
        self.pump_until(
            || {
                self.page().is_some_and(|v| {
                    v.url().is_some_and(|u| u.as_str() == want)
                        && v.load_status() == servo::LoadStatus::Complete
                })
            },
            Duration::from_secs(20),
        )
    }

    fn page_eval(&self, script: &str) -> Option<JSValue> {
        let v = self.page()?;
        self.eval(&v, script)
    }

    fn ui_eval(&self, script: &str) -> Option<JSValue> {
        let v = self.browser.chrome_view()?;
        self.eval(&v, script)
    }

    fn overlay_eval(&self, script: &str) -> Option<JSValue> {
        let v = self.browser.overlay_view()?;
        self.eval(&v, script)
    }
}

fn s(v: Option<JSValue>) -> String {
    match v {
        Some(JSValue::String(s)) => s,
        Some(JSValue::Number(n)) => n.to_string(),
        Some(JSValue::Boolean(b)) => b.to_string(),
        other => format!("{other:?}"),
    }
}

fn key(view: &WebView, c: &str, code: Code) {
    for state in [KeyState::Down, KeyState::Up] {
        view.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
            state,
            Key::Character(c.into()),
            code,
            Location::Standard,
            Modifiers::empty(),
            false,
            false,
        )));
    }
}

struct Report {
    text: String,
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: &str) {
        let _ = writeln!(
            self.text,
            "{} {name}{}",
            if ok { "ok  " } else { "FAIL" },
            if detail.is_empty() {
                String::new()
            } else {
                format!(" — {detail}")
            }
        );
        if !ok {
            self.failed += 1;
        }
    }
}

/// Run the self-test; returns the number of failed checks.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub fn run(report_path: &Path) -> Result<u32, String> {
    crate::engine::install_crypto();
    let data = std::env::temp_dir().join(format!("eon-browser-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let (tx, rx) = channel();
    let ui_ctx = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(1280, 720))
            .map_err(|e| format!("{e:?}"))?,
    );
    let overlay_ctx = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(560, 400)).map_err(|e| format!("{e:?}"))?,
    );
    let page_ctx = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(1280, 720))
            .map_err(|e| format!("{e:?}"))?,
    );
    let _ = page_ctx.make_current();
    // The route process, as the window starts it (its Tor folder is throw-away too).
    let route = crate::route_client::RouteClient::start(&data.join("tor")).map(std::sync::Arc::new);
    // Cookies and storage in memory; history and settings in a throw-away folder.
    let servo = crate::engine::builder(
        true,
        &data,
        route.as_ref().map(|r| r.proxy_uri()),
        Box::new(Waker(tx)),
    )
    .build();
    let mut fixtures = Fixtures::default();
    fixtures.0.insert(
        "site.example/".into(),
        ("text/html; charset=utf-8", SITE.as_bytes().to_vec()),
    );
    fixtures.0.insert(
        "site.example/read".into(),
        ("text/html; charset=utf-8", READ.as_bytes().to_vec()),
    );
    fixtures
        .0
        .insert("site.example/logo.png".into(), ("image/png", PNG.to_vec()));
    fixtures.0.insert(
        "ads.tracker.example/banner.png".into(),
        ("image/png", PNG.to_vec()),
    );
    fixtures.0.insert(
        "ext-tracker.example/p.png".into(),
        ("image/png", PNG.to_vec()),
    );
    let extension = install_test_extension(&data);
    let headless = Headless::default();
    let redraw = headless.redraw.clone();
    let (page_area, overlay_area) = (headless.page.clone(), headless.overlay.clone());
    let browser = Browser::new(
        servo,
        page_ctx.clone(),
        1.0,
        Box::new(headless),
        Options {
            private: false,
            data: data.clone(),
            fixtures: Some(fixtures),
            rules: Some(RULES.into()),
            route: route.clone(),
        },
    );
    browser.attach_chrome(ui_ctx.clone());
    browser.attach_overlay(overlay_ctx.clone());
    browser.open_start(None);
    let run = Run {
        browser,
        rx,
        redraw,
        _contexts: (ui_ctx, page_ctx, overlay_ctx),
        page_area,
        overlay_area,
    };
    let mut r = Report {
        text: format!("EON Browser {} self-test\n\n", crate::browser::VERSION),
        failed: 0,
    };

    // --- start: the bar, a new tab, and no request ------------------------------------
    let started = run.pump_until(
        || {
            run.page()
                .is_some_and(|v| v.load_status() == servo::LoadStatus::Complete)
        },
        Duration::from_secs(30),
    );
    r.check("a new tab opens on first start", started, &run.url());
    // A release build must carry the day's lists (v0.0.0's Windows build did not).
    let seeded = crate::seed::SEED.len();
    r.check(
        "a release carries the blocking lists inside it",
        cfg!(debug_assertions) || seeded >= 7,
        &format!("{seeded} lists"),
    );
    // The bar's first state can arrive later than the page in a release build, where the
    // lists are compiled at first start: wait for it rather than for a fixed time.
    let mut tabs = String::new();
    run.pump_until(
        || {
            tabs = s(run.ui_eval("document.querySelectorAll('.tab').length"));
            tabs == "1"
        },
        Duration::from_secs(15),
    );
    r.check("the bar shows the tab", tabs == "1", &tabs);
    let title = s(run.page_eval("document.title"));
    r.check(
        "the new tab is the browser's own page",
        title == "New tab",
        &title,
    );
    r.check(
        "no request reaches the network before the person opens a page",
        run.browser.requests.borrow().is_empty(),
        &format!("{:?}", run.browser.requests.borrow()),
    );
    let hello = s(run.page_eval("String(!!document.querySelector('.hello'))"));
    r.check(
        "the first new tab welcomes the person and says where things are",
        hello == "true",
        &hello,
    );
    let _ = run.page_eval("document.querySelector('.hello .btn').click(); 'clicked'");
    run.pump_until(
        || crate::paths::Settings::load(&data).welcomed,
        Duration::from_secs(5),
    );
    run.pump_until(
        || s(run.page_eval("String(!!document.querySelector('.hello'))")) == "false",
        Duration::from_secs(5),
    );
    let gone = s(run.page_eval("String(!document.querySelector('.hello'))"));
    r.check(
        "\"Got it\" hides the welcome for good",
        crate::paths::Settings::load(&data).welcomed && gone == "true",
        &gone,
    );

    // --- the window's layout: the interface says where the page goes ----------------
    run.pump_until(|| run.page_area.get().is_some(), Duration::from_secs(5));
    let area = run.page_area.get().unwrap_or_default();
    r.check(
        "the interface places the page under the tabs and the bar, full width",
        area.x.abs() < 1.0 && (area.y - 88.0).abs() < 1.0 && (area.w - 1280.0).abs() < 1.0,
        &format!("{area:?}"),
    );

    // --- the route process (madde 50k), offline -------------------------------------
    route_checks(route.as_deref(), &mut r);

    // --- typed address: cleaned and upgraded ------------------------------------------
    run.browser
        .go("http://site.example/?utm_source=selftest&id=7");
    let ok = run.wait_url("https://site.example/?id=7");
    r.check(
        "a typed http address with a tracker opens clean, over HTTPS",
        ok,
        &run.url(),
    );
    run.settle(1500);

    let ad = s(run.page_eval(
        "(()=>{const i=document.getElementById('ad');return String(i.complete&&i.naturalWidth)})()",
    ));
    r.check("an ad image is blocked", ad == "0", &ad);
    let logo = s(run.page_eval("String(document.getElementById('ok').naturalWidth)"));
    r.check("the site's own image loads", logo == "1", &logo);
    let mut fav = String::new();
    run.pump_until(
        || {
            fav = s(run.ui_eval(
                "(()=>{const i=document.querySelector('.tab.active .fav');return i?String(i.naturalWidth):'none'})()",
            ));
            fav.parse::<u32>().is_ok_and(|w| w > 0)
        },
        Duration::from_secs(10),
    );
    r.check(
        "the tab shows the site's icon",
        fav.parse::<u32>().is_ok_and(|w| w > 0),
        &fav,
    );
    extension_checks(&run, extension.as_deref(), &mut r);
    let slot = s(run.page_eval("getComputedStyle(document.querySelector('.ad-slot')).display"));
    r.check(
        "a site-specific hiding rule hides its element",
        slot == "none",
        &slot,
    );
    let banner =
        s(run.page_eval("getComputedStyle(document.querySelector('.cookie-banner')).display"));
    r.check(
        "a generic hiding rule hides a cookie banner",
        banner == "none",
        &banner,
    );
    let ua = s(run.page_eval("navigator.userAgent"));
    r.check(
        "the user agent is Firefox ESR's, without Servo's token",
        ua == fingerprint::user_agent(Platform::current()),
        &ua,
    );
    let gpc = s(run.page_eval("String(navigator.globalPrivacyControl)"));
    r.check("Global Privacy Control is on", gpc == "true", &gpc);
    run.settle(300);
    let mut blocked = String::new();
    run.pump_until(
        || {
            blocked = s(run.ui_eval("document.getElementById('blocked').textContent"));
            blocked.parse::<u32>().is_ok_and(|n| n >= 1)
        },
        Duration::from_secs(5),
    );
    r.check(
        "the bar counts what was blocked",
        blocked.parse::<u32>().is_ok_and(|n| n >= 1),
        &blocked,
    );
    let address = s(run.ui_eval("document.getElementById('address').value"));
    r.check(
        "the bar shows the cleaned address",
        address == "site.example/?id=7",
        &address,
    );
    let tracker_reached = run
        .browser
        .requests
        .borrow()
        .iter()
        .any(|u| u.contains("tracker.example"));
    r.check(
        "the blocked request never reached the network layer",
        !tracker_reached,
        "",
    );

    // --- input ------------------------------------------------------------------------
    let _ = run.page_eval("document.getElementById('box').focus()");
    if let Some(v) = run.page() {
        key(&v, "e", Code::KeyE);
        key(&v, "ğ", Code::BracketLeft);
    }
    run.settle(300);
    let typed = s(run.page_eval("document.getElementById('box').value"));
    r.check(
        "keys reach a text field (with Turkish letters)",
        typed == "eğ",
        &typed,
    );
    if let Some(v) = run.page() {
        v.notify_input_event(InputEvent::Wheel(WheelEvent::new(
            WheelDelta {
                x: 0.0,
                y: -400.0,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            WebViewPoint::Device(DevicePoint::new(200.0, 300.0)),
        )));
    }
    let scrolled = run.pump_until(
        || s(run.page_eval("String(window.scrollY > 0)")) == "true",
        Duration::from_secs(5),
    );
    r.check("the wheel scrolls the page", scrolled, "");

    // --- screenshots, before leaving the page -----------------------------------------
    let _ = run.page_eval("window.scrollTo(0, 0)");
    run.settle(500);
    let dir = report_path.parent().unwrap_or_else(|| Path::new("."));
    for (name, view) in [
        ("selftest-bar.png", run.browser.chrome_view()),
        ("selftest-page.png", run.page()),
    ] {
        let image = view.and_then(|v| run.screenshot(&v));
        // The bar is dark; a blank (white) capture means it was never drawn.
        let drawn = image
            .as_ref()
            .is_some_and(|img| img.pixels().any(|p| p.0[0] < 128));
        let saved = image.is_some_and(|img| img.save(dir.join(name)).is_ok());
        r.check(&format!("screenshot {name}"), saved && drawn, "");
    }

    // --- a link in the page: cleaned and upgraded through request_navigation ----------
    let _ = run.page_eval("window.scrollTo(0,0); document.getElementById('out').click()");
    let ok = run.wait_url("https://site.example/read?keep=1");
    r.check(
        "a clicked http link with fbclid opens clean, over HTTPS",
        ok,
        &run.url(),
    );

    // --- a harmful site is a warning, not a silent block ------------------------------
    run.browser.go("https://site.example/");
    run.wait_url("https://site.example/");
    let _ = run.page_eval("document.getElementById('evil').click()");
    let warned = run.pump_until(
        || run.url().starts_with("https://page.eon.invalid/blocked"),
        Duration::from_secs(10),
    );
    r.check(
        "a site a list marks harmful shows a warning page",
        warned,
        &run.url(),
    );

    // --- pages cannot act for the person ----------------------------------------------
    run.browser.go("https://site.example/");
    run.wait_url("https://site.example/");
    let _ = run.page_eval("document.getElementById('sneaky').click()");
    run.settle(1000);
    let engine = crate::paths::Settings::load(&data).search_engine;
    r.check(
        "a site cannot choose the search engine through an own-page link",
        engine.is_none(),
        &format!("{engine:?}"),
    );
    let before = run.browser.tab_count();
    let _ = run.page_eval(
        "fetch('https://ui.eon.invalid/cmd?c=%7B%22op%22%3A%22newTab%22%7D').catch(()=>0)",
    );
    run.settle(1000);
    r.check(
        "a site cannot reach the bar's bridge",
        run.browser.tab_count() == before,
        "",
    );

    // --- the own pages: choosing a search engine --------------------------------------
    run.browser.go("rust servo");
    let welcome = run.pump_until(
        || run.url().starts_with("https://page.eon.invalid/welcome"),
        Duration::from_secs(10),
    );
    r.check(
        "a search before an engine is chosen asks which engine",
        welcome,
        &run.url(),
    );
    run.settle(500);
    let _ = run.page_eval("document.querySelector('a[href*=\"engine=duckduckgo\"]').click()");
    let searched = run.pump_until(
        || {
            run.url()
                .starts_with("https://duckduckgo.com/?q=rust+servo")
        },
        Duration::from_secs(10),
    );
    r.check(
        "choosing DuckDuckGo there runs the search with it",
        searched,
        &run.url(),
    );
    let engine = crate::paths::Settings::load(&data).search_engine;
    r.check(
        "the choice is remembered",
        engine.as_deref() == Some("duckduckgo"),
        &format!("{engine:?}"),
    );

    // --- tabs and history -------------------------------------------------------------
    run.browser.new_tab(
        Url::parse("https://site.example/read").map_err(|e| e.to_string())?,
        true,
    );
    run.wait_url("https://site.example/read");
    let mut tabs = String::new();
    run.pump_until(
        || {
            tabs = s(run.ui_eval("document.querySelectorAll('.tab').length"));
            tabs == "2"
        },
        Duration::from_secs(10),
    );
    r.check("a second tab appears in the bar", tabs == "2", &tabs);

    // --- bookmarks and history ----------------------------------------------------
    run.browser.toggle_bookmark();
    let mut star = String::new();
    run.pump_until(
        || {
            star = s(run.ui_eval("document.querySelector('#star img').getAttribute('src')"));
            star.contains("star-on")
        },
        Duration::from_secs(5),
    );
    r.check(
        "Ctrl+D bookmarks the page and the star says so",
        star.contains("star-on"),
        &star,
    );
    run.browser.go("eon:bookmarks");
    run.pump_until(
        || run.url().ends_with("/bookmarks"),
        Duration::from_secs(10),
    );
    run.settle(300);
    let listed = s(run.page_eval("String(document.body.innerText.includes('site.example/read'))"));
    r.check("the bookmarks page lists it", listed == "true", &listed);
    run.browser.go("eon:history");
    run.pump_until(|| run.url().ends_with("/history"), Duration::from_secs(10));
    run.settle(300);
    let visited =
        s(run.page_eval("String(document.body.innerText.includes('https://site.example/?id=7'))"));
    r.check(
        "the history page lists the pages visited",
        visited == "true",
        &visited,
    );

    window_checks(
        &run,
        &data,
        report_path.parent().unwrap_or_else(|| Path::new(".")),
        &mut r,
    );

    // Shut the engine down in order before its folder goes away (see Browser::shutdown).
    run.browser.shutdown();
    let Run {
        browser,
        _contexts: contexts,
        ..
    } = run;
    drop(browser);
    // The offscreen GL contexts are left to the process's end: surfman (on WGL)
    // asserts when a context that is not current is torn down after another one.
    std::mem::forget(contexts);
    let _ = std::fs::remove_dir_all(&data);
    let _ = writeln!(r.text, "\n{} failed", r.failed);
    std::fs::write(report_path, &r.text).map_err(|e| e.to_string())?;
    print!("{}", r.text);
    Ok(r.failed)
}

/// The route process: present, quiet until asked, closed to other programs, and a working
/// tunnel for the browser. Everything here stays on 127.0.0.1.
#[allow(clippy::too_many_lines)] // three small servers and what each proves, in order
fn route_checks(route: Option<&crate::route_client::RouteClient>, r: &mut Report) {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    r.check(
        "the route process starts beside the browser",
        route.is_some(),
        "",
    );
    let Some(route) = route else { return };
    std::thread::sleep(Duration::from_millis(400));
    let tor = route.status().tor;
    r.check(
        "Tor is not contacted until the person chooses it",
        tor == "off",
        &tor,
    );
    let Some(addr) = route.local_addr() else {
        return;
    };

    let ask = |head: &str| -> Option<(String, TcpStream)> {
        let mut s = TcpStream::connect(&addr).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        s.write_all(head.as_bytes()).ok()?;
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).ok()?;
        Some((String::from_utf8_lossy(&buf[..n]).into_owned(), s))
    };
    let refused = ask("CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
        .map(|(h, _)| h)
        .unwrap_or_default();
    r.check(
        "another program cannot use the browser's proxy without its secret",
        refused.starts_with("HTTP/1.1 407"),
        refused.lines().next().unwrap_or(""),
    );

    // A local echo server, reached through the proxy on the direct route.
    let Ok(echo) = TcpListener::bind("127.0.0.1:0") else {
        return;
    };
    let Ok(echo_addr) = echo.local_addr() else {
        return;
    };
    std::thread::spawn(move || {
        if let Ok((mut c, _)) = echo.accept() {
            let mut b = [0u8; 16];
            if let Ok(n) = c.read(&mut b) {
                let _ = c.write_all(&b[..n]);
            }
        }
    });
    let head = format!(
        "CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\nProxy-Authorization: {}\r\n\r\n",
        route.proxy_authorization()
    );
    let tunnelled = ask(&head).and_then(|(h, mut s)| {
        if !h.starts_with("HTTP/1.1 200") {
            return Some(h);
        }
        s.write_all(b"eon").ok()?;
        let mut b = [0u8; 3];
        s.read_exact(&mut b).ok()?;
        Some(String::from_utf8_lossy(&b).into_owned())
    });
    r.check(
        "the browser's connections go through its proxy (direct route)",
        tunnelled.as_deref() == Some("eon"),
        &format!("{tunnelled:?}"),
    );

    // Tor's own proxy, or anyone's SOCKS5 server: a fake one on 127.0.0.1 that records the
    // name it was asked for (the name must reach the proxy, not be looked up here).
    let Ok(socks) = TcpListener::bind("127.0.0.1:0") else {
        return;
    };
    let Ok(socks_addr) = socks.local_addr() else {
        return;
    };
    let (asked_tx, asked_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let Ok((mut c, _)) = socks.accept() else {
            return;
        };
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let mut b = [0u8; 300];
        // Greeting: VER, NMETHODS, METHODS.
        if c.read(&mut b).is_err() || c.write_all(&[5, 0]).is_err() {
            return;
        }
        // Request: VER CMD RSV ATYP=3 LEN NAME PORT.
        let Ok(n) = c.read(&mut b) else { return };
        if n > 5 && b[3] == 3 {
            let len = b[4] as usize;
            let name = String::from_utf8_lossy(&b[5..(5 + len).min(n)]).into_owned();
            let _ = asked_tx.send(name);
        }
        let _ = c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]);
        if let Ok(n) = c.read(&mut b) {
            let _ = c.write_all(&b[..n]);
        }
    });
    route.set_route("socks", &format!("socks5://{socks_addr}"));
    let start = Instant::now();
    while route.status().kind != "socks" && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let head = format!(
        "CONNECT example.invalid:443 HTTP/1.1\r\nHost: example.invalid:443\r\nProxy-Authorization: {}\r\n\r\n",
        route.proxy_authorization()
    );
    let through = ask(&head).and_then(|(h, mut s)| {
        if !h.starts_with("HTTP/1.1 200") {
            return Some(h);
        }
        s.write_all(b"eon").ok()?;
        let mut b = [0u8; 3];
        s.read_exact(&mut b).ok()?;
        Some(String::from_utf8_lossy(&b).into_owned())
    });
    let asked = asked_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    r.check(
        "a SOCKS5 proxy (Tor's own, or yours) carries the connection, and is given the name",
        through.as_deref() == Some("eon") && asked == "example.invalid",
        &format!("{through:?}, asked for {asked:?}"),
    );
    route.set_route("direct", "");
    let start = Instant::now();
    while route.status().kind != "direct" && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Sleeping tabs, the limiters, the monitor and the command bar (v0.0.3).
#[allow(clippy::too_many_lines)] // one scenario per feature, read top to bottom
fn window_checks(run: &Run, data: &Path, shots: &Path, r: &mut Report) {
    // Tab 0 is in the background: put it to sleep, see the moon, wake it where it was.
    let before = run.browser.tab_info(0).and_then(|(_, u)| u);
    let slept = run.browser.sleep_tab(0);
    let mut asleep = String::new();
    run.pump_until(
        || {
            asleep = s(run.ui_eval("document.querySelectorAll('.tab.asleep').length"));
            asleep == "1"
        },
        Duration::from_secs(5),
    );
    r.check(
        "a background tab goes to sleep, and the sidebar shows it",
        slept && run.browser.tab_info(0).is_some_and(|(a, _)| a) && asleep == "1",
        &asleep,
    );
    run.browser.select(0);
    let want = before.as_ref().map(ToString::to_string).unwrap_or_default();
    let woke = run.wait_url(&want);
    r.check(
        "clicking a sleeping tab wakes it where it was",
        woke && run.browser.tab_info(0).is_some_and(|(a, _)| !a),
        &format!("{} (wanted {want})", run.url()),
    );

    // The monitor reads this computer every two seconds.
    let mut ram = String::new();
    run.pump_until(
        || {
            ram = s(run.ui_eval("String(state && state.monitor ? state.monitor.ramMb : 0)"));
            ram.parse::<u32>().is_ok_and(|n| n > 0)
        },
        Duration::from_secs(6),
    );
    r.check(
        "the monitor shows the browser's memory",
        ram.parse::<u32>().is_ok_and(|n| n > 0),
        &format!("{ram} MB"),
    );

    // The limiters, set by clicking their buttons in the panel, are kept.
    let _ = run.ui_eval("openPanel('limits'); 'open'");
    for (key, v) in [
        ("netDownKbps", 256),
        ("ramLimitMb", 2048),
        ("sleepAfterMinutes", 30),
    ] {
        let _ = run.ui_eval(&format!(
            "document.querySelector('.seg[data-key=\"{key}\"] [data-v=\"{v}\"]').click(); 'clicked'"
        ));
    }
    run.pump_until(
        || {
            let st = crate::paths::Settings::load(data);
            st.net_down_kbps == 256 && st.sleep_after_minutes == Some(30)
        },
        Duration::from_secs(5),
    );
    run.settle(2500);
    if let Some(img) = run.browser.chrome_view().and_then(|v| run.screenshot(&v)) {
        let _ = img.save(shots.join("selftest-limits.png"));
    }
    let st = crate::paths::Settings::load(data);
    r.check(
        "the network, memory and sleep limits are kept",
        st.net_down_kbps == 256 && st.ram_limit_mb == 2048 && st.sleep_after_minutes == Some(30),
        &format!(
            "{} KB/s, {} MB, {:?} min",
            st.net_down_kbps, st.ram_limit_mb, st.sleep_after_minutes
        ),
    );
    let _ = run.ui_eval(
        "cmd('setLimits', { netDownKbps: 0, ramLimitMb: 0, sleepAfterMinutes: 15 }) && 'sent'",
    );
    run.settle(300);

    // The menu (Alt+F): opens under its button, lists everything, and an item runs.
    run.browser.open_menu();
    let mut items = String::new();
    run.pump_until(
        || {
            items =
                s(run
                    .overlay_eval("String(document.querySelectorAll('li[role=menuitem]').length)"));
            items.parse::<u32>().is_ok_and(|n| n >= 15) && run.overlay_area.get().is_some()
        },
        Duration::from_secs(5),
    );
    let at = run.overlay_area.get().unwrap_or_default();
    // Under the ⋮ button, right edges aligned (a panel may be open, narrowing the bar).
    let right =
        s(run.ui_eval("String(document.getElementById('menu').getBoundingClientRect().right)"))
            .parse::<f64>()
            .unwrap_or(0.0);
    r.check(
        "the menu opens under its button at the right end of the bar, with every action",
        items.parse::<u32>().is_ok_and(|n| n >= 15)
            && (at.x + at.w - right).abs() < 2.0
            && right > 900.0
            && at.y < 100.0,
        &format!("{items} items at {at:?}, button's right edge {right}"),
    );
    run.settle(300);
    if let Some(img) = run.browser.overlay_view().and_then(|v| run.screenshot(&v)) {
        let _ = img.save(shots.join("selftest-menu.png"));
    }
    let _ = run.overlay_eval("run(shown.find((s) => s.title === 'History')); 'ran'");
    let history = run.pump_until(
        || run.url().ends_with("/history") && run.overlay_area.get().is_none(),
        Duration::from_secs(5),
    );
    r.check(
        "a menu item runs (History) and the menu closes",
        history,
        &run.url(),
    );

    // The command bar: Ctrl+K opens it over the page; typing finds a command; running it
    // opens the panel.
    run.browser.open_palette();
    let mut items = String::new();
    run.pump_until(
        || {
            items = s(run.overlay_eval("String(document.querySelectorAll('li').length)"));
            items.parse::<u32>().is_ok_and(|n| n >= 10) && run.overlay_area.get().is_some()
        },
        Duration::from_secs(5),
    );
    r.check(
        "Ctrl+K opens the command bar above the page",
        items.parse::<u32>().is_ok_and(|n| n >= 10) && run.overlay_area.get().is_some(),
        &format!("{items} commands, {:?}", run.overlay_area.get()),
    );
    run.settle(300);
    if let Some(img) = run.browser.overlay_view().and_then(|v| run.screenshot(&v)) {
        let _ = img.save(shots.join("selftest-palette.png"));
    }
    let _ = run.overlay_eval(
        "document.getElementById('q').value = 'monitor'; filter(); run(shown[0]); 'ran'",
    );
    let mut title = String::new();
    run.pump_until(
        || {
            title = s(run.ui_eval(
                "document.getElementById('panel').hidden ? '' : document.getElementById('panel-title').textContent",
            ));
            title == "Monitor" && run.overlay_area.get().is_none()
        },
        Duration::from_secs(5),
    );
    r.check(
        "a command found by typing runs, and the command bar closes",
        title == "Monitor" && run.overlay_area.get().is_none(),
        &title,
    );
    run.settle(2500);
    if let Some(img) = run.browser.chrome_view().and_then(|v| run.screenshot(&v)) {
        let _ = img.save(shots.join("selftest-panel.png"));
    }
    let area = run.page_area.get().unwrap_or_default();
    r.check(
        "a docked panel narrows the page instead of covering it",
        area.w > 0.0 && area.x + area.w <= 1280.0 - 319.0,
        &format!("{area:?}"),
    );
}

/// A small extension written for the test, installed from a folder: a content script that
/// uses `chrome.i18n`, a style, a `declarativeNetRequest` rule, one web-accessible image
/// and one private file. Returns any installation error.
fn install_test_extension(data: &Path) -> Option<String> {
    let src = data.join("test-extension");
    let w = |rel: &str, body: &[u8]| -> std::io::Result<()> {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, body)
    };
    let written = (|| -> std::io::Result<()> {
        w("manifest.json", br#"{
  "manifest_version": 3, "name": "__MSG_name__", "version": "1.0", "default_locale": "en",
  "permissions": ["declarativeNetRequest"],
  "content_scripts": [{ "matches": ["https://site.example/*"], "js": ["content.js"], "css": ["content.css"] }],
  "web_accessible_resources": [{ "resources": ["img/*"], "matches": ["<all_urls>"] }],
  "declarative_net_request": { "rule_resources": [{ "id": "r", "enabled": true, "path": "rules.json" }] }
}"#)?;
        w("_locales/en/messages.json", br#"{"name":{"message":"EON self-test extension"},"hello":{"message":"hello from an extension"}}"#)?;
        w(
            "content.js",
            b"document.documentElement.dataset.eonExt = chrome.i18n.getMessage('hello');",
        )?;
        w("content.css", b".ext-hide { display: none !important; }")?;
        w("rules.json", br#"[{"id":1,"action":{"type":"block"},"condition":{"urlFilter":"||ext-tracker.example^","resourceTypes":["image"]}}]"#)?;
        w("img/open.png", PNG)?;
        w("private.png", PNG)
    })();
    if let Err(e) = written {
        return Some(e.to_string());
    }
    let mut lists = match eon_browser_core::lists::Lists::load(&data.join("lists")) {
        Ok(l) => l,
        Err(e) => return Some(e.to_string()),
    };
    crate::extensions::Extensions::load(data)
        .install(&src, &mut lists)
        .err()
}

fn extension_checks(run: &Run, install_error: Option<&str>, r: &mut Report) {
    r.check(
        "an extension installs from a folder (Chrome's \"load unpacked\")",
        install_error.is_none(),
        install_error.unwrap_or(""),
    );
    let blocked = s(run.page_eval(
        "(()=>{const i=document.getElementById('exttrk');return String(i.complete&&i.naturalWidth)})()",
    ));
    r.check(
        "an extension's declarativeNetRequest rule blocks in the shield",
        blocked == "0",
        &blocked,
    );
    let said = s(run.page_eval("String(document.documentElement.dataset.eonExt)"));
    r.check(
        "an extension's content script runs, with chrome.i18n",
        said == "hello from an extension",
        &said,
    );
    let hidden = s(run.page_eval("getComputedStyle(document.querySelector('.ext-hide')).display"));
    r.check(
        "an extension's stylesheet applies",
        hidden == "none",
        &hidden,
    );
    let war = s(run.page_eval("String(document.getElementById('extwar').naturalWidth)"));
    r.check(
        "an extension's web-accessible file loads in a page",
        war == "1",
        &war,
    );
    let private = s(run.page_eval("String(document.getElementById('extpriv').naturalWidth)"));
    r.check(
        "a site cannot load an extension's other files (no probing for extensions)",
        private == "0",
        &private,
    );
}
