//! The threads that serve a reviewer launch's candidate pipe (phase 5,
//! step 5d).
//!
//! Each reviewer launch gets the pipe [`pipe_name`] with [`INSTANCES`]
//! instances, each served by its own thread, because a background call of
//! the harness may run beside a foreground one. An instance admits only a
//! token of the runner's account that carries the reviewer's package
//! ([`Pipe::package_instances`]), reads one [`Request`] line, runs its
//! command in the candidate and writes one [`Reply`] line. A client that is
//! refused is written nothing. A call that finds every instance taken waits
//! on the client's side ([`super::candidate_shell`]).
//!
//! The threads end with the launch: [`Server::stop`] wakes them, ends a
//! command that is still running and joins them, so nothing keeps the
//! candidate after its launch.
use super::candidate::Candidate;
use super::pipe::{Accepted, Line, Pipe, Session};
use super::sys::{Handle, check};
use crate::candidate::shim::{Answer, Reply, Request};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

/// How many commands of one reviewer launch run at once.
pub const INSTANCES: u32 = 4;
/// How long one wait for a client lasts before the thread looks whether it
/// was asked to stop.
const TICK: Duration = Duration::from_millis(500);
/// How long an admitted client may take to send its whole request.
const REQUEST_PATIENCE: Duration = Duration::from_secs(10);
/// What a client is told when its request is not a [`Request`].
const NOT_A_REQUEST: &str = "the request is not one the candidate pipe knows";

/// The name of `launch`'s candidate pipe.
pub fn pipe_name(launch: &str) -> String {
    format!("agentc-candidate-{launch}")
}

/// What the serving threads share to learn that the launch is ending.
struct Stop {
    /// A manual-reset event, so every thread's wait ends once it is set.
    event: Handle,
    asked: AtomicBool,
}

// SAFETY: an event handle is a process-wide value; setting it and waiting
// on it are thread-safe.
unsafe impl Sync for Stop {}

impl Stop {
    /// An event nobody has set.
    fn new() -> Result<Self> {
        // SAFETY: no attributes and no name; a manual-reset event, not set.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        Ok(Self {
            event: Handle::owned(event, "CreateEventW")?,
            asked: AtomicBool::new(false),
        })
    }

    /// Asks every thread to end and wakes the ones that wait.
    fn ask(&self) -> Result<()> {
        self.asked.store(true, Ordering::SeqCst);
        // SAFETY: the event handle is live.
        check(unsafe { SetEvent(self.event.raw()) }, "SetEvent")
    }

    /// Whether the threads were asked to end.
    fn asked(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }
}

/// One launch's candidate pipe being served.
pub struct Server {
    stop: Arc<Stop>,
    candidate: Arc<Candidate>,
    threads: Vec<JoinHandle<()>>,
}

impl Server {
    /// Creates `launch`'s candidate pipe for the account `runner` and the
    /// reviewer's `package` and starts a thread per instance. A pipe that
    /// cannot be made as specified is not made, and no thread starts.
    pub fn start(
        launch: &str,
        runner: &str,
        package: &str,
        candidate: Arc<Candidate>,
    ) -> Result<Self> {
        let stop = Arc::new(Stop::new()?);
        let wake = Some(stop.event.raw());
        let name = pipe_name(launch);
        let pipes = Pipe::package_instances(&name, runner, package, INSTANCES, wake)
            .with_context(|| format!("create the candidate pipe {name}"))?;
        let serving = |pipe| {
            let (stop, candidate) = (stop.clone(), candidate.clone());
            std::thread::spawn(move || serve(pipe, &candidate, &stop))
        };
        let threads = pipes.into_iter().map(serving).collect();
        Ok(Self {
            stop,
            candidate,
            threads,
        })
    }

    /// Ends the threads: a running command is ended, the candidate refuses
    /// further ones and every instance of the pipe closes. Returns once all
    /// threads ended, so their hold on the candidate is gone.
    pub fn stop(self) {
        self.candidate.stop();
        if let Err(error) = self.stop.ask() {
            eprintln!("agentc-supervisor: candidate pipe: {error:#}");
        }
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// Serves one instance until the launch ends. A client that is refused, or
/// an exchange that fails, is reported and never ends the thread.
fn serve(mut pipe: Pipe, candidate: &Candidate, stop: &Stop) {
    while !stop.asked() {
        let served = match pipe.accept(TICK) {
            Ok(Accepted::Client(session)) => exchange(session, candidate),
            Ok(Accepted::Refused(refusal)) => Err(refusal.into()),
            Ok(Accepted::Idle) => Ok(()),
            Err(error) => {
                std::thread::sleep(TICK);
                Err(error)
            }
        };
        if let Err(error) = served {
            eprintln!("agentc-supervisor: candidate pipe: {error:#}");
        }
    }
}

/// One request line in, one reply line out; the session's end disconnects
/// the client.
fn exchange(mut session: Session<'_>, candidate: &Candidate) -> Result<()> {
    let Line::Request(bytes) = session.read_line(REQUEST_PATIENCE)? else {
        bail!("an admitted client sent no whole request");
    };
    let mut line = serde_json::to_vec(&reply(&bytes, candidate))?;
    line.push(b'\n');
    session.write_all(&line)
}

/// Runs the command `bytes` asks for and says how it ended, or why
/// nothing ran.
fn reply(bytes: &[u8], candidate: &Candidate) -> Reply {
    let Ok(request) = serde_json::from_slice::<Request>(bytes) else {
        return Reply::Failed(NOT_A_REQUEST.into());
    };
    match candidate.run(&request.command) {
        Ok(call) => Reply::Ran(Answer {
            status: call.status,
            output: hex::encode(call.output),
            truncated: call.truncated,
            note: call.note,
        }),
        Err(error) => Reply::Failed(format!("{error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launchs_pipe_is_named_after_it() {
        assert_eq!(pipe_name("rev-1"), "agentc-candidate-rev-1");
    }

    #[test]
    fn stopping_wakes_a_waiting_thread_at_once() {
        let stop = Arc::new(Stop::new().unwrap());
        let me = super::super::token::current_user_sid().unwrap();
        let name = format!("agentc-test-stop-{}", uuid::Uuid::new_v4().simple());
        let package = "S-1-15-2-1-2-3-4-5-6-7";
        let wake = Some(stop.event.raw());
        let mut pipe = Pipe::for_package(&name, &me, package, wake).unwrap();
        let started = std::time::Instant::now();
        stop.ask().unwrap();
        let waited = pipe.accept(Duration::from_secs(30)).unwrap();
        assert!(matches!(waited, Accepted::Idle));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(stop.asked());
    }
}
