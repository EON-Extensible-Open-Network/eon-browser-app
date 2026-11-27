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
}

impl Host for Headless {
    fn redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, _cursor: Cursor) {}
    fn set_title(&self, _title: &str) {}
    fn chrome_height(&self, _css_px: u32) {}
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
<style>body{font:16px sans-serif;margin:24px;background:#fff;color:#222} .tall{height:3000px}</style></head>
<body>
<h1>A page with trackers</h1>
<img id="ad" src="https://ads.tracker.example/banner.png" width="40" height="40">
<img id="ok" src="https://site.example/logo.png" width="40" height="40">
<div class="ad-slot">AD SLOT</div>
<div class="cookie-banner">We value your privacy</div>
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
    _contexts: (Rc<SoftwareRenderingContext>, Rc<SoftwareRenderingContext>),
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
        SoftwareRenderingContext::new(PhysicalSize::new(1280, 84)).map_err(|e| format!("{e:?}"))?,
    );
    let page_ctx = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(1280, 720))
            .map_err(|e| format!("{e:?}"))?,
    );
    let _ = page_ctx.make_current();
    // Cookies and storage in memory; history and settings in a throw-away folder.
    let servo = crate::engine::builder(true, &data, Box::new(Waker(tx))).build();
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
    let headless = Headless::default();
    let redraw = headless.redraw.clone();
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
        },
    );
    browser.attach_chrome(ui_ctx.clone());
    browser.open_start(None);
    let run = Run {
        browser,
        rx,
        redraw,
        _contexts: (ui_ctx, page_ctx),
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
    run.settle(500);
    let tabs = s(run.ui_eval("document.querySelectorAll('.tab').length"));
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
    let blocked = s(run.ui_eval("document.getElementById('blocked').textContent"));
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
    run.settle(300);
    let tabs = s(run.ui_eval("document.querySelectorAll('.tab').length"));
    r.check("a second tab appears in the bar", tabs == "2", &tabs);

    // Shut the engine down in order before its folder goes away (see Browser::shutdown).
    run.browser.shutdown();
    let Run {
        browser,
        _contexts: contexts,
        ..
    } = run;
    drop(browser);
    // The two offscreen GL contexts are left to the process's end: surfman (on WGL)
    // asserts when a context that is not current is torn down after another one.
    std::mem::forget(contexts);
    let _ = std::fs::remove_dir_all(&data);
    let _ = writeln!(r.text, "\n{} failed", r.failed);
    std::fs::write(report_path, &r.text).map_err(|e| e.to_string())?;
    print!("{}", r.text);
    Ok(r.failed)
}
