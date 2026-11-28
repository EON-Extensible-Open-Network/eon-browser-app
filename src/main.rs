// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! EON Browser (plan madde 50): a web browser on Servo, with `eon-browser-core`'s policy.
//!
//! ```text
//! eon-browser [address]          open the window (the last session, or the address)
//! eon-browser --private          a private window: nothing written to disk
//! eon-browser --selftest <file>  check the promises offscreen, write a report; non-zero on failure
//! eon-browser --version
//! ```

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod browser;
mod engine;
mod extensions;
mod favicon;
mod input;
mod monitor;
mod pages;
mod paths;
mod route_client;
mod selftest;
mod ui;
mod window;

mod seed {
    include!(concat!(env!("OUT_DIR"), "/seed.rs"));
}

use std::path::PathBuf;
use std::process::ExitCode;

/// `--install-extension <file or folder>`: install into the data folder, say what it may
/// do, and exit. The window picks it up at its next start.
fn install_extension(from: Option<PathBuf>) -> ExitCode {
    let Some(from) = from else {
        eprintln!("eon-browser: --install-extension needs a .crx, .xpi, .zip or a folder");
        return ExitCode::from(2);
    };
    let data = paths::data_dir();
    let mut lists = match eon_browser_core::lists::Lists::load(&data.join("lists")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("eon-browser: {e}");
            return ExitCode::from(2);
        }
    };
    let mut exts = extensions::Extensions::load(&data);
    match exts.install(&from, &mut lists) {
        Ok(e) => {
            println!(
                "Installed {} {} ({}).",
                e.manifest.name, e.manifest.version, e.source
            );
            for p in &e.permissions {
                println!("  - {p}");
            }
            if e.rules > 0 {
                println!(
                    "  {} blocking rules run in the shield ({} not translated).",
                    e.rules, e.rules_skipped
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("eon-browser: could not install: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    engine::install_crypto();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut private = false;
    let mut start = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("EON Browser {} (Servo 0.8.0)", browser::VERSION);
                return ExitCode::SUCCESS;
            }
            "--selftest" => {
                let report = args
                    .get(i + 1)
                    .map_or_else(|| PathBuf::from("selftest.txt"), PathBuf::from);
                return match selftest::run(&report) {
                    Ok(0) => ExitCode::SUCCESS,
                    Ok(_) => ExitCode::from(1),
                    Err(e) => {
                        eprintln!("eon-browser: self-test could not run: {e}");
                        ExitCode::from(2)
                    }
                };
            }
            "--install-extension" => {
                return install_extension(args.get(i + 1).map(PathBuf::from));
            }
            "--private" => private = true,
            other if !other.starts_with("--") => start = url::Url::parse(other).ok(),
            _ => {}
        }
        i += 1;
    }
    match window::run(private, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eon-browser: {e}");
            ExitCode::from(2)
        }
    }
}
