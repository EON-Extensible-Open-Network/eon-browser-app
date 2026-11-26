<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Security

Report privately through GitHub's **Security → Report a vulnerability** on this
repository; the organisation-wide policy is
[`.github/SECURITY.md`](https://github.com/EON-Extensible-Open-Network/.github/blob/main/SECURITY.md).

## What is not protected yet — said first

- **Servo runs pages in the browser's own process, without a sandbox, on Windows**, and
  without site isolation on every platform. A bug in the engine that a page can exploit
  is not contained the way Chrome and Firefox contain it. Servo itself is offered "as is,
  without security guarantees". Until this changes, EON Browser is not for banking or
  anything sensitive, and every release note says so.
- Servo's JavaScript engine (SpiderMonkey) receives security fixes about one monthly
  release after Firefox. EON Browser follows Servo's releases.

## What the browser itself guarantees, each checked by `--selftest`

- **The bar's bridge is unreachable from pages.** Requests from a page to
  `ui.eon.invalid` fail; the bar's view loads nothing but its own compiled-in files and
  refuses every other request.
- **Pages cannot act for the person.** Links to the browser's own action addresses
  (`/choose`, `/proceed`, `/allow-http`) work only from the browser's own pages.
- **Blocked requests never reach the network layer**, and a site a list names as harmful
  shows a warning page instead of loading.
- **Typed `javascript:` and `data:` addresses are searched, never run.**
- **A private window writes nothing**: profile in memory, engine storage in memory.
