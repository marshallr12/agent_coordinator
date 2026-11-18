//! The publication broker's handler (phase 6, step 6c): what an
//! implementer launch's session on its publication pipe
//! ([`super::broker`]) may publish.
//!
//! The session speaks the candidate push protocol of the Linux helper
//! ([`coordinator_local::candidate_push`]): one request line naming a
//! commit and its tree, then a bundle. The request never names a ref, a
//! remote or a task: the broker publishes only to the launch's own
//! candidate ref, `refs/agent-coordinator/candidates/<task>/<launch>` (or
//! its create-only WIP refs), on the one remote it is configured with,
//! with the task and launch taken from its grant. A launch's commit must
//! carry the tree it names, and one launch publishes one candidate commit.
//!
//! Who may publish is decided by the grant and the supervisor, never by
//! the request:
//!
//! - a reviewer launch, or one whose grant names no task, is refused before
//!   anything is read;
//! - the supervisor is asked ([`Gatekeeper`]) once the request line is read
//!   and before any bundle byte is, and again just before the remote is
//!   written; a launch without current coordinator authority, and any
//!   question that gets no answer, is refused with `not_authorized`;
//! - a second session of a launch while one of its pushes is under way is
//!   refused, since one launch's pushes share one private repository.
//!
//! A launch whose pipe the broker closed (its launch ended) cannot open it
//! again, so nothing is replayed after a launch ends. Replies are fixed
//! text: the supervisor's reason goes to the broker's log, not the launch.
//! After the reply the broker reads and discards what the client still
//! sends until it leaves or falls quiet, because disconnecting a pipe
//! discards what the client has not read.
use super::authority_pipe::AuthorityRoute;
use super::broker::{Grant, Halt, Handler, Role};
use super::pipe::{Received, Session};
use super::publication_credentials::{NoToken, PushAuthority, Token};
use crate::service::authority::Verdict;
use anyhow::Result;
use coordinator_local::candidate_push::{
    HelperSpec, PushRefusal, PushReply, RefusalCode, serve_one_gated,
};
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The publication broker's service name, which its launch pipes carry.
pub const SERVICE: &str = "publication";
/// How long a read of a session may wait for the client's next bytes once
/// it has started sending, as the Linux helper's I/O timeout. The first
/// bytes cannot wait this long: the pipe drops a client silent for ten
/// seconds after it connects, so the CLI bundles before it connects.
const IDLE_PATIENCE: Duration = Duration::from_secs(600);
/// How long one wait for the client lasts before the broker looks whether
/// the launch is closing.
const TICK: Duration = Duration::from_millis(250);
/// How long the client may stay quiet after the reply before the broker
/// disconnects it.
const LINGER: Duration = Duration::from_secs(5);
/// The most bytes one read of a session takes.
const CHUNK: usize = 64 * 1024;
/// The refusal while another push of the launch is under way.
const BUSY: &str = "another push of this launch is under way";
/// The refusal when no helper spec can be made for the launch.
const UNPUBLISHABLE: &str = "the broker cannot publish this launch";
/// The refusals when no token was minted, as the Linux helper words them.
const NO_CREDENTIALS: &str = "helper could not obtain push credentials";
const RATE_LIMITED: &str = "push rate limit reached; retry later";

/// Who decides whether a launch may publish now.
pub trait Gatekeeper: Send + Sync {
    /// The verdict on `launch` working on `task`.
    fn ask(&self, launch: &str, task: &str) -> Verdict;
}

impl Gatekeeper for AuthorityRoute {
    /// The supervisor's verdict, asked over its authority pipe.
    fn ask(&self, launch: &str, task: &str) -> Verdict {
        AuthorityRoute::ask(self, launch, task)
    }
}

/// The publication broker's [`Handler`].
pub struct Publication {
    remote: String,
    work_root: PathBuf,
    gatekeeper: Arc<dyn Gatekeeper>,
    git_environment: Vec<(String, String)>,
    /// The push credentials, when the broker has them.
    push: Option<Arc<PushAuthority>>,
    /// The launches with a push under way.
    busy: Mutex<BTreeSet<String>>,
}

impl Publication {
    /// A broker publishing to `remote`, keeping each launch's private
    /// repository below `work_root` (an absolute directory only the broker
    /// may write), and asking `gatekeeper` before each push.
    pub fn new(remote: &str, work_root: PathBuf, gatekeeper: Arc<dyn Gatekeeper>) -> Self {
        Self {
            remote: remote.to_owned(),
            work_root,
            gatekeeper,
            git_environment: Vec::new(),
            push: None,
            busy: Mutex::default(),
        }
    }

    /// Sets environment variables for the Git children that contact the
    /// remote when the broker pushes without credentials of its own.
    pub fn with_git_environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.git_environment = environment;
        self
    }

    /// Pushes with a token from `push` for each push, minted once the
    /// supervisor says the launch may publish and revoked after the push.
    pub fn with_push(mut self, push: Arc<PushAuthority>) -> Self {
        self.push = Some(push);
        self
    }

    /// Serves one push for `grant`'s launch working on `task`, unless
    /// another of its pushes is under way. Its token, if any, is revoked
    /// when this returns.
    fn publish(
        &self,
        grant: &Grant,
        task: &str,
        session: &mut Session<'_>,
        halt: &Halt,
    ) -> Result<PushReply> {
        let Some(_turn) = Turn::take(&self.busy, grant.launch()) else {
            return reply_with(session, refusal(RefusalCode::Internal, BUSY));
        };
        let token = match self.token_for(grant.launch(), task) {
            Ok(token) => token,
            Err(refused) => return reply_with(session, refused),
        };
        let Ok(spec) = self.spec(grant, task, token.as_ref()) else {
            return reply_with(session, refusal(RefusalCode::Internal, UNPUBLISHABLE));
        };
        let gate = || self.gate(grant.launch(), task);
        let mut stream = Stream::new(session, halt);
        serve_one_gated(&mut stream, &spec, &gate)
    }

    /// The token for one push of `launch`, minted only after the supervisor
    /// said it may publish and within its budget; none when the broker
    /// pushes without credentials.
    fn token_for(
        &self,
        launch: &str,
        task: &str,
    ) -> std::result::Result<Option<Token<'_>>, PushRefusal> {
        let Some(push) = &self.push else {
            return Ok(None);
        };
        self.gate(launch, task)?;
        match push.mint_for(launch) {
            Ok(token) => Ok(Some(token)),
            Err(NoToken::RateLimited) => Err(refusal(RefusalCode::Internal, RATE_LIMITED)),
            Err(NoToken::Unavailable) => Err(refusal(RefusalCode::Internal, NO_CREDENTIALS)),
        }
    }

    /// The helper spec of `grant`'s launch: its own ref and repository, the
    /// digests of its credentials for the secret scan to refuse, and Git's
    /// environment carrying `token` when there is one.
    fn spec(&self, grant: &Grant, task: &str, token: Option<&Token<'_>>) -> Result<HelperSpec> {
        let work_dir = self.work_root.join(grant.launch());
        let spec = HelperSpec::new(&self.remote, task, grant.launch(), &work_dir)?;
        let spec = spec.with_known_digests(grant.known_digests().to_vec());
        let environment = match token {
            Some(token) => token.git_environment()?,
            None => self.git_environment.clone(),
        };
        Ok(spec.with_git_environment(environment))
    }

    /// The supervisor's verdict as the push protocol's gate; the reason of
    /// a refusal is logged, and the launch is told fixed text.
    fn gate(&self, launch: &str, task: &str) -> std::result::Result<(), PushRefusal> {
        match self.gatekeeper.ask(launch, task) {
            Verdict::Current => Ok(()),
            Verdict::Refused(reason) => {
                let line = serde_json::json!({"publication": launch, "authority": reason});
                eprintln!("{line}");
                Err(refusal(
                    RefusalCode::NotAuthorized,
                    "the coordinator does not authorize this launch to publish now",
                ))
            }
        }
    }

    /// The task `grant`'s launch publishes for, or why it may not publish
    /// at all: only an implementer launch with a task may.
    fn permitted(grant: &Grant) -> std::result::Result<&str, PushRefusal> {
        let why = match (grant.role(), grant.task()) {
            (Role::Implementer, Some(task)) => return Ok(task),
            (Role::Reviewer, _) => "a reviewer launch may not publish",
            (Role::Implementer, None) => "the launch names no task to publish for",
        };
        Err(refusal(RefusalCode::NotAuthorized, why))
    }
}

impl Handler for Publication {
    /// Refuses a launch that may not publish before reading anything,
    /// serves one push otherwise, logs the outcome and lets the client
    /// read the reply before it is disconnected.
    fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> Result<()> {
        let outcome = match Self::permitted(grant) {
            Ok(task) => self.publish(grant, task, session, halt),
            Err(refused) => reply_with(session, refused),
        };
        log(grant, &outcome);
        outcome?;
        drain(session, halt)
    }
}

/// A launch's turn to push, given back when dropped.
struct Turn<'a> {
    busy: &'a Mutex<BTreeSet<String>>,
    launch: String,
}

impl<'a> Turn<'a> {
    /// The turn of `launch`, unless one of its pushes is under way.
    fn take(busy: &'a Mutex<BTreeSet<String>>, launch: &str) -> Option<Self> {
        let mut held = busy.lock().unwrap_or_else(PoisonError::into_inner);
        held.insert(launch.to_owned()).then(|| Self {
            busy,
            launch: launch.to_owned(),
        })
    }
}

impl Drop for Turn<'_> {
    /// Gives the launch's turn back.
    fn drop(&mut self) {
        let mut held = (self.busy.lock()).unwrap_or_else(PoisonError::into_inner);
        held.remove(&self.launch);
    }
}

/// A refusal with fixed text.
fn refusal(code: RefusalCode, message: &str) -> PushRefusal {
    PushRefusal {
        code,
        message: message.to_owned(),
    }
}

/// Writes `refused` as the session's one reply line.
fn reply_with(session: &mut Session<'_>, refused: PushRefusal) -> Result<PushReply> {
    let reply = PushReply::Refused(refused);
    session.write_all(&reply.to_line())?;
    Ok(reply)
}

/// One JSON line per session, naming the launch, role and task of the
/// grant and what came of it; never Git output.
fn log(grant: &Grant, outcome: &Result<PushReply>) {
    let (result, reference, revision) = match outcome {
        Ok(PushReply::Accepted(receipt)) => (
            "accepted".to_owned(),
            Some(receipt.reference.clone()),
            Some(receipt.revision.clone()),
        ),
        Ok(PushReply::Refused(refused)) => {
            let code = serde_json::to_value(refused.code).unwrap_or_default();
            (code.as_str().unwrap_or("unknown").to_owned(), None, None)
        }
        Err(_) => ("connection_failed".to_owned(), None, None),
    };
    let line = serde_json::json!({"publication": grant.launch(), "role": grant.role(),
        "task": grant.task(), "outcome": result, "reference": reference, "revision": revision});
    eprintln!("{line}");
}

/// Reads and discards what the client still sends, until it leaves, stays
/// quiet for [`LINGER`] or the launch closes, so it can read the reply.
fn drain(session: &mut Session<'_>, halt: &Halt) -> Result<()> {
    let mut quiet_since = Instant::now();
    while !halt.asked() {
        match session.read(CHUNK, TICK)? {
            Received::Closed => break,
            Received::Data(_) => quiet_since = Instant::now(),
            Received::Idle if quiet_since.elapsed() >= LINGER => break,
            Received::Idle => {}
        }
    }
    Ok(())
}

/// A session as the byte stream the push protocol reads and writes. A read
/// waits for the client at most [`IDLE_PATIENCE`] and ends at once when the
/// launch closes.
struct Stream<'s, 'p> {
    session: &'s mut Session<'p>,
    halt: &'s Halt,
}

impl<'s, 'p> Stream<'s, 'p> {
    /// The stream of `session`, whose launch's closing `halt` tells.
    fn new(session: &'s mut Session<'p>, halt: &'s Halt) -> Self {
        Self { session, halt }
    }
}

impl Read for Stream<'_, '_> {
    /// The client's next bytes, at most `buffer`'s length; nothing once it
    /// left; an error when the launch closes or the client stays silent.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let started = Instant::now();
        loop {
            if self.halt.asked() {
                return Err(io::Error::other("the launch is closing"));
            }
            match self
                .session
                .read(buffer.len(), TICK)
                .map_err(io::Error::other)?
            {
                Received::Data(bytes) => {
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    return Ok(bytes.len());
                }
                Received::Closed => return Ok(0),
                Received::Idle if started.elapsed() >= IDLE_PATIENCE => {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                Received::Idle => {}
            }
        }
    }
}

impl Write for Stream<'_, '_> {
    /// Writes all of `buffer` to the client.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.session.write_all(buffer).map_err(io::Error::other)?;
        Ok(buffer.len())
    }

    /// Nothing is buffered.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
