<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Patched dependencies

Each directory here is a published crate, copied unchanged except for the lines listed,
and used through `[patch.crates-io]` in the root `Cargo.toml`. Each is removed as soon as
the upstream release carries the same change. The files keep their own licence (REUSE.toml
says which).

## `servo-net` 0.8.0 (MPL-2.0, the Servo Project)

**Changed:** `connector.rs`, `create_http_client`: one line,
`.http2_max_header_list_size(256 * 1024)`.

**Why:** hyper's default limit for an HTTP/2 response's header list is 16 KiB, and Servo
keeps it. YouTube's front page sends more than that (cookies, security policies, origin
trials), so the `h2` crate resets the stream itself (`REQUEST_HEADER_FIELDS_TOO_LARGE` →
`PROTOCOL_ERROR`, `Library`) and the page shows `client error (SendRequest)`. Found on
28 November 2026 with `RUST_LOG=net=debug,h2=debug`. 256 KiB is Chrome's limit.

**Also removed from the copy:** the crate's `tests/` folder and its `[[test]]` target
(they test Servo, not this change, and pull in test-only files), `Cargo.lock` and
`.cargo_vcs_info.json`. Added to its `Cargo.toml`: `[lints.rust] warnings = "allow"`, since
cargo hides a registry crate's warnings but shows a path crate's.

**To update:** when moving to a newer Servo, check whether `create_http_client` sets the
limit. If it does, delete this directory and the `[patch.crates-io]` entry. If it does
not, copy the new `servo-net` here and make the same one-line change.
