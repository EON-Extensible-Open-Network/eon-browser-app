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

**What it does not do yet — read before using it**
- **Not sandboxed on Windows.** Servo does not yet isolate pages from each other or from your
  computer the way Chrome and Firefox do. Do not use it for banking or anything sensitive yet.
- **No video or audio** yet (v0.0.1). **No DRM**, ever: Netflix and similar will not play.
- Some sites render wrongly or not at all; Servo is younger than the big engines.
- No bookmarks bar, downloads, permission prompts, extensions or sync yet.

**Run it:** download the file for your system and check it against `SHA256SUMS.txt`.
`eon-browser --selftest report.txt` checks what the browser promises, offscreen.
On Windows, Smart App Control may block a downloaded unsigned program.
