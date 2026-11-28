// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Servo, set up the way madde 50 asks.

use std::path::Path;

use eon_browser_core::fingerprint::{self, Platform};
use servo::{EventLoopWaker, Opts, Preferences, ServoBuilder};

/// A builder for the engine. A private window keeps cookies and storage in memory only;
/// otherwise they live in `data/engine`. The user agent is Firefox ESR's (madde 50f).
/// `proxy` is the route process's address (madde 50k): every connection goes through it.
pub fn builder(
    private: bool,
    data: &Path,
    proxy: Option<&str>,
    waker: Box<dyn EventLoopWaker>,
) -> ServoBuilder {
    let mut opts = Opts::default();
    if private {
        opts.temporary_storage = true;
        opts.config_dir = None;
    } else {
        opts.config_dir = Some(data.join("engine"));
    }
    let prefs = Preferences {
        user_agent: fingerprint::user_agent(Platform::current()),
        // WebGPU stays off: attack surface and an unstudied fingerprinting surface.
        dom_webgpu_enabled: false,
        network_http_proxy_uri: proxy.unwrap_or_default().to_string(),
        network_https_proxy_uri: proxy.unwrap_or_default().to_string(),
        network_http_no_proxy: String::new(),
        ..Preferences::default()
    };
    ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(waker)
}

/// Install the TLS crypto provider Servo's networking expects. Once per process.
pub fn install_crypto() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    /// YouTube's front page sends more than hyper's default 16 KiB of HTTP/2 headers; the
    /// patched `servo-net` raises the limit (patches/README.md). A Servo upgrade that drops
    /// the patch without carrying the change upstream must fail here, not on YouTube.
    #[test]
    fn servo_net_accepts_large_http2_headers() {
        let manifest = include_str!("../Cargo.toml");
        let connector = include_str!("../patches/servo-net/connector.rs");
        assert!(manifest.contains("servo-net = { path = \"patches/servo-net\" }"));
        assert!(connector.contains(".http2_max_header_list_size(256 * 1024)"));
    }
}
