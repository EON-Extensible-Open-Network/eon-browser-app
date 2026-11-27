<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Changelog

## v0.0.2 — 27 November 2026

What v0.0.1 was meant to ship, with the two fixes its release found. **v0.0.1 was tagged
but never published**: its release stopped at the self-test on Linux and macOS, as it
should. Two timing assumptions, both exposed by a release build compiling its lists at
first start:

- at `HeadParsed`, `WebView::url` could still name the previous page, so a site's hiding
  rules were looked up for the wrong address; the document's own address (recorded from
  its main-frame request) is used, as blocking already did;
- the self-test waited a fixed 500 ms for the bar's first state; it now waits for it.

## v0.0.1 — 27 November 2026 (tagged, not published)

**The route** (plan madde 50k): every connection goes through a small proxy of the
browser's own, in a process of its own (`eon-browser-route`, shipped beside the browser),
which sends it directly, **through Tor**, or through **a proxy server the person brings**.

- Tor runs through **Arti**, the Tor Project's Rust implementation: a circuit per site,
  renewed every ten minutes, and **New identity** (`Ctrl+Shift+U`, or the route page)
  drops every circuit and clears every cookie. `.onion` addresses open on the Tor route.
- The route page (`eon:route`, or the indicator in the bar) chooses; the choice is kept.
- Tor is not contacted until the person chooses it; the proxy answers only to a secret
  the browser gave it, so no other program can use it; the process exits with the
  browser.
- Measured on 27 November: Tor connects in 2–8 seconds on the owner's connection;
  check.torproject.org in EON Browser: "Congratulations. This browser is configured to
  use Tor."; the same site keeps its exit, another site gets another, New identity
  changes it.
- The self-test grows to 37 checks: the route (4), extensions (6), and that a release
  carries its lists.

**Other browsers' extensions, stage 1** (plan madde 50h): `--install-extension` takes a
Chrome CRX3, a Firefox XPI, a zip or an unpacked folder, says in sentences what the
extension may do, and installs it; `eon:extensions` turns it on, off or removes it. Its
`declarativeNetRequest` rules run in the shield; its content scripts and styles run in
matching pages with `chrome.runtime` and `chrome.i18n`; only its web-accessible files are
served, so sites cannot probe for it. Tried with the real **uBlock Origin Lite** from the
Chrome Web Store: 117,636 rules run, 1,147 not translated yet (redirects, header changes,
conditions not handled), and BBC News shows 21 trackers blocked by its rules alone.
Storage, messaging, background pages and pop-ups are stage 2.

**Fixed: the v0.0.0 Windows file carried no blocking lists** — the release script gave the
build a POSIX path it could not read. The build now stops when lists were asked for and
none were found, and the self-test checks a release carries them.

Found while building: **Arti and Servo cannot share one build** — Arti turns on
`derive_more`'s "full" feature, whose `PartialEq`/`Eq` derives shadow std's inside
webrender (`#[macro_use] extern crate derive_more`), and webrender stops compiling. Hence
the separate process, which is also the safer shape. And **YouTube's failure is inside
Servo's HTTP client**, not the network: curl through the same proxy gets 200.

## v0.0.0 — 26 November 2026

The first version (plan madde 50): Servo 0.8.0 in a window, the bar drawn by Servo itself,
tabs, history, the private window, and `eon-browser-core`'s policy on every request and
navigation. Lists shipped in the release. `--selftest` with 26 checks.

Found while building:

- **Servo links rusqlite 0.38**; the core moved to it, since two SQLite builds cannot link
  into one program.
- **Servo cannot start a script evaluation from inside another's callback** (a `RefCell`
  is borrowed twice). Follow-up scripts are queued and run on the next turn.
- **Inline `<svg><use>` is not drawn by Servo 0.8**; the bar's icons are SVG images.
- **Servo's default features leave out Web Crypto and Brotli streams**; they are on.
  Devtools are off. WebGL needs libclang to build ANGLE's shader translator, so it waits for
  v0.0.1 with video.
- **There is no `Accept-Language` preference and no way to add a request header** in
  Servo's embedding API, so the `Sec-GPC` header is not sent yet; the script-side
  `navigator.globalPrivacyControl` is.
