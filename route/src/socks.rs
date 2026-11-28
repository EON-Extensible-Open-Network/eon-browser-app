// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! A SOCKS5 client (RFC 1928, with RFC 1929 user/password), for routes through a SOCKS
//! server: **Tor's own proxy** (the Tor service on `127.0.0.1:9050`, Tor Browser on
//! `127.0.0.1:9150`) or any SOCKS5 server the person brings.
//!
//! The site's *name* is sent, never an address the browser resolved itself: through Tor
//! the name is resolved at the exit, so the DNS question does not leave the computer in
//! the clear (and `.onion` names work).

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// `socks5://[user:pass@]host:port` → (host, port, credentials).
/// A SOCKS server's host, port and optional user name and password.
pub type Server = (String, u16, Option<(String, String)>);

pub fn parse(address: &str) -> Option<Server> {
    let u = url::Url::parse(address).ok()?;
    if !matches!(u.scheme(), "socks5" | "socks5h" | "socks") {
        return None;
    }
    let creds = (!u.username().is_empty()).then(|| {
        (
            u.username().to_string(),
            u.password().unwrap_or("").to_string(),
        )
    });
    Some((
        u.host_str()?.trim_matches(['[', ']']).to_string(),
        u.port().unwrap_or(1080),
        creds,
    ))
}

fn err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// Connect to `target:port` through the SOCKS5 server at `proxy`.
pub async fn connect(
    proxy: (&str, u16),
    creds: Option<&(String, String)>,
    target: &str,
    port: u16,
) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect(proxy).await?;
    // Greeting: no authentication, and user/password when we have them.
    if creds.is_some() {
        s.write_all(&[5, 2, 0, 2]).await?;
    } else {
        s.write_all(&[5, 1, 0]).await?;
    }
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await?;
    if reply[0] != 5 {
        return Err(err("not a SOCKS5 server"));
    }
    match (reply[1], creds) {
        (0, _) => {}
        (2, Some((user, pass))) => {
            if user.len() > 255 || pass.len() > 255 {
                return Err(err("SOCKS credentials too long"));
            }
            let mut msg = vec![1, user.len() as u8];
            msg.extend(user.as_bytes());
            msg.push(pass.len() as u8);
            msg.extend(pass.as_bytes());
            s.write_all(&msg).await?;
            let mut r = [0u8; 2];
            s.read_exact(&mut r).await?;
            if r[1] != 0 {
                return Err(err("the SOCKS server refused the user name or password"));
            }
        }
        _ => return Err(err("the SOCKS server asks for a method we do not offer")),
    }
    // CONNECT by name (ATYP 3), or by address when the target is a literal IP.
    let mut req = vec![5, 1, 0];
    if let Ok(ip) = target.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(v4) => {
                req.push(1);
                req.extend(v4.octets());
            }
            std::net::IpAddr::V6(v6) => {
                req.push(4);
                req.extend(v6.octets());
            }
        }
    } else {
        if target.len() > 255 {
            return Err(err("host name too long for SOCKS"));
        }
        req.push(3);
        req.push(target.len() as u8);
        req.extend(target.as_bytes());
    }
    req.extend(port.to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0 {
        return Err(err(&format!(
            "the SOCKS server could not connect (code {})",
            head[1]
        )));
    }
    // Skip the bound address.
    let skip = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut len = [0u8; 1];
            s.read_exact(&mut len).await?;
            usize::from(len[0]) + 2
        }
        _ => return Err(err("a malformed SOCKS reply")),
    };
    let mut rest = vec![0u8; skip];
    s.read_exact(&mut rest).await?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn addresses() {
        assert_eq!(
            parse("socks5://127.0.0.1:9150"),
            Some(("127.0.0.1".into(), 9150, None))
        );
        assert_eq!(
            parse("socks5h://u:p@proxy.example:1080"),
            Some(("proxy.example".into(), 1080, Some(("u".into(), "p".into()))))
        );
        assert_eq!(parse("http://x:1"), None);
    }

    /// A tiny SOCKS5 server: checks the request carries the *name*, then echoes.
    #[tokio::test]
    async fn connects_by_name_and_relays() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|_| unreachable!());
        let addr = listener.local_addr().unwrap_or_else(|_| unreachable!());
        let server = tokio::spawn(async move {
            let (mut c, _) = listener.accept().await.ok()?;
            let mut g = [0u8; 3];
            c.read_exact(&mut g).await.ok()?;
            c.write_all(&[5, 0]).await.ok()?;
            let mut h = [0u8; 5];
            c.read_exact(&mut h).await.ok()?;
            let mut name = vec![0u8; usize::from(h[4])];
            c.read_exact(&mut name).await.ok()?;
            let mut port = [0u8; 2];
            c.read_exact(&mut port).await.ok()?;
            c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok()?;
            let mut b = [0u8; 3];
            c.read_exact(&mut b).await.ok()?;
            c.write_all(&b).await.ok()?;
            Some((
                h[3],
                String::from_utf8(name).ok()?,
                u16::from_be_bytes(port),
            ))
        });
        let mut s = connect(
            ("127.0.0.1", addr.port()),
            None,
            "duckduckgogg42.onion",
            443,
        )
        .await
        .unwrap_or_else(|e| unreachable!("{e}"));
        s.write_all(b"eon").await.unwrap_or_else(|_| unreachable!());
        let mut b = [0u8; 3];
        s.read_exact(&mut b)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(&b, b"eon");
        let seen = server.await.ok().flatten();
        assert_eq!(
            seen,
            Some((3, "duckduckgogg42.onion".to_string(), 443)),
            "sent by name"
        );
    }
}
