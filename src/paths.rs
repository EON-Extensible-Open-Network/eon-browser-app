// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Where EON Browser keeps what it writes, and the few settings that are not per site.
//!
//! Everything goes in one folder beside the executable (`eon-browser-data`), as every EON
//! program does, unless `EON_BROWSER_DATA` names another. A private window writes nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The data folder.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("EON_BROWSER_DATA") {
        return PathBuf::from(d);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("eon-browser-data")
}

/// Settings that belong to the person, not to a site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The search engine the person chose; `None` until they chose (madde 50e).
    pub search_engine: Option<String>,
    /// The route (madde 50k): `direct` (or empty), `tor`, `proxy`.
    pub route: String,
    /// The proxy server's address, for `proxy`.
    pub route_address: String,
    /// Put a background tab to sleep after this many minutes (Firefox's and Edge's
    /// sleeping tabs). `None` means the default, 15; `Some(0)` means never.
    pub sleep_after_minutes: Option<u32>,
    /// Opera GX's RAM limiter: past this many MiB, background tabs go to sleep, the least
    /// recently used first. 0 is no limit.
    pub ram_limit_mb: u32,
    /// Opera GX's network limiter, KiB/s down and up. 0 is no limit.
    pub net_down_kbps: u32,
    pub net_up_kbps: u32,
}

impl Settings {
    pub fn sleep_after(&self) -> Option<std::time::Duration> {
        match self.sleep_after_minutes.unwrap_or(15) {
            0 => None,
            m => Some(std::time::Duration::from_secs(u64::from(m) * 60)),
        }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join("settings.json"))
    }
}
