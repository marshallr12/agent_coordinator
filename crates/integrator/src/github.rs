//! GitHub App authentication and the few REST calls the integrator makes.
//! The App signs an RS256 JWT with its private key, trades it for a one-hour
//! installation token and keeps that token in memory only. Git reaches the
//! token through the `askpass` helper (see `askpass.rs`), never through a URL
//! or a file.
use crate::config::GithubConfig;
use anyhow::{Context, Result, anyhow, bail, ensure};
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
        let pem = std::fs::read(&self.config.private_key)
            .with_context(|| format!("read {}", self.config.private_key.display()))?;
        let jwt = sign_app_jwt(&pem, self.config.app_id, unix_now())?;
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.config.api_base, self.config.installation_id
        );
        let reply = self.send(self.http.post(url), &jwt).await?;
        reply["token"]
            .as_str()
            .map(str::to_owned)
            .context("installation token missing from GitHub reply")
    }

    /// GETs `path` (relative to the API base) with the installation token.
    pub async fn get(&self, path: &str) -> Result<Value> {
        let token = self.installation_token().await?;
        let url = format!("{}{path}", self.config.api_base);
        self.send(self.http.get(url), &token).await
    }

    /// POSTs an empty body to `path` (relative to the API base) with the
    /// installation token.
    pub async fn post(&self, path: &str) -> Result<Value> {
        let token = self.installation_token().await?;
        let url = format!("{}{path}", self.config.api_base);
        self.send(self.http.post(url), &token).await
    }

    /// Sends one request with GitHub's headers; non-2xx is an error.
    async fn send(&self, request: reqwest::RequestBuilder, bearer: &str) -> Result<Value> {
        let response = request
            .bearer_auth(bearer)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| anyhow!("GitHub request failed: {}", error.without_url()))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "GitHub replied {status}: {}",
                body["message"].as_str().unwrap_or("")
            );
        }
        Ok(body)
    }
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

    #[test]
    fn repo_ids_come_only_from_github_https_urls() {
        let id = RepoId::from_url("https://github.com/o/r.git").unwrap();
        assert_eq!((id.owner.as_str(), id.name.as_str()), ("o", "r"));
        assert!(RepoId::from_url("/tmp/remote.git").is_none());
        assert!(RepoId::from_url("https://github.com/o").is_none());
    }
}
