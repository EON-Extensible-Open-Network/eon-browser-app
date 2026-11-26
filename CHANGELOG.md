<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Changelog

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
