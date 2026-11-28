**EON Browser** is a web browser on Servo, the open engine written in Rust — not Chromium,
not a system webview. This is an **early alpha** (plan madde 50).

**What it does now**
- Ads and trackers blocked out of the box (EasyList, EasyPrivacy, uBlock Origin's lists and
  the EasyList cookie list, shipped inside the program): no request is made until you open a page.
- Tracking parameters removed from links (`utm_*`, `fbclid`, `gclid`, …) and redirect
  wrappers skipped, in every window. Every site opened over HTTPS; if a site has none, you
  choose to continue on HTTP for that site.
- No search engine chosen for you; no suggestions sent while you type; no telemetry, no AI.
- Tabs, history with search in the address bar, a private window (`Ctrl+Shift+N`).
- **Tor in the browser** (from v0.0.2): choose the route in the Route panel — direct, Tor (a
  circuit per site, renewed every ten minutes, New identity with `Ctrl+Shift+U`), Tor
  Browser's or the tor service's own proxy (from v0.0.3), or your own SOCKS5 or HTTP server.
  Tor hides your address from sites; it does not make you as hard to recognise as Tor
  Browser does.
- **Light and in your control** (from v0.0.3): unused tabs go to sleep; a memory limit,
  download and upload limits, and a live monitor of what the browser uses, read on your
  computer only.
- **A window you already know** (from v0.0.5): tabs on top with site icons, the menu `⋮` at
  the right (`Alt+F`), a short welcome the first time; vertical tabs if you prefer them.
- **YouTube opens** (from v0.0.5); its videos do not play yet (no video support yet).

- **Extensions from other browsers** (from v0.0.2, first stage): install a Chrome `.crx`,
  a Firefox `.xpi` or a folder with `eon-browser --install-extension <file>`; it shows what
  the extension may do first. Blocking rules and page scripts and styles run; settings
  pages, pop-ups and background work come next.

**What it does not do yet — read before using it**
- **Not sandboxed on Windows.** Servo does not yet isolate pages from each other or from your
  computer the way Chrome and Firefox do. Do not use it for banking or anything sensitive yet.
- **No video or audio** yet (planned next, with WebGL). **No DRM**, ever: Netflix and similar will not play.
- Some sites render wrongly or not at all; Servo is younger than the big engines.
- No bookmarks bar, downloads, permission prompts or sync yet.

**Run it:** download the archive for your system, check it against `SHA256SUMS.txt`, and
keep `eon-browser` and `eon-browser-route` in the same folder.
`eon-browser --selftest report.txt` checks what the browser promises, offscreen.
On Windows, Smart App Control may block a downloaded unsigned program.
