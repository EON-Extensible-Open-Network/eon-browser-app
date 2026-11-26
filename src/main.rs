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
mod input;
mod pages;
mod paths;
mod selftest;
mod ui;
mod window;

mod seed {
    include!(concat!(env!("OUT_DIR"), "/seed.rs"));
}

use std::path::PathBuf;
use std::process::ExitCode;

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
                // Exit at once when the report is written: tearing the engine down at the
                // end of `main` can abort in SpiderMonkey (a mutex still held), which
                // would turn a passing run into a failing exit code.
                let code = match selftest::run(&report) {
                    Ok(0) => 0,
                    Ok(_) => 1,
                    Err(e) => {
                        eprintln!("eon-browser: self-test could not run: {e}");
                        2
                    }
                };
                std::process::exit(code);
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
