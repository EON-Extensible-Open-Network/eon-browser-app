// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The filter lists shipped with a release (plan madde 50e): the release workflow downloads
//! them into a folder and names it in `EON_BROWSER_SEED_DIR`; each `<list id>.txt` there is
//! compiled into the program, so blocking works on first start without one request.
//! A build without the variable ships no lists (development builds).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-env-changed=EON_BROWSER_SEED_DIR");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into()));
    let mut code = String::from("pub const SEED: &[(&str, &[u8])] = &[\n");
    let mut at = 0u64;
    if let Ok(dir) = std::env::var("EON_BROWSER_SEED_DIR") {
        println!("cargo:rerun-if-changed={dir}");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for p in files {
            if p.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let Some(id) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let _ = writeln!(
                code,
                "    ({id:?}, include_bytes!({:?})),",
                p.display().to_string()
            );
        }
        at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
    }
    let _ = writeln!(code, "];\npub const SEED_AT: u64 = {at};");
    if std::fs::write(out.join("seed.rs"), code).is_err() {
        std::process::exit(1);
    }
}
