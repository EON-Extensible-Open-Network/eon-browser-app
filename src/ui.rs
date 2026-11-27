// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The bar's files, compiled into the program: there is nothing to install beside it and
//! nothing the bar loads from anywhere else.

pub const FONT: &[u8] = include_bytes!("ui/fonts/Geist-Variable.woff2");

pub fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", include_bytes!("ui/index.html"))),
        "/style.css" => Some(("text/css; charset=utf-8", include_bytes!("ui/style.css"))),
        "/app.js" => Some((
            "text/javascript; charset=utf-8",
            include_bytes!("ui/app.js"),
        )),
        "/fonts/Geist-Variable.woff2" => Some(("font/woff2", FONT)),
        "/icons/back.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/back.svg"))),
        "/icons/forward.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/forward.svg"))),
        "/icons/reload.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/reload.svg"))),
        "/icons/plus.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/plus.svg"))),
        "/icons/close.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/close.svg"))),
        "/icons/lock.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/lock.svg"))),
        "/icons/open.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/open.svg"))),
        "/icons/page.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/page.svg"))),
        "/icons/shield.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/shield.svg"))),
        "/icons/shield-off.svg" => {
            Some(("image/svg+xml", include_bytes!("ui/icons/shield-off.svg")))
        }
        "/icons/search.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/search.svg"))),
        "/icons/clock.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/clock.svg"))),
        "/icons/url.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/url.svg"))),
        "/icons/star.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/star.svg"))),
        "/icons/star-on.svg" => Some(("image/svg+xml", include_bytes!("ui/icons/star-on.svg"))),
        _ => None,
    }
}
