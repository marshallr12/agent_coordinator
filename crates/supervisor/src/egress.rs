//! Egress allowlist proxy (autonomy plan §2.3, "root-owned egress firewall
//! allowlist"). The host firewall lets agent uids reach only this loopback
//! proxy; the proxy tunnels HTTPS (`CONNECT host:443`) to allowlisted hosts
//! and refuses everything else. Decisions are logged as JSON lines on stderr.
use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Hosts agents need by default: model vendors (API and subscription auth),
/// GitHub, and crates.io. A leading dot allows every subdomain.
pub const DEFAULT_ALLOW: &[&str] = &[
    "api.anthropic.com",
    ".claude.ai",
    "claude.ai",
    "console.anthropic.com",
    "platform.claude.com",
    "api.openai.com",
    "auth.openai.com",
    "chatgpt.com",
    ".chatgpt.com",
    "github.com",
    "api.github.com",
    "codeload.github.com",
    ".githubusercontent.com",
    "crates.io",
    "index.crates.io",
    "static.crates.io",
];

/// The longest request head a client may send before its CONNECT is judged.
pub const MAX_HEAD: usize = 8 * 1024;
/// How long a client has to send its whole request head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// The answer to a request that is refused; nothing is contacted upstream.
pub const FORBIDDEN: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
/// The answer once the tunnel to an allowed destination is open.
pub const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

/// Where the request head in `bytes` ends (just past its blank line), if
/// it is complete.
pub fn head_end(bytes: &[u8]) -> Option<usize> {
    let end = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    Some(end + 4)
}

/// The host a request head asks to tunnel to, when it is a CONNECT to port
/// 443 of a host `allow` lists; `None` for anything to refuse.
pub fn permitted_target(head: &str, allow: &[String]) -> Option<String> {
    connect_target(head).filter(|host| allowed(host, allow))
}

/// As the proxy's own resolution, blocking: `host:443`'s publicly routable
/// addresses only, so an allowlisted name cannot be pointed at loopback or
/// the local network. A name that does not resolve has none.
pub fn public_addresses_blocking(host: &str) -> Vec<SocketAddr> {
    use std::net::ToSocketAddrs;
    match (host, 443).to_socket_addrs() {
        Ok(resolved) => resolved.filter(|a| public_address(a.ip())).collect(),
        Err(_) => Vec::new(),
    }
}

/// Whether `host` matches an allowlist entry (exact, or `.suffix`).
pub fn allowed(host: &str, allow: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allow.iter().any(|entry| match entry.strip_prefix('.') {
        Some(suffix) => host.ends_with(&format!(".{suffix}")),
        None => host == *entry,
    })
}

/// Parses `CONNECT host:443 HTTP/1.x` from a request head; only port 443.
pub fn connect_target(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let (host, port) = target.rsplit_once(':')?;
    let valid_host = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    (method == "CONNECT" && version.starts_with("HTTP/1.") && port == "443" && valid_host)
        .then(|| host.to_owned())
}

/// Serves the proxy forever on `listen`.
pub async fn serve(listen: &str, allow: Vec<String>) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    let allow = std::sync::Arc::new(allow);
    loop {
        let (client, _) = listener.accept().await?;
        let allow = allow.clone();
        tokio::spawn(async move {
            let _ = handle(client, &allow).await;
        });
    }
}

/// Handles one client: read the head, decide, then tunnel or refuse. An
/// allowlisted name is tunnelled only to a resolved address that is public.
async fn handle(mut client: TcpStream, allow: &[String]) -> Result<()> {
    let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await??;
    let target = connect_target(&head);
    let addresses = match target.as_deref().filter(|host| allowed(host, allow)) {
        Some(host) => permitted_addresses(host).await,
        None => Vec::new(),
    };
    log(target.as_deref().unwrap_or("-"), !addresses.is_empty());
    if addresses.is_empty() {
        client.write_all(FORBIDDEN).await?;
        return Ok(());
    }
    let mut upstream = TcpStream::connect(&addresses[..]).await?;
    client.write_all(ESTABLISHED).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Resolves `host:443` and keeps only publicly routable addresses, so an
/// allowlisted name cannot be pointed at loopback or the local network.
async fn permitted_addresses(host: &str) -> Vec<SocketAddr> {
    match tokio::net::lookup_host((host, 443)).await {
        Ok(resolved) => resolved.filter(|a| public_address(a.ip())).collect(),
        Err(_) => Vec::new(),
    }
}

/// Whether `ip` may be tunnelled to: not loopback, private, link-local,
/// unspecified, multicast, reserved or otherwise local. An IPv6 address
/// that carries an IPv4 one (mapped, compatible or the NAT64 well-known
/// prefix) is judged by the IPv4 address it reaches.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

/// The IPv4 address an IPv6 address stands for: IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-translated (`::ffff:0:a.b.c.d`),
/// IPv4-compatible (`::a.b.c.d`), NAT64 with the well-known prefix
/// (`64:ff9b::a.b.c.d`) or 6to4 (`2002:aabb:ccdd::`).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    let v4 = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    let tail = v4(segments[6], segments[7]);
    let compatible = segments[..6] == [0; 6];
    let translated = segments[..6] == [0, 0, 0, 0, 0xffff, 0];
    let nat64 = segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    let six_to_four = (segments[0] == 0x2002).then(|| v4(segments[1], segments[2]));
    let carried = (compatible || translated || nat64).then_some(tail);
    ip.to_ipv4_mapped().or(carried).or(six_to_four)
}

/// IPv4 addresses that are not local, private, reserved or non-unicast.
fn public_v4(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    let this_network = first == 0;
    let shared = first == 100 && (second & 0xc0) == 64; // 100.64.0.0/10
    let benchmarking = first == 198 && (second & 0xfe) == 18; // 198.18.0.0/15
    let reserved = first >= 240; // 240.0.0.0/4, the broadcast address too
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || this_network
        || shared
        || benchmarking
        || reserved)
}

/// IPv6 addresses that are not local, unique-local or non-unicast, and not
/// the local-use NAT64 prefix, whose translation is the local network's.
fn public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let site_local = (segments[0] & 0xffc0) == 0xfec0; // fec0::/10
    let local_nat64 = segments[..3] == [0x64, 0xff9b, 1]; // 64:ff9b:1::/48
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || site_local
        || local_nat64)
}

/// Reads until the blank line that ends the request head.
async fn read_head(client: &mut TcpStream) -> Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < MAX_HEAD, "request head too large");
        anyhow::ensure!(client.read(&mut byte).await? == 1, "client closed early");
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// One JSON line per decision.
fn log(host: &str, allowed: bool) {
    eprintln!(
        "{}",
        serde_json::json!({"egress": host, "allowed": allowed})
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<String> {
        DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowlist_matches_exact_hosts_and_dot_suffixes_only() {
        assert!(allowed("GitHub.com", &list()));
        assert!(allowed("objects.githubusercontent.com", &list()));
        assert!(!allowed("githubusercontent.com.evil.test", &list()));
        assert!(!allowed("evilgithub.com", &list()));
    }

    #[test]
    fn only_connect_to_port_443_is_accepted() {
        assert_eq!(
            connect_target("CONNECT github.com:443 HTTP/1.1\r\n\r\n").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            connect_target("CONNECT github.com:22 HTTP/1.1\r\n\r\n"),
            None
        );
        assert_eq!(
            connect_target("GET http://github.com/ HTTP/1.1\r\n\r\n"),
            None
        );
        assert_eq!(connect_target("CONNECT a b:443 HTTP/1.1\r\n\r\n"), None);
    }

    /// Sends one CONNECT for `host` to a proxy allowing `allow`; the reply.
    async fn proxy_reply(host: &str, allow: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle(client, &allow).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        let request = format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        reply
    }

    #[test]
    fn a_head_ends_at_its_blank_line_and_only_allowed_connects_are_permitted() {
        assert_eq!(head_end(b"CONNECT a:443 HTTP/1.1\r\n\r\nrest"), Some(26));
        assert_eq!(head_end(b"CONNECT a:443 HTTP/1.1\r\n"), None);
        let permitted = |head: &str| permitted_target(head, &list());
        assert_eq!(
            permitted("CONNECT github.com:443 HTTP/1.1\r\n\r\n").as_deref(),
            Some("github.com")
        );
        assert_eq!(permitted("CONNECT example.com:443 HTTP/1.1\r\n\r\n"), None);
        assert_eq!(permitted("CONNECT github.com:80 HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn blocking_resolution_keeps_no_local_address() {
        assert!(public_addresses_blocking("localhost").is_empty());
        assert!(public_addresses_blocking("127.0.0.1").is_empty());
        assert!(public_addresses_blocking("no-such-host.invalid").is_empty());
    }

    #[tokio::test]
    async fn refused_hosts_get_403_without_upstream_contact() {
        let reply = proxy_reply("example.com", list()).await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }

    #[tokio::test]
    async fn allowlisted_name_resolving_to_loopback_gets_403() {
        let reply = proxy_reply("localhost", vec!["localhost".into()]).await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }

    #[test]
    fn local_and_non_unicast_addresses_are_not_public() {
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "100.64.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "fec0::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.1.1",
            "::127.0.0.1",
            "::10.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "198.18.0.1",
            "198.19.255.255",
            "240.0.0.1",
            "64:ff9b:1::a00:1",
            "2002:7f00:1::",
            "2002:c0a8:101::1",
            "::ffff:0:7f00:1",
        ] {
            assert!(!public_address(local.parse().unwrap()), "{local}");
        }
    }

    #[test]
    fn public_addresses_are_permitted() {
        for public in [
            "1.1.1.1",
            "140.82.112.3",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
            "198.20.0.1",
            "223.255.255.1",
            "2002:101:101::1",
        ] {
            assert!(public_address(public.parse().unwrap()), "{public}");
        }
    }
}
