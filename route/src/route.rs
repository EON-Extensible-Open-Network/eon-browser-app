// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The route every connection takes (plan madde 50k): directly, through **Tor**, or through
//! a **proxy server the person brings**.
//!
//! Servo is pointed, from the start, at a small proxy of the browser's own on `127.0.0.1`
//! that answers only to a secret it was given (so no other program on the computer can use
//! it). Servo opens every connection through it as an HTTP `CONNECT` tunnel; the proxy
//! sends each one the way the person chose, and the choice can change at any moment
//! without restarting anything.
//!
//! Tor runs inside the browser through **Arti**, the Tor Project's Rust implementation:
//!
//! - **a circuit per site** — two sites never share an exit, so they cannot be linked by
//!   it;
//! - **circuits renewed every ten minutes**, so a site sees the address change;
//! - **New identity** — every circuit dropped and every cookie cleared at once.
//!
//! Tor is not contacted until the person picks it: the browser's promise of no request
//! before a page is opened holds (madde 50d). Choosing Tor hides the IP address; it does
//! not make a person as hard to recognise as Tor Browser does, and the window says so.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arti_client::{IsolationToken, StreamPrefs, TorClient, TorClientConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tor_rtcompat::PreferredRuntime;

/// How long a site keeps its Tor circuit before getting a new one (and a new exit).
pub const CIRCUIT_LIFETIME: Duration = Duration::from_mins(10);

/// The way connections go.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase", tag = "kind", content = "address")]
pub enum Route {
    #[default]
    Direct,
    Tor,
    /// An HTTP proxy the person brings: `http://[user:pass@]host:port`.
    Proxy(String),
    /// A SOCKS5 server: Tor's own proxy (`socks5://127.0.0.1:9050` for the Tor service,
    /// `:9150` for Tor Browser) or any the person brings. Names are resolved by the server.
    Socks(String),
}

/// What Tor is doing, for the window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase", tag = "state", content = "detail")]
pub enum TorState {
    Off,
    Connecting,
    Ready,
    Failed(String),
}

struct Shared {
    route: Route,
    tor_state: TorState,
    tor: Option<Arc<TorClient<PreferredRuntime>>>,
    /// site → (its isolation token, since when).
    circuits: HashMap<String, (IsolationToken, Instant)>,
}

/// The browser's proxy and its route.
pub struct Router {
    shared: Arc<Mutex<Shared>>,
    addr: SocketAddr,
    secret: String,
    runtime: tokio::runtime::Handle,
    tor_dir: PathBuf,
    /// What flowed through, and the person's speed limits.
    pub meter: Arc<crate::meter::Meter>,
}

impl Router {
    /// Start the proxy on a free local port. `tor_dir` keeps Arti's directory cache and
    /// guards; a private window gets a throw-away one.
    pub fn start(route: &Route, tor_dir: PathBuf, secret: String) -> std::io::Result<Arc<Router>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("eon-route")
            .enable_all()
            .build()?;
        let listener = runtime.block_on(TcpListener::bind(("127.0.0.1", 0)))?;
        let addr = listener.local_addr()?;
        let router = Arc::new(Router {
            shared: Arc::new(Mutex::new(Shared {
                route: Route::Direct,
                tor_state: TorState::Off,
                tor: None,
                circuits: HashMap::new(),
            })),
            addr,
            secret,
            runtime: runtime.handle().clone(),
            tor_dir,
            meter: Arc::new(crate::meter::Meter::default()),
        });
        let r = router.clone();
        std::thread::Builder::new()
            .name("eon-route".into())
            .spawn(move || {
                runtime.block_on(async move {
                    loop {
                        let Ok((stream, _)) = listener.accept().await else {
                            continue;
                        };
                        let r = r.clone();
                        tokio::spawn(async move {
                            let _ = r.serve(stream).await;
                        });
                    }
                });
            })?;
        router.set_route(route);
        Ok(router)
    }

    pub fn route(&self) -> Route {
        self.lock().route.clone()
    }

    pub fn tor_state(&self) -> TorState {
        self.lock().tor_state.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Change the route. Choosing Tor starts connecting to the Tor network now.
    pub fn set_route(self: &Arc<Self>, route: &Route) {
        let start_tor = {
            let mut s = self.lock();
            s.route = route.clone();
            *route == Route::Tor && s.tor.is_none() && s.tor_state != TorState::Connecting
        };
        if start_tor {
            self.lock().tor_state = TorState::Connecting;
            let me = self.clone();
            self.runtime.spawn(async move {
                let result = me.bootstrap().await;
                let mut s = me.lock();
                match result {
                    Ok(client) => {
                        s.tor = Some(client);
                        s.tor_state = TorState::Ready;
                    }
                    Err(e) => s.tor_state = TorState::Failed(e),
                }
            });
        }
    }

    async fn bootstrap(&self) -> Result<Arc<TorClient<PreferredRuntime>>, String> {
        let mut cfg = TorClientConfig::builder();
        cfg.storage()
            .cache_dir(arti_client::config::CfgPath::new_literal(
                self.tor_dir.join("cache"),
            ))
            .state_dir(arti_client::config::CfgPath::new_literal(
                self.tor_dir.join("state"),
            ));
        let cfg = cfg.build().map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(90), TorClient::create_bootstrapped(cfg))
            .await
            .map_err(|_| "the Tor network did not answer within 90 seconds".to_string())?
            .map_err(|e| e.to_string())
    }

    /// New identity: every site gets a new circuit (and so a new exit) on its next
    /// connection. The browser clears cookies and storage alongside.
    pub fn new_identity(&self) {
        self.lock().circuits.clear();
    }

    /// The circuit a connection to `host` uses: one per site, renewed after its lifetime.
    fn isolation(&self, host: &str) -> IsolationToken {
        let site = eon_browser_core::site::of_host(host);
        let mut s = self.lock();
        let now = Instant::now();
        let entry = s
            .circuits
            .entry(site)
            .or_insert((IsolationToken::new(), now));
        if now.duration_since(entry.1) > CIRCUIT_LIFETIME {
            *entry = (IsolationToken::new(), now);
        }
        entry.0
    }

    #[allow(clippy::too_many_lines)] // one match over the routes, each a few lines
    async fn serve(self: Arc<Self>, mut client: TcpStream) -> std::io::Result<()> {
        let head = read_head(&mut client).await?;
        let Some((target, auth)) = parse_connect(&head) else {
            return client
                .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
                .await;
        };
        if auth.as_deref()
            != Some(&format!(
                "Basic {}",
                base64(&format!("eon:{}", self.secret))
            ))
        {
            return client
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
        }
        let Some((host, port)) = split_host_port(&target) else {
            return client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        };
        let route = self.route();
        match route {
            Route::Direct => {
                if host.rsplit('.').next() == Some("onion") {
                    return client
                        .write_all(b"HTTP/1.1 502 Onion addresses need the Tor route\r\n\r\n")
                        .await;
                }
                match TcpStream::connect((host.as_str(), port)).await {
                    Ok(mut upstream) => {
                        client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await?;
                        crate::meter::relay(&mut client, &mut upstream, &self.meter).await;
                    }
                    Err(_) => {
                        client
                            .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                            .await?;
                    }
                }
            }
            Route::Tor => {
                let tor = self.wait_for_tor().await;
                let Some(tor) = tor else {
                    return client
                        .write_all(b"HTTP/1.1 503 Tor is not connected\r\n\r\n")
                        .await;
                };
                let mut prefs = StreamPrefs::new();
                prefs.set_isolation(self.isolation(&host));
                prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));
                match tor.connect_with_prefs((host.as_str(), port), &prefs).await {
                    Ok(mut upstream) => {
                        client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await?;
                        crate::meter::relay(&mut client, &mut upstream, &self.meter).await;
                    }
                    Err(_) => {
                        client
                            .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                            .await?;
                    }
                }
            }
            Route::Socks(server) => {
                let Some((s_host, s_port, creds)) = crate::socks::parse(&server) else {
                    return client
                        .write_all(b"HTTP/1.1 502 The SOCKS address is not usable\r\n\r\n")
                        .await;
                };
                match crate::socks::connect((s_host.as_str(), s_port), creds.as_ref(), &host, port)
                    .await
                {
                    Ok(mut upstream) => {
                        client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await?;
                        crate::meter::relay(&mut client, &mut upstream, &self.meter).await;
                    }
                    Err(_) => {
                        client
                            .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                            .await?;
                    }
                }
            }
            Route::Proxy(upstream) => {
                let Some((up_host, up_port, up_auth)) = parse_upstream(&upstream) else {
                    return client
                        .write_all(b"HTTP/1.1 502 The proxy address is not usable\r\n\r\n")
                        .await;
                };
                let Ok(mut up) = TcpStream::connect((up_host.as_str(), up_port)).await else {
                    return client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                };
                let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
                if let Some(a) = up_auth {
                    req.push_str("Proxy-Authorization: Basic ");
                    req.push_str(&base64(&a));
                    req.push_str("\r\n");
                }
                req.push_str("\r\n");
                up.write_all(req.as_bytes()).await?;
                let reply = read_head(&mut up).await?;
                if reply.starts_with("HTTP/1.1 200") || reply.starts_with("HTTP/1.0 200") {
                    client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await?;
                    crate::meter::relay(&mut client, &mut up, &self.meter).await;
                } else {
                    client
                        .write_all(b"HTTP/1.1 502 The proxy refused\r\n\r\n")
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn wait_for_tor(&self) -> Option<Arc<TorClient<PreferredRuntime>>> {
        for _ in 0..900 {
            {
                let s = self.lock();
                if let Some(t) = &s.tor {
                    return Some(t.clone());
                }
                if matches!(s.tor_state, TorState::Failed(_) | TorState::Off) {
                    return None;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Where the proxy listens.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Read a request or response head, up to the blank line (at most 16 KiB).
async fn read_head(s: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while buf.len() < 16 * 1024 {
        if s.read(&mut byte).await? == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `CONNECT host:port` and its `Proxy-Authorization`, if the head is one.
fn parse_connect(head: &str) -> Option<(String, Option<String>)> {
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let mut parts = first.split(' ');
    if parts.next()? != "CONNECT" {
        return None;
    }
    let target = parts.next()?.to_string();
    let auth = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("proxy-authorization"))
        .map(|(_, v)| v.trim().to_string());
    Some((target, auth))
}

fn split_host_port(target: &str) -> Option<(String, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_string(), port.parse().ok()?))
}

/// `http://[user:pass@]host:port` → host, port, `user:pass`.
fn parse_upstream(address: &str) -> Option<(String, u16, Option<String>)> {
    let u = url::Url::parse(address).ok()?;
    if u.scheme() != "http" {
        return None;
    }
    let auth = (!u.username().is_empty())
        .then(|| format!("{}:{}", u.username(), u.password().unwrap_or("")));
    Some((u.host_str()?.to_string(), u.port_or_known_default()?, auth))
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
    fn base64_matches_the_standard() {
        assert_eq!(base64("eon:abc"), "ZW9uOmFiYw==");
        assert_eq!(base64("user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64("a"), "YQ==");
    }

    #[test]
    fn connect_heads() {
        let (t, a) = parse_connect("CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\nProxy-Authorization: Basic eA==\r\n\r\n")
            .unwrap_or_default();
        assert_eq!(t, "example.com:443");
        assert_eq!(a.as_deref(), Some("Basic eA=="));
        assert!(parse_connect("GET http://x/ HTTP/1.1\r\n\r\n").is_none());
        assert_eq!(split_host_port("[::1]:8080"), Some(("::1".into(), 8080)));
        assert_eq!(
            parse_upstream("http://u:p@proxy.example:3128"),
            Some(("proxy.example".into(), 3128, Some("u:p".into())))
        );
        assert_eq!(parse_upstream("socks5://x:1"), None);
    }
}
