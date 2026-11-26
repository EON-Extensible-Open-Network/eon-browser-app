<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Contributing to EON Browser

The organisation-wide guide,
[`.github/CONTRIBUTING.md`](https://github.com/EON-Extensible-Open-Network/.github/blob/main/CONTRIBUTING.md),
applies: DCO sign-off (`git commit -s`), Conventional Commits (`feat(browser): ...`,
`fix(ui): ...`). By contributing you license your work under `GPL-3.0-or-later` with the
EON Module ABI Exception.

- **Policy belongs in `eon-browser-core`**, not here: what is blocked, cleaned, upgraded.
  This repository applies it to Servo.
- **Servo only.** No Chromium, CEF, WebView2 or Tauri (madde 50a); CI checks it.
- **The bar (`src/ui/`) talks only to `https://ui.eon.invalid/cmd`**; CI checks it.
- **A promise gets a `--selftest` check**, and the check is shown to fail when the
  promise is broken.
- **Fixes to Servo go upstream, written by hand**: Servo does not accept AI-written
  contributions.
- Nothing that phones home, no AI feature, no affiliate rewriting (madde 36, 50d).
