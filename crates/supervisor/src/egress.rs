//! Egress allowlist proxy (autonomy plan §2.3, "root-owned egress firewall
//! allowlist"). The host firewall lets agent uids reach only this loopback
//! proxy; the proxy tunnels HTTPS (`CONNECT host:443`) to allowlisted hosts
//! and refuses everything else. Decisions are logged as JSON lines on stderr.
use anyhow::{Context, Result};
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

const MAX_HEAD: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Handles one client: read the head, decide, then tunnel or refuse.
async fn handle(mut client: TcpStream, allow: &[String]) -> Result<()> {
    let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await??;
    let target = connect_target(&head);
    let decision = target.as_deref().is_some_and(|host| allowed(host, allow));
    log(target.as_deref().unwrap_or("-"), decision);
    let Some(host) = target.filter(|_| decision) else {
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    };
    let mut upstream = TcpStream::connect((host.as_str(), 443)).await?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
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

    #[tokio::test]
    async fn refused_hosts_get_403_without_upstream_contact() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            handle(client, &list()).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }
}
