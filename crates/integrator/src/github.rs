//! GitHub App authentication and the few REST calls this crate's binaries
//! make. The App signs an RS256 JWT with its private key, trades it for a
//! one-hour installation token and keeps that token in memory only. Git
//! reaches the token through an askpass helper (`askpass.rs` for the
//! integrator, `push/askpass.rs` for `agentc-push`), never through a URL or a
//! file.
use crate::config::GithubConfig;
use anyhow::{Context, Result, anyhow, ensure};
use aws_lc_rs::{rand, signature as sig};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Refresh the installation token this long before GitHub expires it.
const TOKEN_MARGIN: Duration = Duration::from_secs(600);
/// Installation tokens live one hour.
const TOKEN_LIFETIME: Duration = Duration::from_secs(3600);
const USER_AGENT: &str = "agentc-integrator";
/// How many times an idempotent GET is sent when GitHub answers 5xx.
const GET_ATTEMPTS: u32 = 2;
/// How long to wait before repeating a GET that got a 5xx reply.
const GET_RETRY_WAIT: Duration = Duration::from_secs(2);

/// `owner/name` of a GitHub repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    pub owner: String,
    pub name: String,
}

impl RepoId {
    /// Parses `https://github.com/<owner>/<name>(.git)`; other URLs have no id.
    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("https://github.com/")?;
        let rest = rest.trim_end_matches('/').trim_end_matches(".git");
        let (owner, name) = rest.split_once('/')?;
        let valid = |s: &str| !s.is_empty() && !s.contains('/');
        (valid(owner) && valid(name)).then(|| Self {
            owner: owner.into(),
            name: name.into(),
        })
    }
}

/// An authenticated GitHub App installation.
pub struct GithubApp {
    config: GithubConfig,
    http: reqwest::Client,
    retry_wait: Duration,
    token: Mutex<Option<(String, Instant)>>,
}

impl GithubApp {
    /// Builds the App client; the key is read on every token refresh.
    pub fn new(config: &GithubConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            config: config.clone(),
            http,
            retry_wait: GET_RETRY_WAIT,
            token: Mutex::new(None),
        })
    }

    /// A valid installation token, minted when the cached one is near expiry.
    pub async fn installation_token(&self) -> Result<String> {
        if let Some(token) = self.cached_token() {
            return Ok(token);
        }
        let token = self.mint_token().await?;
        let expires = Instant::now() + TOKEN_LIFETIME - TOKEN_MARGIN;
        *self.token.lock().expect("token lock") = Some((token.clone(), expires));
        Ok(token)
    }

    /// The cached token if it is still comfortably valid.
    fn cached_token(&self) -> Option<String> {
        let guard = self.token.lock().expect("token lock");
        guard
            .as_ref()
            .filter(|(_, expires)| Instant::now() < *expires)
            .map(|(token, _)| token.clone())
    }

    /// Trades a fresh App JWT for an installation token.
    async fn mint_token(&self) -> Result<String> {
        let jwt = self.app_jwt()?;
        let reply = self
            .send(self.http.post(self.access_tokens_url()), &jwt)
            .await?;
        token_from(&reply)
    }

    /// Trades a fresh App JWT for a new installation token narrowed by
    /// `scope`, an `access_tokens` request body (for example `repositories`
    /// and `permissions`). The token is not cached: the caller owns it and
    /// ends it with [`GithubApp::revoke_token`].
    pub async fn scoped_token(&self, scope: &Value) -> Result<String> {
        let jwt = self.app_jwt()?;
        let request = self.http.post(self.access_tokens_url()).json(scope);
        token_from(&self.send(request, &jwt).await?)
    }

    /// Revokes `token` before it expires (`DELETE /installation/token`,
    /// authenticated by the token itself).
    pub async fn revoke_token(&self, token: &str) -> Result<()> {
        let url = format!("{}/installation/token", self.config.api_base);
        self.send(self.http.delete(url), token).await.map(drop)
    }

    /// Signs an App JWT with the private key, read fresh from disk.
    fn app_jwt(&self) -> Result<String> {
        let pem = std::fs::read(&self.config.private_key)
            .with_context(|| format!("read {}", self.config.private_key.display()))?;
        sign_app_jwt(&pem, self.config.app_id, unix_now())
    }

    /// The installation's `access_tokens` endpoint.
    fn access_tokens_url(&self) -> String {
        format!(
            "{}/app/installations/{}/access_tokens",
            self.config.api_base, self.config.installation_id
        )
    }

    /// GETs `path` (relative to the API base) with the installation token. A
    /// 5xx reply is repeated after a short wait, up to [`GET_ATTEMPTS`]
    /// sends in all: a GET changes nothing, so repeating it is safe.
    pub async fn get(&self, path: &str) -> Result<Value> {
        let token = self.installation_token().await?;
        let url = format!("{}{path}", self.config.api_base);
        let mut attempt = 1;
        loop {
            match self
                .send_once(self.http.get(&url), &token, "GET", path)
                .await
            {
                Err(Failure::Server(_)) if attempt < GET_ATTEMPTS => {
                    attempt += 1;
                    tokio::time::sleep(self.retry_wait).await;
                }
                other => return other.map_err(Failure::into_error),
            }
        }
    }

    /// POSTs an empty body to `path` (relative to the API base) with the
    /// installation token.
    pub async fn post(&self, path: &str) -> Result<Value> {
        let token = self.installation_token().await?;
        let url = format!("{}{path}", self.config.api_base);
        self.send(self.http.post(url), &token).await
    }

    /// Sends one request once; non-2xx is an error. `path` names the request
    /// in the error and is never sent anywhere.
    async fn send(&self, request: reqwest::RequestBuilder, bearer: &str) -> Result<Value> {
        let (method, path) = request
            .try_clone()
            .and_then(|copy| copy.build().ok())
            .map(|built| (built.method().to_string(), built.url().path().to_owned()))
            .unwrap_or_default();
        self.send_once(request, bearer, &method, &path)
            .await
            .map_err(Failure::into_error)
    }

    /// Sends one request with GitHub's headers. Errors name `method` and
    /// `path` (relative to the API base, without a query string) and never
    /// the host or a credential.
    async fn send_once(
        &self,
        request: reqwest::RequestBuilder,
        bearer: &str,
        method: &str,
        path: &str,
    ) -> Result<Value, Failure> {
        let path = path.split(['?', '#']).next().unwrap_or_default();
        let response = request
            .bearer_auth(bearer)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| {
                Failure::Other(anyhow!(
                    "GitHub request {method} {path} failed: {}",
                    error.without_url()
                ))
            })?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        let error = anyhow!(
            "GitHub replied {status} to {method} {path}: {}",
            body["message"].as_str().unwrap_or("")
        );
        Err(if status.is_server_error() {
            Failure::Server(error)
        } else {
            Failure::Other(error)
        })
    }
}

/// Why one send failed; only a 5xx reply is worth repeating.
enum Failure {
    Server(anyhow::Error),
    Other(anyhow::Error),
}

impl Failure {
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Server(error) | Self::Other(error) => error,
        }
    }
}

/// The `token` field of an `access_tokens` reply.
fn token_from(reply: &Value) -> Result<String> {
    reply["token"]
        .as_str()
        .map(str::to_owned)
        .context("installation token missing from GitHub reply")
}

/// Seconds since the Unix epoch.
fn unix_now() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(now.as_secs()).unwrap_or(i64::MAX)
}

/// Signs a GitHub App JWT (RS256). `iat` is backdated a minute for clock
/// skew; GitHub rejects lifetimes over ten minutes, so `exp` is +9 minutes.
pub fn sign_app_jwt(pem: &[u8], app_id: u64, now: i64) -> Result<String> {
    ensure!(app_id != 0, "GitHub App id is not configured");
    let key = rsa_key(pem)?;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = json!({"iat": now - 60, "exp": now + 540, "iss": app_id.to_string()});
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
    let input = format!("{header}.{payload}");
    let mut signature = vec![0u8; key.public_modulus_len()];
    key.sign(
        &sig::RSA_PKCS1_SHA256,
        &rand::SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| anyhow!("RSA signing failed"))?;
    Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// Parses a PKCS#1 ("RSA PRIVATE KEY", GitHub's format) or PKCS#8 PEM key.
fn rsa_key(pem: &[u8]) -> Result<sig::RsaKeyPair> {
    if let Ok(der) = PrivatePkcs1KeyDer::from_pem_slice(pem) {
        return sig::RsaKeyPair::from_der(der.secret_pkcs1_der())
            .map_err(|error| anyhow!("invalid RSA key: {error}"));
    }
    let der = PrivatePkcs8KeyDer::from_pem_slice(pem)
        .map_err(|_| anyhow!("no RSA private key in PEM"))?;
    sig::RsaKeyPair::from_pkcs8(der.secret_pkcs8_der())
        .map_err(|error| anyhow!("invalid RSA key: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::encoding::AsDer;
    use aws_lc_rs::rsa::{KeyPair, KeySize};
    use aws_lc_rs::signature::KeyPair as _;
    use base64::engine::general_purpose::STANDARD;
    use std::sync::Arc;

    /// A fresh PKCS#8 PEM key and its public key bytes.
    fn test_key() -> (Vec<u8>, Vec<u8>) {
        let pair = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let der = pair.as_der().unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(der.as_ref())
        );
        (pem.into_bytes(), pair.public_key().as_ref().to_vec())
    }

    #[test]
    fn jwt_verifies_and_carries_app_claims() {
        let (pem, public) = test_key();
        let jwt = sign_app_jwt(&pem, 42, 1_000_000).unwrap();
        let (input, signature) = jwt.rsplit_once('.').unwrap();
        let key = sig::UnparsedPublicKey::new(&sig::RSA_PKCS1_2048_8192_SHA256, public);
        key.verify(
            input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .unwrap();
        let claims = input.split('.').nth(1).unwrap();
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!(
            claims,
            json!({"iat": 999_940, "exp": 1_000_540, "iss": "42"})
        );
    }

    #[test]
    fn unconfigured_app_and_bad_keys_are_rejected() {
        let (pem, _) = test_key();
        assert!(sign_app_jwt(&pem, 0, 0).is_err());
        assert!(sign_app_jwt(b"not a key", 1, 0).is_err());
    }

    /// Requests the mock GitHub saw, as `METHOD path?query`, and how many
    /// 502 replies each route still owes.
    #[derive(Default)]
    struct Mock {
        seen: Vec<String>,
        flaky_failures_left: u32,
    }

    type Shared = Arc<Mutex<Mock>>;

    /// A mock GitHub on a loopback port: `/flaky` fails `flaky_failures_left`
    /// times with 502 and then succeeds, `/down` always answers 502,
    /// `/missing` answers 404, and minting a token always answers 502 except
    /// for `/app/installations/9/access_tokens` on `ok_mint`.
    async fn app_with_mock(
        flaky_failures: u32,
        ok_mint: bool,
    ) -> (GithubApp, Shared, tempfile::TempDir) {
        use axum::extract::{Request, State};
        use axum::http::StatusCode;
        use axum::{Json, Router};
        let mock: Shared = Arc::new(Mutex::new(Mock {
            seen: Vec::new(),
            flaky_failures_left: flaky_failures,
        }));
        async fn handle(
            State((mock, ok_mint)): State<(Shared, bool)>,
            request: Request,
        ) -> (StatusCode, Json<Value>) {
            let target = request
                .uri()
                .path_and_query()
                .map(ToString::to_string)
                .unwrap_or_default();
            let mut mock = mock.lock().unwrap();
            mock.seen.push(format!("{} {target}", request.method()));
            let bad_gateway = (
                StatusCode::BAD_GATEWAY,
                Json(json!({"message": "Server Error"})),
            );
            match request.uri().path() {
                "/flaky" if mock.flaky_failures_left > 0 => {
                    mock.flaky_failures_left -= 1;
                    bad_gateway
                }
                "/flaky" => (StatusCode::OK, Json(json!({"ok": true}))),
                "/missing" => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))),
                "/app/installations/9/access_tokens" if ok_mint => (
                    StatusCode::CREATED,
                    Json(json!({"token": "ghs_secret_token"})),
                ),
                _ => bad_gateway,
            }
        }
        let router = Router::new()
            .fallback(handle)
            .with_state((mock.clone(), ok_mint));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let directory = tempfile::tempdir().unwrap();
        let (pem, _) = test_key();
        let private_key = directory.path().join("app.pem");
        std::fs::write(&private_key, pem).unwrap();
        let mut app = GithubApp::new(&GithubConfig {
            api_base,
            app_id: 7,
            installation_id: 9,
            private_key,
        })
        .unwrap();
        app.retry_wait = Duration::from_millis(1);
        (app, mock, directory)
    }

    fn seen(mock: &Shared) -> Vec<String> {
        mock.lock().unwrap().seen.clone()
    }

    #[tokio::test]
    async fn a_get_that_gets_a_502_is_repeated_and_can_succeed() {
        let (app, mock, _key) = app_with_mock(1, true).await;
        let reply = app.get("/flaky?filter=all").await.unwrap();
        assert_eq!(reply, json!({"ok": true}));
        let gets: Vec<String> = seen(&mock)
            .into_iter()
            .filter(|line| line.starts_with("GET"))
            .collect();
        assert_eq!(gets, ["GET /flaky?filter=all", "GET /flaky?filter=all"]);
    }

    #[tokio::test]
    async fn a_get_that_keeps_getting_502_fails_after_a_bounded_number_of_sends() {
        let (app, mock, _key) = app_with_mock(0, true).await;
        let error = app.get("/down?filter=all").await.unwrap_err();
        let gets = seen(&mock)
            .into_iter()
            .filter(|line| line.starts_with("GET"))
            .count();
        assert_eq!(gets, usize::try_from(GET_ATTEMPTS).unwrap());
        assert_eq!(
            format!("{error:#}"),
            "GitHub replied 502 Bad Gateway to GET /down: Server Error"
        );
    }

    #[tokio::test]
    async fn a_get_that_gets_a_client_error_is_not_repeated() {
        let (app, mock, _key) = app_with_mock(0, true).await;
        let error = app.get("/missing").await.unwrap_err();
        assert!(format!("{error:#}").contains("404 Not Found to GET /missing"));
        let gets = seen(&mock).iter().filter(|l| l.starts_with("GET")).count();
        assert_eq!(gets, 1);
    }

    #[tokio::test]
    async fn posts_are_not_repeated_after_a_502() {
        let (app, mock, _key) = app_with_mock(0, true).await;
        let error = app.post("/down").await.unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "GitHub replied 502 Bad Gateway to POST /down: Server Error"
        );
        let (app, mock_mint, _key) = app_with_mock(0, false).await;
        let error = app.installation_token().await.unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "GitHub replied 502 Bad Gateway to POST /app/installations/9/access_tokens: Server Error"
        );
        assert_eq!(seen(&mock_mint).len(), 1, "minting is sent once");
        let posts = seen(&mock)
            .iter()
            .filter(|l| l.starts_with("POST /down"))
            .count();
        assert_eq!(posts, 1);
    }

    #[tokio::test]
    async fn errors_name_the_path_without_query_host_or_credential() {
        let (app, _mock, _key) = app_with_mock(0, true).await;
        let error = app
            .get("/repos/o/r/actions/runs/1/jobs?filter=all&per_page=100")
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("GET /repos/o/r/actions/runs/1/jobs:"),
            "{message}"
        );
        for leaked in [
            "filter",
            "per_page",
            "127.0.0.1",
            "ghs_secret_token",
            "Bearer",
        ] {
            assert!(!message.contains(leaked), "{leaked} leaked: {message}");
        }
    }

    #[test]
    fn repo_ids_come_only_from_github_https_urls() {
        let id = RepoId::from_url("https://github.com/o/r.git").unwrap();
        assert_eq!((id.owner.as_str(), id.name.as_str()), ("o", "r"));
        assert!(RepoId::from_url("/tmp/remote.git").is_none());
        assert!(RepoId::from_url("https://github.com/o").is_none());
    }
}
