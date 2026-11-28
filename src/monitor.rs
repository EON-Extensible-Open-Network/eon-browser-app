// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! What the browser costs right now (Opera GX's monitor, plan madde 50l): memory and
//! processor of the browser and its route process together, and the network speed through
//! the route. Read on this computer, shown in the window, sent nowhere.

use std::time::Instant;

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// One reading.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// Resident memory of the browser and its route process, in MiB.
    pub ram_mb: u32,
    /// Processor use, as a share of the whole machine (0–100).
    pub cpu: f32,
    /// Network speed through the route, in KiB/s.
    pub down_kbps: u32,
    pub up_kbps: u32,
}

pub struct Monitor {
    system: System,
    own: Pid,
    cores: f32,
    last_bytes: Option<(u64, u64, Instant)>,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            system: System::new(),
            own: Pid::from_u32(std::process::id()),
            cores: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f32),
            last_bytes: None,
        }
    }
}

impl Monitor {
    /// Read memory and processor for the browser and `route` (the route process's id), and
    /// turn the route's byte totals into speeds since the last reading.
    pub fn sample(&mut self, route: Option<u32>, bytes: Option<(u64, u64)>) -> Sample {
        let mut pids = vec![self.own];
        if let Some(r) = route {
            pids.push(Pid::from_u32(r));
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let (mut ram, mut cpu) = (0u64, 0f32);
        for pid in &pids {
            if let Some(p) = self.system.process(*pid) {
                ram += p.memory();
                cpu += p.cpu_usage();
            }
        }
        let (mut down, mut up) = (0, 0);
        if let Some((d, u)) = bytes {
            let now = Instant::now();
            if let Some((ld, lu, at)) = self.last_bytes {
                let secs = now.duration_since(at).as_secs_f64().max(0.001);
                down = ((d.saturating_sub(ld)) as f64 / secs / 1024.0) as u32;
                up = ((u.saturating_sub(lu)) as f64 / secs / 1024.0) as u32;
            }
            self.last_bytes = Some((d, u, now));
        }
        Sample {
            ram_mb: (ram / (1024 * 1024)) as u32,
            cpu: (cpu / self.cores).clamp(0.0, 100.0),
            down_kbps: down,
            up_kbps: up,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let mut m = Monitor::default();
        let _ = m.sample(None, Some((0, 0)));
        let s = m.sample(None, Some((2048, 1024)));
        assert!(s.ram_mb > 0, "{s:?}");
        assert!(s.cpu >= 0.0 && s.cpu <= 100.0);
        // Bytes became a speed (some KiB/s, since only a moment passed).
        assert!(s.down_kbps >= s.up_kbps);
    }
}
