//! The publication broker's push credentials (phase 6, step 6d), as the
//! Linux helper has them (`agentc-push`): an installation token of the push
//! App for each push, narrowed to the one repository with only
//! `contents: write`, minted just before the push and revoked after it
//! whatever the outcome. Git reaches the token only through the askpass
//! mode of this same binary ([`agentc_integrator::push_askpass`]): never in
//! a URL, an argument, a Git config or a file.
//!
//! A per-launch budget ([`MintBudget`]) bounds how often one launch can
//! make the broker ask GitHub for a token, so a launch looping on pushes
//! cannot spend the App's shared rate limit.
use agentc_integrator::config::GithubConfig;
use agentc_integrator::github::{GithubApp, RepoId, sign_app_jwt};
use agentc_integrator::mint_budget::{MINT_BURST, MINT_INTERVAL, MintBudget};
use agentc_integrator::push_askpass;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// Mints and revokes the token one push uses.
pub trait Credentials: Send + Sync {
    /// A fresh token for one push.
    fn mint(&self) -> Result<String>;
    /// Ends `token` early, once its push is over.
    fn revoke(&self, token: &str) -> Result<()>;
}

/// The `access_tokens` request body that narrows a token to `repository`
/// (its name only) and `contents: write`, as the Linux helper asks.
pub fn token_scope(repository: &str) -> Value {
    json!({"repositories": [repository], "permissions": {"contents": "write"}})
}

/// Installation tokens of the push App for one repository.
pub struct GithubCredentials {
    app: GithubApp,
    scope: Value,
    runtime: tokio::runtime::Runtime,
}

impl GithubCredentials {
    /// Credentials from the App `config` for the GitHub repository
    /// `remote` (`https://github.com/<owner>/<name>[.git]`). The key is
    /// read and an App JWT signed here, so a key the broker cannot read or
    /// use stops it at start rather than at the first push.
    pub fn new(config: &GithubConfig, remote: &str) -> Result<Self> {
        let repository = RepoId::from_url(remote)
            .context("windows_publication.remote must be https://github.com/<owner>/<repo>.git")?;
        check_key(config)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        Ok(Self {
            app: GithubApp::new(config)?,
            scope: token_scope(&repository.name),
            runtime,
        })
    }
}

/// Reads the App's key and signs a JWT with it, without sending anything.
fn check_key(config: &GithubConfig) -> Result<()> {
    let pem = std::fs::read(&config.private_key)
        .with_context(|| format!("read the push key {}", config.private_key.display()))?;
    let now = chrono::Utc::now().timestamp();
    sign_app_jwt(&pem, config.app_id, now).context("the push key cannot sign an App JWT")?;
    Ok(())
}

impl Credentials for GithubCredentials {
    /// Mints a scoped installation token.
    fn mint(&self) -> Result<String> {
        self.runtime.block_on(self.app.scoped_token(&self.scope))
    }

    /// Revokes the token through GitHub.
    fn revoke(&self, token: &str) -> Result<()> {
        self.runtime.block_on(self.app.revoke_token(token))
    }
}

/// What the broker pushes with: the credentials, the askpass program Git
/// asks for them, and each launch's mint budget.
pub struct PushAuthority {
    credentials: Box<dyn Credentials>,
    askpass: PathBuf,
    budgets: Mutex<BTreeMap<String, MintBudget>>,
}

/// Why no token was minted for a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoToken {
    /// The launch spent its mint budget.
    RateLimited,
    /// GitHub, or the key, did not give one.
    Unavailable,
}

impl PushAuthority {
    /// Pushes with `credentials`, Git asking `askpass` (this binary) for
    /// them.
    pub fn new(credentials: Box<dyn Credentials>, askpass: &Path) -> Result<Self> {
        ensure!(
            askpass.is_absolute(),
            "the askpass program must be an absolute path"
        );
        Ok(Self {
            credentials,
            askpass: askpass.to_path_buf(),
            budgets: Mutex::default(),
        })
    }

    /// A token for one push of `launch`, within its budget.
    pub fn mint_for(&self, launch: &str) -> std::result::Result<Token<'_>, NoToken> {
        if !self.spend(launch) {
            return Err(NoToken::RateLimited);
        }
        match self.credentials.mint() {
            Ok(value) => Ok(Token {
                authority: self,
                launch: launch.to_owned(),
                value,
            }),
            Err(error) => {
                let line = json!({"publication": launch, "credentials": format!("{error:#}")});
                eprintln!("{line}");
                Err(NoToken::Unavailable)
            }
        }
    }

    /// Takes one mint from `launch`'s budget, if it has one left.
    fn spend(&self, launch: &str) -> bool {
        let mut budgets = self.budgets.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let budget = budgets
            .entry(launch.to_owned())
            .or_insert_with(|| MintBudget::new(MINT_BURST, MINT_INTERVAL, now));
        budget.take(now)
    }
}

/// One push's token, revoked when dropped.
pub struct Token<'a> {
    authority: &'a PushAuthority,
    launch: String,
    value: String,
}

impl Token<'_> {
    /// The environment of the Git children that contact the remote: the
    /// askpass program, the token beside it and Git's fixed settings.
    pub fn git_environment(&self) -> Result<Vec<(String, String)>> {
        push_askpass::git_environment(&self.authority.askpass, &self.value)
    }
}

impl Drop for Token<'_> {
    /// Revokes the token and logs that it did, or why it could not: the
    /// token then only expires on its own within the hour. The token
    /// itself is never logged.
    fn drop(&mut self) {
        let revoked = match self.authority.credentials.revoke(&self.value) {
            Ok(()) => "revoked".to_owned(),
            Err(error) => format!("not revoked: {error:#}"),
        };
        eprintln!("{}", json!({"publication": self.launch, "token": revoked}));
    }
}

#[cfg(test)]
mod tests;
