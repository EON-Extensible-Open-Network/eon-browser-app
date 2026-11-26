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
