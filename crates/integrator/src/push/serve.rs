//! The helper's connection loop. It serves the connections on its socket
//! strictly one at a time until shutdown. A connection whose preamble (see
//! `preamble.rs`) is worth it, and that fits the mint budget (see
//! `budget.rs`), gets its own installation token, minted before the push
//! protocol runs and revoked after it whatever the outcome; Git reaches the
//! token only through the askpass mode (see `askpass.rs`).
use crate::askpass;
use crate::budget::MintBudget;
use crate::preamble::{Preamble, Replayed};
use crate::process;
use crate::socket::BoundSocket;
use agentc_integrator::github::GithubApp;
use anyhow::{Context, Result};
use coordinator_local::candidate_push::{
    HelperSpec, PushRefusal, PushReply, RefusalCode, serve_one,
};
use serde_json::{Value, json};
use std::future::Future;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one socket read or write may stall before the connection is
/// dropped. The client bundles its candidate after connecting and before it
/// sends the request, so the first read waits out that bundling; a peer that
/// stalls longer holds up only this launch's next push.
pub const IO_TIMEOUT: Duration = Duration::from_secs(600);
/// The refusal sent when no token could be minted for a connection.
const NO_CREDENTIALS: &str = "helper could not obtain push credentials";
/// The refusal sent when the mint budget is spent.
const RATE_LIMITED: &str = "push rate limit reached; retry later";

/// The refusal for a client outside this helper's launch.
const FOREIGN_LAUNCH: &str = "connection is not from this helper's launch";

/// Mints and revokes the per-connection token Git pushes with.
pub trait Credentials {
    /// A fresh token for one connection.
    async fn mint(&self) -> Result<String>;
    /// Ends `token` early, once its connection is served.
    async fn revoke(&self, token: &str) -> Result<()>;
}

/// Installation tokens from the push App, each narrowed to one repository
/// with only `contents: write`.
pub struct GithubCredentials {
    app: GithubApp,
    scope: Value,
}

impl GithubCredentials {
    /// Credentials from `app` scoped to the repository named `repository`
    /// (the name only, as GitHub's `repositories` list takes it).
    pub fn new(app: GithubApp, repository: &str) -> Self {
        Self {
            app,
            scope: token_scope(repository),
        }
    }
}

/// The `access_tokens` request body that narrows a token to `repository` and
/// `contents: write`.
pub fn token_scope(repository: &str) -> Value {
    json!({"repositories": [repository], "permissions": {"contents": "write"}})
}

impl Credentials for GithubCredentials {
    /// Mints a scoped installation token.
    async fn mint(&self) -> Result<String> {
        self.app.scoped_token(&self.scope).await
    }

    /// Revokes the token through GitHub.
    async fn revoke(&self, token: &str) -> Result<()> {
        self.app.revoke_token(token).await
    }
}

/// One helper: its socket, the spec every push uses and its bundle limit,
/// its credentials and mint budget, the askpass program Git runs, and the
/// per-operation socket timeout.
pub struct Server<C> {
    pub socket: BoundSocket,
    pub spec: HelperSpec,
    pub max_bundle_bytes: u64,
    pub credentials: C,
    pub budget: Mutex<MintBudget>,
    pub askpass: PathBuf,
    pub timeout: Duration,
    /// The process whose descendants alone may connect: the `launch-root`
    /// that spawned this helper and its launch.
    pub launch: u32,
}

impl<C: Credentials> Server<C> {
    /// Serves connections one at a time until `shutdown` completes; a
    /// shutdown requested mid-connection takes effect once it is served. The
    /// socket is removed when the server is dropped on return.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let listener = self.socket.listener.try_clone()?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(listener)?;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => return Ok(()),
                accepted = listener.accept() => self.handle(accepted?.0).await,
            }
        }
    }

    /// Serves one connection and logs one line for it.
    async fn handle(&self, stream: tokio::net::UnixStream) {
        let from_launch = self.is_launch_client(&stream);
        let outcome = match blocking(stream, self.timeout) {
            Ok(stream) if from_launch => self.serve_connection(stream).await,
            Ok(stream) => refuse(stream, FOREIGN_LAUNCH).await,
            Err(error) => Err(error),
        };
        eprintln!("{}", outcome_line(self.spec.reference(), &outcome));
    }

    /// Whether the connecting process belongs to this helper's launch. A
    /// peer whose process id or ancestry cannot be read is refused.
    fn is_launch_client(&self, stream: &tokio::net::UnixStream) -> bool {
        let peer = stream
            .peer_cred()
            .ok()
            .and_then(|credentials| credentials.pid());
        let peer = peer.and_then(|pid| u32::try_from(pid).ok());
        peer.is_some_and(|pid| process::descends_from(pid, self.launch).unwrap_or(false))
    }

    /// Reads the preamble first. One not worth a mint is served without a
    /// token (`serve_one` refuses it before contacting the remote); one
    /// beyond the mint budget is refused; the rest are served with a token.
    async fn serve_connection(&self, mut stream: UnixStream) -> Result<PushReply> {
        let (preamble, stream) = tokio::task::spawn_blocking(move || {
            let preamble = Preamble::read(&mut stream);
            (preamble, stream)
        })
        .await
        .context("the preamble task panicked")?;
        if !preamble.worth_minting(self.max_bundle_bytes) {
            return self.serve_protocol(preamble.replay(stream), "").await;
        }
        if !self
            .budget
            .lock()
            .expect("budget lock")
            .take(Instant::now())
        {
            return refuse(stream, RATE_LIMITED).await;
        }
        self.serve_with_token(preamble.replay(stream)).await
    }

    /// Mints a token, runs the protocol with it and revokes it, whatever the
    /// reply. When no token can be minted the client gets a refusal.
    async fn serve_with_token(&self, stream: Replayed<UnixStream>) -> Result<PushReply> {
        let token = match self.credentials.mint().await {
            Ok(token) => token,
            Err(error) => {
                eprintln!("agentc-push: could not mint a token: {error:#}");
                return refuse(stream, NO_CREDENTIALS).await;
            }
        };
        let outcome = self.serve_protocol(stream, &token).await;
        if let Err(error) = self.credentials.revoke(&token).await {
            eprintln!("agentc-push: could not revoke the connection's token: {error:#}");
        }
        outcome
    }

    /// Runs [`serve_one`] on a blocking thread with Git's environment
    /// carrying `token` (empty for a connection without a mint, which
    /// askpass refuses).
    async fn serve_protocol(
        &self,
        mut stream: Replayed<UnixStream>,
        token: &str,
    ) -> Result<PushReply> {
        let environment = askpass::git_environment(&self.askpass, token)?;
        let spec = self.spec.clone().with_git_environment(environment);
        tokio::task::spawn_blocking(move || serve_one(&mut stream, &spec))
            .await
            .context("the push task panicked")?
    }
}

/// Converts an accepted stream to a blocking one with `timeout` on every
/// read and write.
fn blocking(stream: tokio::net::UnixStream, timeout: Duration) -> Result<UnixStream> {
    let stream = stream.into_std()?;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

/// Sends an `internal` refusal with `message`, reading nothing more.
async fn refuse<W: Write + Send + 'static>(mut stream: W, message: &str) -> Result<PushReply> {
    let reply = PushReply::Refused(PushRefusal {
        code: RefusalCode::Internal,
        message: message.into(),
    });
    let line = reply.to_line();
    tokio::task::spawn_blocking(move || stream.write_all(&line))
        .await
        .context("the reply task panicked")?
        .context("send the refusal")?;
    Ok(reply)
}

/// The log line for one connection to the helper for `reference`: the
/// outcome code, and for an accepted push the commit and the one it
/// replaced. Holds no token, Git output or error chain from Git.
pub fn outcome_line(reference: &str, outcome: &Result<PushReply>) -> String {
    let detail = match outcome {
        Ok(PushReply::Accepted(receipt)) => format!(
            "outcome=accepted revision={} previous={}",
            receipt.revision,
            receipt.previous.as_deref().unwrap_or("none")
        ),
        Ok(PushReply::Refused(refusal)) => format!("outcome={}", code_name(refusal.code)),
        Err(error) => format!("outcome=connection_failed error={error:#}"),
    };
    format!("agentc-push: request reference={reference} {detail}")
}

/// A refusal code's wire name, such as `lease_conflict`.
fn code_name(code: RefusalCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests;
