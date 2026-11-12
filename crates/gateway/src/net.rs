//! Public egress: every fetch of a public resource (pages, playlists, segments, keys) refuses
//! private, loopback, link-local, CGNAT, unique-local, multicast and metadata addresses on every
//! connection, redirects included.
//!
//! - The relay's HTTP client resolves names through [`PublicResolver`] and checks IP-literal hosts
//!   (and every redirect hop) with [`check_url`].
//! - The extractor subprocesses get `--proxy` pointing at [`EgressProxy`], an HTTP/CONNECT proxy
//!   on loopback that applies the same rule to every connection they make.
//!
//! The Acestream control client does not use any of this: it only talks to its configured origins.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Whether an address may be reached by public fetches.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0064 && s[1] == 0xff9b)) // NAT64 can reach IPv4 private ranges
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local() // includes 169.254.169.254 (cloud metadata)
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || o[0] >= 240) // reserved
}

/// Hosts exempt from the rule (tests only).
#[derive(Debug, Clone, Default)]
pub struct Allow(Arc<HashSet<String>>);

impl Allow {
    pub fn new(hosts: &[String]) -> Self {
        Allow(Arc::new(hosts.iter().map(|h| h.trim().to_ascii_lowercase()).collect()))
    }
    fn allows(&self, host: &str) -> bool {
        self.0
            .contains(&host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("refused: {0} is not a public address")]
    Private(String),
    #[error("refused: only http and https URLs are fetched")]
    Scheme,
    #[error("refused: URLs with credentials are not fetched")]
    Credentials,
    #[error("cannot resolve {0}: {1}")]
    Resolve(String, String),
}

/// Check a URL before fetching it: scheme, credentials, and an IP-literal host.
pub fn check_url(u: &url::Url, allow: &Allow) -> Result<(), EgressError> {
    if !matches!(u.scheme(), "http" | "https") {
        return Err(EgressError::Scheme);
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(EgressError::Credentials);
    }
    match u.host() {
        Some(url::Host::Ipv4(ip)) if !allow.allows(&ip.to_string()) && !public_v4(ip) => {
            Err(EgressError::Private(ip.to_string()))
        }
        Some(url::Host::Ipv6(ip)) if !allow.allows(&ip.to_string()) && !is_public(IpAddr::V6(ip)) => {
            Err(EgressError::Private(ip.to_string()))
        }
        None => Err(EgressError::Private("(no host)".into())),
        _ => Ok(()),
    }
}

/// Resolve `host` and keep only public addresses (all of them, for an allowed host).
pub async fn resolve_public(host: &str, port: u16, allow: &Allow) -> Result<Vec<SocketAddr>, EgressError> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if allow.allows(bare) || is_public(ip) {
            Ok(vec![SocketAddr::new(ip, port)])
        } else {
            Err(EgressError::Private(bare.into()))
        };
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|e| EgressError::Resolve(bare.into(), e.to_string()))?
        .collect();
    if allow.allows(bare) {
        return Ok(addrs);
    }
    // Any private answer refuses the name: a mixed answer is a rebinding attempt or a misconfiguration.
    if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
        return Err(EgressError::Private(format!("{bare} ({})", bad.ip())));
    }
    if addrs.is_empty() {
        return Err(EgressError::Resolve(bare.into(), "no addresses".into()));
    }
    Ok(addrs)
}

/// A reqwest resolver that refuses private answers.
pub struct PublicResolver(pub Allow);

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = resolve_public(&host, 0, &allow).await?;
            let it: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(it)
        })
    }
}

/// The HTTP client for public fetches: public-only DNS, every redirect hop checked, bounded.
pub fn public_client(allow: &Allow, timeout: Duration) -> reqwest::Client {
    let redirect_allow = allow.clone();
    reqwest::Client::builder()
        .dns_resolver(Arc::new(PublicResolver(allow.clone())))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            match check_url(attempt.url(), &redirect_allow) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }))
        .connect_timeout(timeout)
        .read_timeout(timeout)
        .build()
        .expect("building the public HTTP client")
}

/// An HTTP proxy on loopback for subprocesses: CONNECT tunnels and absolute-form plain HTTP, each
/// to a public address only.
pub struct EgressProxy {
    pub addr: SocketAddr,
}

impl EgressProxy {
    pub async fn start(allow: Allow) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    continue;
                };
                let allow = allow.clone();
                tokio::spawn(async move {
                    if let Err(e) = proxy_one(sock, allow).await {
                        tracing::debug!("egress proxy: {e}");
                    }
                });
            }
        });
        Ok(EgressProxy { addr })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn proxy_one(sock: TcpStream, allow: Allow) -> anyhow::Result<()> {
    let mut reader = BufReader::new(sock);
    let mut head = Vec::new();
    // Read the request head (bounded).
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
        if n == 0 {
            return Ok(());
        }
        head.push(line.clone());
        if line == "\r\n" || line == "\n" {
            break;
        }
        if head.len() > 100 {
            anyhow::bail!("request head too long");
        }
    }
    let first = head[0].trim_end().to_string();
    let mut parts = first.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or("HTTP/1.1").to_string(),
    );
    let refuse = |code: &str| format!("{version} {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = target.rsplit_once(':') else {
            reader.get_mut().write_all(refuse("400 Bad Request").as_bytes()).await?;
            return Ok(());
        };
        let port: u16 = port.parse()?;
        let addrs = match resolve_public(host, port, &allow).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("egress proxy refused CONNECT {target}: {e}");
                reader.get_mut().write_all(refuse("403 Forbidden").as_bytes()).await?;
                return Ok(());
            }
        };
        let mut upstream = connect_any(&addrs).await?;
        reader
            .get_mut()
            .write_all(format!("{version} 200 Connection Established\r\n\r\n").as_bytes())
            .await?;
        // Bytes the client sent after the head are still in the buffer.
        let buffered = reader.buffer().to_vec();
        let mut client = reader.into_inner();
        upstream.write_all(&buffered).await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }
    // Plain HTTP in absolute form.
    let Ok(u) = url::Url::parse(&target) else {
        reader.get_mut().write_all(refuse("400 Bad Request").as_bytes()).await?;
        return Ok(());
    };
    if u.scheme() != "http" || check_url(&u, &allow).is_err() {
        reader.get_mut().write_all(refuse("403 Forbidden").as_bytes()).await?;
        return Ok(());
    }
    let host = u.host_str().unwrap_or_default().to_string();
    let port = u.port_or_known_default().unwrap_or(80);
    let addrs = match resolve_public(&host, port, &allow).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("egress proxy refused {target}: {e}");
            reader.get_mut().write_all(refuse("403 Forbidden").as_bytes()).await?;
            return Ok(());
        }
    };
    let mut upstream = connect_any(&addrs).await?;
    let path = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_string(),
    };
    let mut out = format!("{method} {path} {version}\r\n");
    for h in &head[1..] {
        let lower = h.to_ascii_lowercase();
        if lower.starts_with("proxy-") || lower.starts_with("connection:") {
            continue;
        }
        if h == "\r\n" || h == "\n" {
            out.push_str("Connection: close\r\n\r\n");
        } else {
            out.push_str(h);
        }
    }
    upstream.write_all(out.as_bytes()).await?;
    let buffered = reader.buffer().to_vec();
    upstream.write_all(&buffered).await?;
    let mut client = reader.into_inner();
    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = upstream.split();
    let up = async { tokio::io::copy(&mut cr, &mut uw).await };
    let down = async {
        let r = tokio::io::copy(&mut ur, &mut cw).await;
        let _ = cw.shutdown().await;
        r
    };
    let _ = tokio::join!(up, down);
    Ok(())
}

async fn connect_any(addrs: &[SocketAddr]) -> anyhow::Result<TcpStream> {
    let mut last = None;
    for a in addrs {
        match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(a)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = Some(e.to_string()),
            Err(_) => last = Some("timed out".into()),
        }
    }
    anyhow::bail!("connect failed: {}", last.unwrap_or_default())
}

/// Read at most `max` bytes of a response body.
pub async fn read_capped(mut res: reqwest::Response, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if out.len() + chunk.len() > max {
            anyhow::bail!("response larger than {max} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Read a whole stream into memory (tests).
#[allow(dead_code)]
async fn drain(mut s: impl AsyncReadExt + Unpin) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = s.read_to_end(&mut v).await;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        for ip in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.50",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "87.240.190.72", "2a00:bdc0::1", "::ffff:93.184.216.34"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn checks_urls() {
        let none = Allow::default();
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(check_url(&u("https://vk.com/x"), &none).is_ok());
        assert!(check_url(&u("http://192.168.1.1/x"), &none).is_err());
        assert!(check_url(&u("http://[::1]/x"), &none).is_err());
        assert!(check_url(&u("ftp://vk.com/x"), &none).is_err());
        assert!(check_url(&u("http://a:b@vk.com/x"), &none).is_err());
        let local = Allow::new(&["127.0.0.1".into()]);
        assert!(check_url(&u("http://127.0.0.1:9/x"), &local).is_ok());
    }

    #[tokio::test]
    async fn resolver_refuses_private_names() {
        let none = Allow::default();
        assert!(resolve_public("localhost", 80, &none).await.is_err());
        assert!(resolve_public("127.0.0.1", 80, &none).await.is_err());
        let local = Allow::new(&["localhost".into()]);
        assert!(resolve_public("localhost", 80, &local).await.is_ok());
    }

    #[tokio::test]
    async fn proxy_refuses_private_targets() {
        let p = EgressProxy::start(Allow::default()).await.unwrap();
        let mut s = TcpStream::connect(p.addr).await.unwrap();
        s.write_all(b"CONNECT 127.0.0.1:22 HTTP/1.1\r\nHost: 127.0.0.1:22\r\n\r\n")
            .await
            .unwrap();
        let out = String::from_utf8(drain(&mut s).await).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
        let mut s = TcpStream::connect(p.addr).await.unwrap();
        s.write_all(b"GET http://10.1.2.3/x HTTP/1.1\r\nHost: 10.1.2.3\r\n\r\n")
            .await
            .unwrap();
        let out = String::from_utf8(drain(&mut s).await).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
    }

    #[tokio::test]
    async fn proxy_tunnels_to_allowed_targets() {
        // A tiny upstream that answers one request.
        let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = up.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        });
        let p = EgressProxy::start(Allow::new(&["127.0.0.1".into()])).await.unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(p.url()).unwrap())
            .build()
            .unwrap();
        let body = client
            .get(format!("http://{up_addr}/x"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }
}
