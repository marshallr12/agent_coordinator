//! The role runner as a Windows service.
//!
//! The service starts, in this order: it validates its settings, makes its
//! state directory, reads back the preshutdown time limit its installer
//! set and logs it, creates the
//! secured pipe, opens the runner and judges every journaled launch
//! ([`crate::service::startup`]), and only then reports running. Nothing
//! before the pipe exists is served, and a failure at any step stops the
//! service; there is no mode without the access list or the identity check.
//!
//! One thread does everything after that. It waits for a client on the
//! pipe, serves the admitted client one request line at a time, and between
//! waits steps the service's ending ([`crate::service::drain`]). A stop or
//! preshutdown request, which the control handler only records, therefore
//! reaches the launches within one [`TICK`]: new `prepare`, `hold_file` and
//! `start` requests are refused at once, launches are asked to stop, what
//! remains is terminated at the deadline, and the service reports stopped
//! only after every launch it held is journaled as ended. No request may
//! hold the thread longer than that either: a wait a client asks for is cut
//! to [`MAX_WAIT_MS`] whatever the service is doing ([`admitted`]), and the
//! client asks again.
use super::pipe::{Accepted, Line, Pipe, Session};
use super::runner::{Request, Runner, answer};
use super::scm::{self, Host};
use super::{reader, sddl, token};
use crate::service::drain::Drain;
use crate::service::lifecycle::{Clock, Lifecycle, Monotonic, Phase, Report, Signal};
use crate::service::settings::ServiceSettings;
use crate::service::startup;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the worker looks at the service's life while it waits.
pub const TICK: Duration = Duration::from_millis(250);
/// The longest a wait a client asks for may hold the serving thread. The
/// service has one thread, so a longer wait would keep a stop recorded
/// meanwhile from being acted on. The client sees the launch still running
/// and asks again.
pub const MAX_WAIT_MS: u64 = 250;
/// How long Windows may wait for the service to report that it started.
const START_HINT: Duration = Duration::from_secs(30);

/// Runs the runner as the service `settings.name`. From a console this
/// fails at once saying it must be started by the service manager.
pub fn run(settings: ServiceSettings) -> Result<()> {
    let name = settings.name.clone();
    scm::dispatch(&name, move |host| serve(&settings, host))
}

/// The service's whole life; it returns whether it stopped with no launch
/// left unconfirmed.
fn serve(settings: &ServiceSettings, host: &Host) -> Result<bool> {
    host.starting(1, START_HINT)?;
    settings.validate()?;
    let runner_sid = token::current_user_sid()?;
    ensure_state_directory(&settings.state_dir, &runner_sid)?;
    let mut log = Log::open(&settings.state_dir);
    match open(settings, host, &runner_sid, &mut log) {
        Ok((pipe, runner)) => Ok(Hub::new(settings, host, runner, log).work(pipe)),
        Err(error) => {
            log.line(&format!("the service could not start: {error:#}"));
            Err(error)
        }
    }
}

/// What the service needs before it may report running: the pipe and the
/// runner, with the journal judged.
fn open(
    settings: &ServiceSettings,
    host: &Host,
    runner_sid: &str,
    log: &mut Log,
) -> Result<(Pipe, Runner)> {
    log.line(&preshutdown_line(settings));
    host.starting(2, START_HINT)?;
    let wake = Some(host.wake());
    let pipe = Pipe::create(&settings.pipe_name, runner_sid, &settings.client_sid, wake)
        .context("the pipe is not served")?;
    let mut runner = Runner::new(&settings.state_dir)?;
    let settled = startup::settle(&mut runner)?;
    for line in settled.lines() {
        log.line(&line);
    }
    Ok((pipe, runner))
}

/// What the service logs about its preshutdown time limit. The installer
/// sets the limit; the service's account may not change its own service's
/// configuration, so the service only reads the limit back and says
/// whether it is short of `preshutdown_timeout_seconds`. Nothing here stops
/// the service.
fn preshutdown_line(settings: &ServiceSettings) -> String {
    let wanted = Duration::from_secs(settings.preshutdown_timeout_seconds);
    match scm::preshutdown_timeout(&settings.name) {
        Ok(limit) if limit >= wanted => format!("preshutdown time limit is {limit:?}"),
        Ok(limit) => format!(
            "preshutdown time limit is {limit:?}, less than preshutdown_timeout_seconds \
             ({wanted:?}); the installer must set it"
        ),
        Err(error) => format!("preshutdown time limit could not be read: {error:#}"),
    }
}

/// Makes the state directory with an access list for the runner's account,
/// the system and administrators only. A directory that already exists is
/// used as found: its access list is its installer's.
fn ensure_state_directory(directory: &Path, owner: &str) -> Result<()> {
    if directory.exists() {
        return Ok(());
    }
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    reader::create_directory(directory, &sddl::state_directory(owner)?)
}

/// The service's log: standard error, which a service has nowhere to show,
/// and `service.log` in the state directory when it can be opened.
struct Log(Option<std::fs::File>);

impl Log {
    fn open(state: &Path) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(state.join("service.log"));
        Self(file.ok())
    }

    fn line(&mut self, text: &str) {
        let line = format!("{} {text}", chrono::Utc::now().to_rfc3339());
        eprintln!("agentc-supervisor windows-service: {line}");
        if let Some(file) = self.0.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// What the one serving thread works with once the service is running.
struct Hub<'a> {
    host: &'a Host,
    life: Lifecycle<Monotonic>,
    drain: Drain,
    runner: Runner,
    log: Log,
    interval: Duration,
    last_report: Option<Instant>,
}

impl<'a> Hub<'a> {
    fn new(settings: &ServiceSettings, host: &'a Host, runner: Runner, log: Log) -> Self {
        let limits = settings.limits();
        Self {
            host,
            life: Lifecycle::new(Monotonic::new(), limits),
            drain: Drain::new(),
            runner,
            log,
            interval: limits.status_interval,
            last_report: None,
        }
    }

    /// Serves until the service has stopped; true when it stopped with
    /// nothing unconfirmed.
    fn work(mut self, mut pipe: Pipe) -> bool {
        if let Err(error) = self.host.report(&Report::Running) {
            self.log
                .line(&format!("could not report running: {error:#}"));
        }
        self.log.line("running");
        while !self.tick() {
            match pipe.accept(TICK) {
                Ok(Accepted::Idle) => {}
                Ok(Accepted::Refused(refusal)) => {
                    self.log.line(&format!("a client was refused: {refusal}"));
                }
                Ok(Accepted::Client(mut session)) => {
                    let (account, process) = {
                        let client = session.client();
                        (client.account.clone(), client.process)
                    };
                    self.log
                        .line(&format!("serving {account} (process {process})"));
                    self.serve_client(&mut session);
                    self.log.line("the client is gone");
                }
                Err(error) => {
                    self.log.line(&format!("the pipe failed: {error:#}"));
                    std::thread::sleep(TICK);
                }
            }
        }
        let quiescent = matches!(self.life.phase(), Phase::Stopped { quiescent: true });
        self.log.line(if quiescent {
            "stopped: every launch is confirmed ended"
        } else {
            "stopped with launches whose end Windows did not confirm"
        });
        quiescent
    }

    /// Steps the service's life: takes in a recorded request, moves the
    /// ending along and reports to Windows. True once the service stopped.
    fn tick(&mut self) -> bool {
        self.host.reset_wake();
        let signal = self.host.signal();
        let step = self.drain.step(&mut self.life, signal, &mut self.runner);
        for problem in &step.problems {
            self.log.line(problem);
        }
        match step.report {
            Report::Running => {}
            Report::Stopped { .. } => return true,
            Report::Stopping { .. } => {
                let due = self
                    .last_report
                    .is_none_or(|at| at.elapsed() >= self.interval);
                if due {
                    self.last_report = Some(Instant::now());
                    if let Err(error) = self.host.report(&step.report) {
                        self.log
                            .line(&format!("could not report progress: {error:#}"));
                    }
                }
            }
        }
        false
    }

    /// Serves one admitted client until it leaves or the service stops.
    fn serve_client(&mut self, session: &mut Session<'_>) {
        loop {
            if self.tick() {
                return;
            }
            match session.read_line(TICK) {
                Ok(Line::Idle) => {}
                Ok(Line::Closed) => return,
                Ok(Line::Request(bytes)) => {
                    let mut reply = self.respond(&bytes).to_string();
                    reply.push('\n');
                    if let Err(error) = session.write_all(reply.as_bytes()) {
                        self.log
                            .line(&format!("the answer was not delivered: {error:#}"));
                        return;
                    }
                }
                Err(error) => {
                    self.log.line(&format!("the client was dropped: {error:#}"));
                    return;
                }
            }
        }
    }

    /// The answer to one request line.
    fn respond(&mut self, line: &[u8]) -> Value {
        let request = admitted(&mut self.life, self.host.signal(), line);
        answer(request.and_then(|request| self.runner.handle(request)))
    }
}

/// The request in `line` as the service will carry it out, or why it will
/// not. A request that begins a launch is refused once the service is
/// ending. Every wait is cut to [`MAX_WAIT_MS`], running or ending, so the
/// serving thread is back at the service's life within one tick.
pub fn admitted<C: Clock>(
    life: &mut Lifecycle<C>,
    signal: &Signal,
    line: &[u8],
) -> Result<Request> {
    let parsed = serde_json::from_slice::<Request>(line);
    let mut request = parsed.context("the request is not one the runner knows")?;
    life.admit(signal, request.kind())?;
    request.limit_wait(MAX_WAIT_MS);
    Ok(request)
}
