// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! EON Browser's own pages, served from `https://page.eon.invalid/` — a host that never
//! resolves, answered by the browser itself. They are plain pages: they can only link to
//! an *action* address (`/choose`, `/proceed`, `/allow-http`), and the browser acts on it
//! only when the page asking is itself one of these (see `Browser::request_navigation`),
//! so no site can change a setting by linking there.

use std::fmt::Write as _;

use eon_browser_core::search::SearchEngine;
use url::Url;

pub const HOST: &str = "page.eon.invalid";

/// The address of an own page.
pub fn url(path_and_query: &str) -> Url {
    Url::parse(&format!("https://{HOST}{path_and_query}"))
        .unwrap_or_else(|_| Url::parse("about:blank").unwrap_or_else(|_| unreachable!()))
}

/// Whether `path` is an action rather than a page.
pub fn is_action(path: &str) -> bool {
    matches!(path, "/choose" | "/proceed" | "/allow-http")
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

const STYLE: &str = r#"
@font-face { font-family: "Geist"; src: url("/fonts/Geist-Variable.woff2") format("woff2"); font-weight: 100 900; }
:root { --ink:#15132b; --deck:#1f1c3d; --raise:#272350; --line:#2f2b58; --mist:#ece9fb; --haze:#a7a2cc;
        --dim:#7e78a8; --pulse:#a08bff; --current:#4fd8e8; --mint:#74e3ae; --amber:#ffc861; --coral:#ff7a7a; }
* { box-sizing: border-box; }
html { background: var(--ink); color: var(--mist); font: 15px/1.55 "Geist", system-ui, sans-serif; }
body { margin: 0; min-height: 100vh; display: flex; justify-content: center; }
main { width: min(680px, 100% - 48px); padding: 12vh 0 64px; }
h1 { font-size: 30px; line-height: 1.2; font-weight: 650; margin: 0 0 12px; letter-spacing: -0.01em; }
p { color: var(--haze); margin: 0 0 16px; }
.mark { width: 44px; height: 44px; border-radius: 12px; margin-bottom: 28px;
        background: linear-gradient(135deg, var(--raise), var(--ink)); display: flex; align-items: center;
        justify-content: center; border: 1px solid var(--line); }
.mark i { display: block; width: 18px; height: 6px; border-radius: 3px; background: var(--current); }
.grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin: 28px 0; }
a.card { display: block; padding: 14px 16px; border-radius: 12px; background: var(--deck);
         border: 1px solid var(--line); color: var(--mist); text-decoration: none; }
a.card:hover { border-color: var(--pulse); }
a.card b { display: block; font-weight: 600; margin-bottom: 2px; }
a.card span { color: var(--dim); font-size: 13px; }
.actions { display: flex; gap: 10px; margin-top: 28px; flex-wrap: wrap; }
a.btn { padding: 9px 16px; border-radius: 10px; text-decoration: none; font-weight: 600;
        background: var(--pulse); color: var(--ink); }
a.btn.quiet { background: var(--deck); color: var(--mist); border: 1px solid var(--line); }
code { font: 13px ui-monospace, monospace; color: var(--current); word-break: break-all; }
ul.facts { list-style: none; padding: 0; margin: 24px 0; }
ul.facts li { padding: 10px 0; border-top: 1px solid var(--line); color: var(--haze); }
ul.facts li b { color: var(--mist); font-weight: 600; }
.warn h1 { color: var(--amber); }
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title>\
         <style>{STYLE}</style></head><body><main>{body}</main></body></html>",
        escape(title)
    )
}

const MARK: &str = "<div class=\"mark\"><i></i></div>";

/// Render the own page at `url`, or `None` if there is no such page.
pub fn render(
    url: &Url,
    engines: &[SearchEngine],
    chosen: Option<&str>,
    version: &str,
) -> Option<String> {
    let q = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    };
    match url.path() {
        "/newtab" => Some(newtab(chosen, engines)),
        "/welcome" => Some(welcome(engines, &q("q"))),
        "/blocked" => Some(blocked(&q("url"))),
        "/https-failed" => Some(https_failed(&q("url"))),
        "/about" => Some(about(version)),
        _ => None,
    }
}

fn newtab(chosen: Option<&str>, engines: &[SearchEngine]) -> String {
    let engine = chosen
        .and_then(|id| engines.iter().find(|e| e.id == id))
        .map_or_else(
            || "<a href=\"/welcome\">Choose a search engine</a>".to_string(),
            |e| {
                format!(
                    "Searches go to <b>{}</b>. <a href=\"/welcome\">Change</a>",
                    escape(&e.name)
                )
            },
        );
    page(
        "New tab",
        &format!(
            "{MARK}<h1>New tab</h1><p>Type in the bar above to search or to go to an address.</p>\
             <p>{engine}</p>\
             <ul class=\"facts\">\
             <li><b>Nothing is sent anywhere</b> until you open a page: no telemetry, no suggestions while you type.</li>\
             <li><b>Ads and trackers are blocked</b> by EasyList, EasyPrivacy and uBlock Origin's lists.</li>\
             <li><b>Tracking parameters are removed</b> from links, and every site is opened over HTTPS.</li>\
             </ul><p><a href=\"/about\">About EON Browser</a></p>"
        ),
    )
}

fn welcome(engines: &[SearchEngine], query: &str) -> String {
    let mut cards = String::new();
    for e in engines {
        let _ = write!(
            cards,
            "<a class=\"card\" href=\"/choose?engine={}&amp;q={}\"><b>{}</b><span>{}</span></a>",
            enc(&e.id),
            enc(query),
            escape(&e.name),
            escape(&e.about)
        );
    }
    page(
        "Choose a search engine",
        &format!(
            "{MARK}<h1>Choose a search engine</h1>\
             <p>EON Browser has no search deal with anyone, so it does not choose for you. \
             What you type is sent only to the engine you pick, and only when you press Enter.</p>\
             <div class=\"grid\">{cards}</div>\
             <p>You can change it from a new tab at any time. Typing <code>@w</code>, <code>@ddg</code> \
             or another engine's keyword before a search uses that engine once.</p>"
        ),
    )
}

fn blocked(target: &str) -> String {
    page(
        "Site blocked",
        &format!(
            "<div class=\"warn\">{MARK}<h1>A filter list marks this site as harmful</h1>\
             <p>uBlock Origin's badware list, or another list you have on, names <code>{}</code> \
             as a site that tricks or harms people.</p>\
             <div class=\"actions\"><a class=\"btn\" href=\"javascript:history.back()\">Go back</a>\
             <a class=\"btn quiet\" href=\"/proceed?url={}\">Open it anyway</a></div></div>",
            escape(target),
            enc(target)
        ),
    )
}

fn https_failed(target: &str) -> String {
    page(
        "No secure connection",
        &format!(
            "<div class=\"warn\">{MARK}<h1>This site does not offer a secure connection</h1>\
             <p>EON Browser opens every site over HTTPS. <code>{}</code> did not answer that way, \
             so what you send and receive there could be read or changed on the way.</p>\
             <div class=\"actions\"><a class=\"btn\" href=\"javascript:history.back()\">Go back</a>\
             <a class=\"btn quiet\" href=\"/allow-http?url={}\">Continue on HTTP for this site</a></div>\
             <p style=\"margin-top:20px\">Your choice is remembered for this site only.</p></div>",
            escape(target),
            enc(target)
        ),
    )
}

fn about(version: &str) -> String {
    page(
        "About EON Browser",
        &format!(
            "{MARK}<h1>EON Browser {}</h1>\
             <p>A web browser on Servo, the open engine written in Rust. Free, open source, \
             <code>GPL-3.0-or-later</code>. Part of EON, an open ecosystem for everyone.</p>\
             <ul class=\"facts\">\
             <li><b>No telemetry.</b> Nothing about you or your browsing is collected, by EON or by anyone through EON.</li>\
             <li><b>No AI, no sponsored tiles, no affiliate links,</b> no search engine chosen for you.</li>\
             <li><b>An early alpha.</b> Some sites will not work yet. Video with DRM (Netflix and similar) never will.</li>\
             <li><b>Not yet sandboxed on Windows.</b> Servo does not isolate pages from each other or from your \
             computer the way Chrome and Firefox do. Prefer another browser for banking until it does.</li>\
             </ul>",
            escape(version)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_escape_what_they_show() {
        let u = url("/blocked?url=%3Cscript%3Ealert(1)%3C%2Fscript%3E");
        let html = render(&u, &[], None, "0").unwrap_or_default();
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn unknown_pages_and_actions() {
        assert!(render(&url("/nope"), &[], None, "0").is_none());
        assert!(is_action("/choose") && is_action("/allow-http") && !is_action("/welcome"));
    }

    #[test]
    fn welcome_lists_every_engine() {
        let engines = eon_browser_core::search::catalogue();
        let html = render(&url("/welcome?q=rust"), &engines, None, "0").unwrap_or_default();
        for e in &engines {
            assert!(html.contains(&format!("engine={}", e.id)), "{}", e.id);
        }
        assert!(html.contains("q=rust"));
    }
}
