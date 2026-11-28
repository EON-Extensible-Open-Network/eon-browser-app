// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The browser's side of the route process (`eon-browser-route`, beside the executable;
//! plan madde 50k). It is started with the browser, given a secret, and Servo is pointed
//! at its proxy; the route (direct, Tor, a proxy server) is changed by a line on its stdin.
//! When the browser goes, its stdin closes and the process exits.

use std::fmt::Write as _;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

/// What the route process last reported.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// `direct`, `tor` or `proxy`.
    pub kind: String,
    /// The proxy server's address, for `proxy`.
    pub address: String,
    /// `off`, `connecting`, `ready` or `failed`.
    pub tor: String,
    /// Why Tor failed, if it did.
    pub detail: String,
}

pub struct RouteClient {
    pid: u32,
    /// Bytes through the route so far, down and up.
    bytes: Arc<Mutex<(u64, u64)>>,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    status: Arc<Mutex<Status>>,
    changed: Arc<AtomicBool>,
    proxy_uri: String,
}

impl RouteClient {
    /// Start the route process found beside this executable. `None` if it is missing or
    /// does not come up: the browser then connects directly and says Tor is unavailable.
    pub fn start(tor_dir: &Path) -> Option<RouteClient> {
        let exe = std::env::current_exe().ok()?;
        let name = if cfg!(windows) {
            "eon-browser-route.exe"
        } else {
            "eon-browser-route"
        };
        let path = exe.parent()?.join(name);
        if !path.exists() {
            return None;
        }
        let mut command = Command::new(path);
        command
            .arg("--tor-dir")
            .arg(tor_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().ok()?;
        let secret = secret();
        let mut stdin = child.stdin.take()?;
        writeln!(stdin, "{secret}").ok()?;
        stdin.flush().ok()?;
        let stdout = child.stdout.take()?;

        let status = Arc::new(Mutex::new(Status {
            kind: "direct".into(),
            tor: "off".into(),
            ..Status::default()
        }));
        let changed = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(Mutex::new((0u64, 0u64)));
        let (ready_tx, ready_rx) = mpsc::channel::<String>();
        let (st, ch, by) = (status.clone(), changed.clone(), bytes.clone());
        std::thread::Builder::new()
            .name("eon-route-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(addr) = line.strip_prefix("ready ") {
                        let _ = ready_tx.send(addr.trim().to_string());
                    } else if let Some(json) = line.strip_prefix("stats ") {
                        if let Ok(v) = serde_json::from_str::<Value>(json) {
                            let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
                            *by.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                                (n("down"), n("up"));
                        }
                    } else if let Some(json) = line.strip_prefix("status ") {
                        if let Ok(v) = serde_json::from_str::<Value>(json) {
                            let mut s =
                                st.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            *s = parse_status(&v);
                            ch.store(true, Ordering::SeqCst);
                        }
                    }
                }
            })
            .ok()?;
        let Ok(addr) = ready_rx.recv_timeout(Duration::from_secs(10)) else {
            let _ = child.kill();
            return None;
        };
        Some(RouteClient {
            pid: child.id(),
            bytes,
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            status,
            changed,
            proxy_uri: format!("http://eon:{secret}@{addr}"),
        })
    }

    /// The address Servo is given.
    pub fn proxy_uri(&self) -> &str {
        &self.proxy_uri
    }

    /// The `Proxy-Authorization` value the proxy expects, for the self-test.
    pub fn proxy_authorization(&self) -> String {
        let creds = self
            .proxy_uri
            .strip_prefix("http://")
            .and_then(|r| r.rsplit_once('@'))
            .map_or("", |(c, _)| c);
        format!("Basic {}", base64(creds))
    }

    /// The local address, for the self-test.
    pub fn local_addr(&self) -> Option<String> {
        self.proxy_uri.rsplit_once('@').map(|(_, a)| a.to_string())
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the status changed since the last call.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::SeqCst)
    }

    fn send(&self, line: &str) {
        let mut guard = self
            .stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stdin) = guard.as_mut() {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }

    /// `kind`: `direct`, `tor` or `proxy` (with `address`, `http://[user:pass@]host:port`).
    pub fn set_route(&self, kind: &str, address: &str) {
        let json = match kind {
            "tor" => serde_json::json!({ "kind": "tor" }),
            "proxy" => serde_json::json!({ "kind": "proxy", "address": address }),
            "socks" => serde_json::json!({ "kind": "socks", "address": address }),
            _ => serde_json::json!({ "kind": "direct" }),
        };
        self.send(&format!("route {json}"));
    }

    /// The route process's id, for the monitor.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Bytes through the route so far (down, up).
    pub fn bytes(&self) -> (u64, u64) {
        *self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The network limiter, in KiB/s (0: no limit).
    pub fn set_limits(&self, down_kbps: u32, up_kbps: u32) {
        self.send(&format!(
            "limit {} {}",
            u64::from(down_kbps) * 1024,
            u64::from(up_kbps) * 1024
        ));
    }

    pub fn new_identity(&self) {
        self.send("new-identity");
    }
}

impl Drop for RouteClient {
    fn drop(&mut self) {
        // Closing stdin tells the process to exit; it is killed if it has not.
        drop(
            self.stdin
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
    }
}

fn parse_status(v: &Value) -> Status {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    Status {
        kind: s(v.pointer("/route/kind")),
        address: s(v.pointer("/route/address")),
        tor: s(v.pointer("/tor/state")),
        detail: s(v.pointer("/tor/detail")),
    }
}

/// 128 bits for the proxy's password: two hashers keyed from the operating system's
/// randomness (std seeds `RandomState` from it), over the time and the process id.
fn secret() -> String {
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        let _ = write!(out, "{:016x}", h.finish());
    }
    out
}

fn base64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let v: Value = serde_json::from_str(
            r#"{"route":{"kind":"proxy","address":"http://p:3128"},"tor":{"state":"failed","detail":"no network"}}"#,
        )
        .unwrap_or_default();
        let s = parse_status(&v);
        assert_eq!(
            (s.kind.as_str(), s.address.as_str()),
            ("proxy", "http://p:3128")
        );
        assert_eq!(
            (s.tor.as_str(), s.detail.as_str()),
            ("failed", "no network")
        );
    }

    #[test]
    fn secrets_are_long_and_differ() {
        let (a, b) = (secret(), secret());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
