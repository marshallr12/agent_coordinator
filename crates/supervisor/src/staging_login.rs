//! Per-run staging login for verifying reviewers (decision U22, refining U18).
//!
//! Candidate code must never read the persistent verification login (an
//! operator password; see `verification`). Before a verifying reviewer
//! launch, the supervisor, outside every sandbox, signs in to the staging
//! coordinator with that login and writes only the new browser session's
//! cookie to `$RUN/verification-session.json`, which candidate commands may
//! read. When the launch ends it signs that session out; a missed sign-out is
//! bounded by the coordinator's fixed session lifetime. This speaks the Agent
//! Coordinator's own login API, so it applies to coordinator staging only.
use crate::config::Config;
use crate::profile::{LaunchSpec, run_files};
use crate::verification;
use anyhow::{Context, Result, bail, ensure};
use reqwest::header::{HeaderMap, SET_COOKIE};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Each staging request must finish within this time.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The fields of the persistent login file this module uses.
#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

/// One signed-in staging browser session, owned by a launch.
pub struct StagingSession {
    origin: Url,
    client: Client,
    cookie: String,
    csrf: String,
}

/// Signs in for a verifying reviewer launch and writes its session file;
/// `None` for launches without a verification environment.
pub fn open(spec: &LaunchSpec, config: &Config) -> Result<Option<StagingSession>> {
    let (Some(entry), Some(project)) = (
        verification::for_launch(spec, config),
        spec.project.as_deref(),
    ) else {
        return Ok(None);
    };
    let origin = origin(&entry.url)?;
    let login = read_login(&verification::credential_file(config, project))?;
    let client = client(&origin, config)?;
    let session = block_on(sign_in(origin, client, login)).context("staging sign-in")?;
    let path = spec.run.join(run_files::VERIFICATION_SESSION);
    if let Err(error) = write_session_file(&path, &session) {
        // The cookie never reached the launch; end it now rather than leave it live.
        let _ = session.close();
        return Err(error);
    }
    Ok(Some(session))
}

impl StagingSession {
    /// Signs this session out, so the handed-over cookie stops working.
    pub fn close(self) -> Result<()> {
        block_on(self.sign_out()).context("staging sign-out")
    }

    /// `POST /api/v1/auth/logout` as this browser session.
    async fn sign_out(self) -> Result<()> {
        let response = self
            .client
            .post(self.origin.join("api/v1/auth/logout")?)
            .header("Origin", origin_text(&self.origin))
            .header("Cookie", &self.cookie)
            .header("X-CSRF-Token", &self.csrf)
            .header("Idempotency-Key", uuid::Uuid::new_v4().to_string())
            .json(&json!({}))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "status {}",
            response.status()
        );
        Ok(())
    }
}

/// The site root of a verification URL; only http(s) is accepted.
fn origin(url: &str) -> Result<Url> {
    let parsed = Url::parse(url).with_context(|| format!("verification url {url:?}"))?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        "verification url {url:?} must be http(s) with a host"
    );
    Ok(Url::parse(&origin_text(&parsed))?)
}

/// `scheme://host[:port]`, the value the coordinator expects in `Origin`.
fn origin_text(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Reads the persistent login (the host owner's file, never copied).
fn read_login(path: &Path) -> Result<Login> {
    let bytes = crate::confine::read_regular(path)
        .with_context(|| format!("verification login {}", path.display()))?;
    serde_json::from_slice(&bytes).context("verification login must hold username and password")
}

/// An HTTP client that never follows redirects. Staging the relay treats as
/// loopback is reached directly at the relay's address; anything else only
/// over https through the egress proxy, so the password never travels in
/// clear text off the host.
fn client(origin: &Url, config: &Config) -> Result<Client> {
    let builder = Client::builder()
        .timeout(TIMEOUT)
        // Sign-in and sign-out run on separate runtimes; never reuse a socket.
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none());
    let host = origin.host_str().unwrap_or_default();
    let builder = match crate::relay::staging_address(origin.as_str())? {
        Some(address) => builder.no_proxy().resolve(host, address),
        None => {
            ensure!(
                origin.scheme() == "https",
                "non-loopback staging must use https"
            );
            let proxy = format!("http://{}", config.egress_listen);
            builder.proxy(reqwest::Proxy::all(proxy)?)
        }
    };
    Ok(builder.build()?)
}

/// `POST /api/v1/auth/login`; keeps the session cookie and its CSRF token.
async fn sign_in(origin: Url, client: Client, login: Login) -> Result<StagingSession> {
    let response = client
        .post(origin.join("api/v1/auth/login")?)
        .header("Origin", origin_text(&origin))
        .json(&json!({"username": login.username, "password": login.password}))
        .send()
        .await?;
    let status = response.status();
    ensure!(status.is_success(), "status {status}");
    let cookie = session_cookie(response.headers())?;
    let csrf = csrf_token(response.json().await?)?;
    Ok(StagingSession {
        origin,
        client,
        cookie,
        csrf,
    })
}

/// The CSRF token in a login response body (`data.csrf_token`).
fn csrf_token(body: Value) -> Result<String> {
    match body["data"]["csrf_token"].as_str() {
        Some(token) if !token.is_empty() => Ok(token.to_owned()),
        _ => bail!("login response has no csrf_token"),
    }
}

/// The single `name=value` pair the login response sets.
fn session_cookie(headers: &HeaderMap) -> Result<String> {
    let mut values = headers.get_all(SET_COOKIE).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        bail!("login response must set exactly one cookie");
    };
    let pair = value.to_str()?.split(';').next().unwrap_or_default().trim();
    let valid = pair
        .split_once('=')
        .is_some_and(|(name, value)| !name.is_empty() && !value.is_empty());
    ensure!(valid, "login response set a malformed cookie");
    Ok(pair.to_owned())
}

/// The session file candidate code reads: the origin and the cookie only.
fn session_json(session: &StagingSession) -> Value {
    let (name, value) = session.cookie.split_once('=').unwrap_or_default();
    json!({
        "url": origin_text(&session.origin),
        "cookie": {"name": name, "value": value},
    })
}

/// Writes the session file once; an existing file or link is refused.
fn write_session_file(path: &Path, session: &StagingSession) -> Result<()> {
    crate::confine::path_without_symlinks(path, true)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    let text = serde_json::to_string_pretty(&session_json(session))?;
    Ok(file.write_all(text.as_bytes())?)
}

/// Runs one staging request on a private single-threaded runtime.
fn block_on<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(future)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::profile::{Harness, Role};
    use crate::verification::Verification;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// One request the fake staging coordinator received.
    struct Request {
        line: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Request {
        /// The value of header `name` (case-insensitive), if sent.
        fn header(&self, name: &str) -> Option<&str> {
            let found = self
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name));
            found.map(|(_, value)| value.as_str())
        }
    }

    /// Reads one HTTP/1.1 request with a Content-Length body.
    fn read_request(stream: &std::net::TcpStream) -> Request {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let headers = read_headers(&mut reader);
        let mut request = Request {
            line: line.trim_end().into(),
            headers,
            body: String::new(),
        };
        let length = request.header("content-length");
        let mut body = vec![0; length.map_or(0, |v| v.parse().unwrap())];
        reader.read_exact(&mut body).unwrap();
        request.body = String::from_utf8(body).unwrap();
        request
    }

    /// Reads header lines up to the blank line ending them.
    fn read_headers(reader: &mut impl BufRead) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            match header.trim_end().split_once(": ") {
                Some((n, v)) => headers.push((n.to_owned(), v.to_owned())),
                None => return headers,
            }
        }
    }

    /// A fake staging coordinator answering each request with `status`,
    /// `extra` headers and `body`; every request is forwarded to the receiver.
    fn staging(
        status: &'static str,
        extra: &'static str,
        body: &'static str,
    ) -> (u16, mpsc::Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                sender.send(read_request(&stream)).unwrap();
                let reply = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        (port, receiver)
    }

    /// A verifying reviewer launch of project "p1" against loopback `port`,
    /// with the persistent login installed.
    fn fixture(root: &Path, port: u16) -> (LaunchSpec, Config) {
        fixture_at(root, &format!("http://127.0.0.1:{port}/ui"))
    }

    /// As `fixture`, verifying against `url`.
    fn fixture_at(root: &Path, url: &str) -> (LaunchSpec, Config) {
        let mut config = Config {
            state_dir: root.join("roles"),
            egress_listen: "127.0.0.1:9".into(),
            ..Config::default()
        };
        let entry = Verification {
            url: url.into(),
            browser: false,
        };
        config.verification.insert("p1".into(), entry);
        let login = verification::credential_file(&config, "p1");
        std::fs::create_dir_all(login.parent().unwrap()).unwrap();
        std::fs::write(&login, r#"{"username":"verifier","password":"pw"}"#).unwrap();
        (reviewer(root), config)
    }

    /// A reviewer launch spec of project "p1" with its run directory.
    fn reviewer(root: &Path) -> LaunchSpec {
        let spec = LaunchSpec {
            role: Role::Reviewer,
            harness: Harness::Claude,
            clone: root.join("clone"),
            run: root.join("run"),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: Some("p1".into()),
            task: None,
            push_socket: None,
        };
        std::fs::create_dir_all(&spec.run).unwrap();
        spec
    }

    const SIGNED_IN: &str = r#"{"data":{"actor":{},"csrf_token":"csrf-1"}}"#;
    const COOKIE: &str = "Set-Cookie: coordinator_local=s3cret; Path=/; HttpOnly\r\n";

    #[test]
    fn a_verifying_launch_hands_over_only_a_session_and_signs_it_out() {
        let root = tempfile::tempdir().unwrap();
        let (port, requests) = staging("200 OK", COOKIE, SIGNED_IN);
        let (spec, config) = fixture(root.path(), port);
        let session = open(&spec, &config).unwrap().unwrap();
        assert_sign_in(&requests.recv().unwrap(), port);
        assert_session_file(&spec.run.join(run_files::VERIFICATION_SESSION), port);
        session.close().unwrap();
        assert_sign_out(&requests.recv().unwrap());
    }

    /// The login request: right route, Origin and credentials.
    fn assert_sign_in(login: &Request, port: u16) {
        let origin = format!("http://127.0.0.1:{port}");
        assert_eq!(login.line, "POST /api/v1/auth/login HTTP/1.1");
        assert_eq!(login.header("origin"), Some(origin.as_str()));
        assert_eq!(login.body, r#"{"password":"pw","username":"verifier"}"#);
    }

    /// The private session file holds only the origin and the cookie.
    fn assert_session_file(path: &Path, port: u16) {
        use std::os::unix::fs::PermissionsExt;
        let written: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let expected = json!({"url": format!("http://127.0.0.1:{port}"),
                              "cookie": {"name": "coordinator_local", "value": "s3cret"}});
        assert_eq!(written, expected);
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The logout request: the session's cookie, CSRF token and a fresh key.
    fn assert_sign_out(logout: &Request) {
        assert_eq!(logout.line, "POST /api/v1/auth/logout HTTP/1.1");
        assert_eq!(logout.header("cookie"), Some("coordinator_local=s3cret"));
        assert_eq!(logout.header("x-csrf-token"), Some("csrf-1"));
        let key = logout.header("idempotency-key");
        assert!(key.is_some_and(|k| !k.is_empty()));
        assert_eq!(logout.body, "{}");
    }

    #[test]
    fn launches_without_verification_get_no_session() {
        let root = tempfile::tempdir().unwrap();
        let (spec, config) = fixture(root.path(), 9);
        let implementer = LaunchSpec {
            role: Role::Implementer,
            ..spec.clone()
        };
        let other = LaunchSpec {
            project: Some("p2".into()),
            task: None,
            push_socket: None,
            ..spec
        };
        for spec in [implementer, other] {
            assert!(open(&spec, &config).unwrap().is_none());
            assert!(!spec.run.join(run_files::VERIFICATION_SESSION).exists());
        }
    }

    #[test]
    fn a_refused_or_malformed_sign_in_writes_no_session() {
        for (status, extra) in [
            ("401 Unauthorized", COOKIE),
            ("200 OK", ""),
            ("200 OK", "Set-Cookie: a=1\r\nSet-Cookie: b=2\r\n"),
            ("200 OK", "Set-Cookie: novalue\r\n"),
            ("302 Found", "Location: http://example.invalid/\r\n"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (port, _requests) = staging(status, extra, SIGNED_IN);
            let (spec, config) = fixture(root.path(), port);
            assert!(open(&spec, &config).is_err(), "{status} {extra}");
            assert!(!spec.run.join(run_files::VERIFICATION_SESSION).exists());
        }
    }

    #[test]
    fn an_existing_session_file_or_link_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let (port, requests) = staging("200 OK", COOKIE, SIGNED_IN);
        let (spec, config) = fixture(root.path(), port);
        let path = spec.run.join(run_files::VERIFICATION_SESSION);
        std::os::unix::fs::symlink(root.path().join("elsewhere"), &path).unwrap();
        assert!(open(&spec, &config).is_err());
        assert!(!root.path().join("elsewhere").exists());
        let lines: Vec<_> = requests.try_iter().map(|r| r.line).collect();
        assert_eq!(lines[1], "POST /api/v1/auth/logout HTTP/1.1", "{lines:?}");
    }

    #[test]
    fn only_http_origins_with_a_host_are_accepted() {
        assert_eq!(
            origin("https://staging.example/ui/").unwrap().as_str(),
            "https://staging.example/"
        );
        for url in ["file:///etc/passwd", "ftp://host/", "not a url"] {
            assert!(origin(url).is_err(), "{url}");
        }
    }

    #[test]
    fn dot_localhost_staging_is_reached_directly_like_the_relay() {
        let root = tempfile::tempdir().unwrap();
        let (port, requests) = staging("200 OK", COOKIE, SIGNED_IN);
        let url = format!("http://staging.localhost:{port}/ui");
        let (spec, config) = fixture_at(root.path(), &url);
        let session = open(&spec, &config).unwrap().unwrap();
        let login = requests.recv().unwrap();
        let origin = format!("http://staging.localhost:{port}");
        assert_eq!(login.header("origin"), Some(origin.as_str()));
        session.close().unwrap();
    }

    #[test]
    fn off_host_staging_must_use_https() {
        let root = tempfile::tempdir().unwrap();
        let (spec, config) = fixture_at(root.path(), "http://staging.example/");
        let error = open(&spec, &config).err().unwrap();
        assert!(format!("{error:#}").contains("must use https"), "{error:#}");
    }
}
