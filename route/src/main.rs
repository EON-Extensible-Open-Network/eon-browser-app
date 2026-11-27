// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! `eon-browser-route`: started by EON Browser, never by a person.
//!
//! ```text
//! eon-browser-route --tor-dir <dir>
//! stdin, first line:   the secret the proxy will demand
//! stdout:              ready 127.0.0.1:<port>
//! stdin, then:         route {"kind":"tor"} | route {"kind":"direct"} |
//!                      route {"kind":"proxy","address":"http://host:port"} |
//!                      new-identity | status
//! stdout, then:        status {"route":…,"tor":…}   (on every change, and when asked)
//! ```
//!
//! It exits when the browser closes its stdin, so it can never outlive the browser.

mod route;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use route::{Route, Router};

fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    let tor_dir = args
        .iter()
        .position(|a| a == "--tor-dir")
        .and_then(|i| args.get(i + 1))
        .map_or_else(
            || std::env::temp_dir().join("eon-browser-tor"),
            PathBuf::from,
        );

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let Some(Ok(secret)) = lines.next() else {
        std::process::exit(2);
    };
    let router = match Router::start(&Route::Direct, tor_dir, secret.trim().to_string()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eon-browser-route: {e}");
            std::process::exit(2);
        }
    };
    say(&format!("ready {}", router.local_addr()));

    // Report changes in Tor's state as they happen.
    let watcher = router.clone();
    std::thread::spawn(move || {
        let mut last = String::new();
        loop {
            let now = status(&watcher);
            if now != last {
                say(&format!("status {now}"));
                last = now;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    });

    for line in lines {
        let Ok(line) = line else { break };
        let line = line.trim();
        if let Some(json) = line.strip_prefix("route ") {
            if let Ok(r) = serde_json::from_str::<Route>(json) {
                router.set_route(&r);
            }
        } else if line == "new-identity" {
            router.new_identity();
        } else if line == "status" {
            say(&format!("status {}", status(&router)));
        }
    }
    // stdin closed: the browser is gone.
    std::process::exit(0);
}

fn status(r: &Router) -> String {
    serde_json::json!({ "route": r.route(), "tor": r.tor_state() }).to_string()
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
