// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Counting and limiting what flows through the route (Opera GX's network limiter, plan
//! madde 50l): every connection of the browser goes through this process, so one shared
//! meter sees all of it. Limits are bytes per second, down and up separately; `0` is no
//! limit.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// One direction's budget: bytes allowed in the current one-second window.
struct Window {
    start: Instant,
    used: u64,
}

pub struct Meter {
    pub down: AtomicU64,
    pub up: AtomicU64,
    down_limit: AtomicU64,
    up_limit: AtomicU64,
    down_window: Mutex<Window>,
    up_window: Mutex<Window>,
}

impl Default for Meter {
    fn default() -> Self {
        let w = || {
            Mutex::new(Window {
                start: Instant::now(),
                used: 0,
            })
        };
        Meter {
            down: AtomicU64::new(0),
            up: AtomicU64::new(0),
            down_limit: AtomicU64::new(0),
            up_limit: AtomicU64::new(0),
            down_window: w(),
            up_window: w(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Down,
    Up,
}

impl Meter {
    pub fn set_limits(&self, down: u64, up: u64) {
        self.down_limit.store(down, Ordering::Relaxed);
        self.up_limit.store(up, Ordering::Relaxed);
    }

    pub fn limits(&self) -> (u64, u64) {
        (
            self.down_limit.load(Ordering::Relaxed),
            self.up_limit.load(Ordering::Relaxed),
        )
    }

    /// Account for `n` bytes; returns how long to wait before sending more, if the
    /// direction is over its limit for this second.
    fn take(&self, dir: Direction, n: u64) -> Option<Duration> {
        let (total, limit, window) = match dir {
            Direction::Down => (&self.down, &self.down_limit, &self.down_window),
            Direction::Up => (&self.up, &self.up_limit, &self.up_window),
        };
        total.fetch_add(n, Ordering::Relaxed);
        let limit = limit.load(Ordering::Relaxed);
        if limit == 0 {
            return None;
        }
        let mut w = window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(w.start) >= Duration::from_secs(1) {
            w.start = now;
            w.used = 0;
        }
        w.used += n;
        if w.used > limit {
            // Over budget: wait out this window, plus whatever the excess needs.
            let excess = w.used - limit;
            // Byte counts within one second are far below f64's 2^52 exact range.
            #[allow(clippy::cast_precision_loss)]
            let extra = Duration::from_secs_f64(excess as f64 / limit as f64);
            Some(Duration::from_secs(1).saturating_sub(now.duration_since(w.start)) + extra)
        } else {
            None
        }
    }
}

/// Copy `from` → `to`, counting and pacing as `dir`.
async fn pump<R, W>(mut from: R, mut to: W, meter: &Meter, dir: Direction) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Small reads when limited, so pacing is smooth; 32 KiB otherwise.
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let (down, up) = meter.limits();
        let limit = if dir == Direction::Down { down } else { up };
        let cap = if limit == 0 {
            buf.len()
        } else {
            (limit as usize / 8).clamp(1024, buf.len())
        };
        let n = from.read(&mut buf[..cap]).await?;
        if n == 0 {
            let _ = to.shutdown().await;
            return Ok(());
        }
        to.write_all(&buf[..n]).await?;
        if let Some(wait) = meter.take(dir, n as u64) {
            tokio::time::sleep(wait).await;
        }
    }
}

/// Relay between the browser (`client`) and a site (`upstream`) both ways, through the
/// meter, until either side closes.
pub async fn relay<A, B>(client: A, upstream: B, meter: &Meter)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (cr, cw) = tokio::io::split(client);
    let (ur, uw) = tokio::io::split(upstream);
    let up = pump(cr, uw, meter, Direction::Up);
    let down = pump(ur, cw, meter, Direction::Down);
    let _ = tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_without_a_limit() {
        let m = Meter::default();
        assert_eq!(m.take(Direction::Down, 1000), None);
        assert_eq!(m.take(Direction::Up, 10), None);
        assert_eq!(m.down.load(Ordering::Relaxed), 1000);
        assert_eq!(m.up.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn waits_when_over_the_limit() {
        let m = Meter::default();
        m.set_limits(1000, 0);
        assert_eq!(m.take(Direction::Down, 600), None);
        let wait = m.take(Direction::Down, 600).unwrap_or_default();
        assert!(wait > Duration::from_millis(900), "{wait:?}");
        assert_eq!(m.take(Direction::Up, 10_000_000), None, "up is unlimited");
    }

    #[tokio::test]
    async fn a_limited_relay_takes_as_long_as_it_should() {
        let meter = Meter::default();
        meter.set_limits(64 * 1024, 0);
        // browser <-> (client end | relay | site end) <-> site
        let (mut browser, client_end) = tokio::io::duplex(1 << 20);
        let (site_end, mut site) = tokio::io::duplex(1 << 20);
        let data = vec![7u8; 160 * 1024];
        let writer = tokio::spawn(async move {
            let _ = site.write_all(&data).await;
            let _ = site.shutdown().await;
        });
        let start = Instant::now();
        let relay_task = relay(client_end, site_end, &meter);
        let reader = async {
            let mut got = Vec::new();
            let _ = browser.read_to_end(&mut got).await;
            // The browser closes its side too, so the upward copy ends.
            let _ = browser.shutdown().await;
            got.len()
        };
        let ((), len) = tokio::join!(relay_task, reader);
        let _ = writer.await;
        assert_eq!(len, 160 * 1024);
        // 160 KiB at 64 KiB/s: about two seconds or more.
        assert!(
            start.elapsed() >= Duration::from_millis(1500),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(meter.down.load(Ordering::Relaxed), 160 * 1024);
    }
}
