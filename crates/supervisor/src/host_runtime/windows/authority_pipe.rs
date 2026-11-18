//! The supervisor's authority pipe (phase 6, step 6c): how the publication
//! broker asks, before each push, whether a launch still has coordinator
//! authority.
//!
//! The supervisor's account owns the pipe ([`Pipe::control`]) and only the
//! publication broker's account is served; the broker refuses a pipe any
//! other account owns ([`super::pipe_client`]). One request line names the
//! launch (from the broker's grant, never from a launch's request) and the
//! task its grant carries; the answer is the supervisor's [`Verdict`],
//! judged afresh from the coordinator ([`Authority::judge`]).
//!
//! Every way an answer can fail to arrive is a refusal: a supervisor that
//! is not running, a pipe another account owns, a malformed answer and one
//! that does not come within [`CONTROL_PATIENCE`]. Nothing is published on
//! a guess.
use super::egress_admission::{CONTROL_PATIENCE, within};
use super::pipe::{Accepted, Line, Pipe, Session};
use crate::service::authority::{Authority, Verdict};
use crate::service::pipe_policy::{checked_sid, pipe_path};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::HANDLE;

/// The authority pipe's name below `\\.\pipe\` unless configured otherwise.
pub const DEFAULT_PIPE: &str = "agentc-supervisor-authority";
/// How long the server waits for the next line of a connected broker
/// before it looks whether it should stop.
const LINE_PATIENCE: Duration = Duration::from_millis(500);

/// One question on the authority pipe.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Question {
    /// May `launch`, which works on `task`, publish now?
    Authority { launch: String, task: String },
}

/// The supervisor's side of the authority pipe.
pub struct AuthorityServer {
    pipe: Pipe,
    authority: Arc<Authority>,
}

impl AuthorityServer {
    /// Serves the pipe `name`, owned by the supervisor's account
    /// `supervisor`, to the publication broker's account `publication`
    /// (both SID text), answering from `authority`; `wake` interrupts
    /// waits.
    pub fn new(
        name: &str,
        supervisor: &str,
        publication: &str,
        authority: Arc<Authority>,
        wake: Option<HANDLE>,
    ) -> Result<Self> {
        let pipe = Pipe::control(name, supervisor, publication, wake)
            .with_context(|| format!("create the authority pipe {name}"))?;
        Ok(Self { pipe, authority })
    }

    /// Waits up to `patience` for the broker and answers its questions
    /// until it leaves or `stop` says to. A refused client is reported and
    /// nothing is written to it.
    pub fn serve_one(&mut self, patience: Duration, stop: &dyn Fn() -> bool) -> Result<()> {
        match self.pipe.accept(patience)? {
            Accepted::Client(mut session) => answer_all(&self.authority, &mut session, stop),
            Accepted::Refused(refusal) => {
                eprintln!("agentc-supervisor: authority pipe: {refusal}");
                Ok(())
            }
            Accepted::Idle => Ok(()),
        }
    }
}

/// Answers one broker's question lines until it leaves or `stop` says to.
fn answer_all(
    authority: &Authority,
    session: &mut Session<'_>,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    while !stop() {
        let line = match session.read_line(LINE_PATIENCE)? {
            Line::Request(line) => line,
            Line::Idle => continue,
            Line::Closed => break,
        };
        let mut answer = respond(authority, &line).to_string();
        answer.push('\n');
        session.write_all(answer.as_bytes())?;
    }
    Ok(())
}

/// The answer to one question line: `ok` with `current` and, for a
/// refusal, its `reason`; or `ok` false for a line that is no question.
fn respond(authority: &Authority, line: &[u8]) -> Value {
    let question: Question = match serde_json::from_slice(line) {
        Ok(question) => question,
        Err(error) => return json!({"ok": false, "error": format!("not a question: {error}")}),
    };
    let Question::Authority { launch, task } = question;
    let verdict = authority.judge(&launch, &task);
    log(&launch, &task, &verdict);
    match verdict {
        Verdict::Current => json!({"ok": true, "current": true}),
        Verdict::Refused(reason) => json!({"ok": true, "current": false, "reason": reason}),
    }
}

/// One JSON line per judgement.
fn log(launch: &str, task: &str, verdict: &Verdict) {
    let current = *verdict == Verdict::Current;
    let line = json!({"authority": launch, "task": task, "current": current});
    eprintln!("{line}");
}

/// The publication broker's way to the supervisor's authority pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityRoute {
    pipe: String,
    supervisor_sid: String,
}

impl AuthorityRoute {
    /// The supervisor serving the pipe `pipe` as the account
    /// `supervisor_sid` (SID text).
    pub fn new(pipe: &str, supervisor_sid: &str) -> Result<Self> {
        pipe_path(pipe)?;
        Ok(Self {
            pipe: pipe.to_owned(),
            supervisor_sid: checked_sid(supervisor_sid)?.to_owned(),
        })
    }

    /// The supervisor's verdict on `launch` working on `task`. A question
    /// that gets no answer within [`CONTROL_PATIENCE`], or a malformed one,
    /// is a refusal saying so.
    pub fn ask(&self, launch: &str, task: &str) -> Verdict {
        let question = json!({"op": "authority", "launch": launch, "task": task});
        let (pipe, owner) = (self.pipe.clone(), self.supervisor_sid.clone());
        let answer = within("the supervisor", pipe, owner, move |mut client| {
            client.ask(&question)
        });
        match answer.and_then(|answer| verdict_of(&answer)) {
            Ok(verdict) => verdict,
            Err(error) => Verdict::Refused(format!("the supervisor could not be asked: {error:#}")),
        }
    }
}

/// The verdict an answer line carries.
fn verdict_of(answer: &Value) -> Result<Verdict> {
    if answer["ok"] != json!(true) {
        let why = answer["error"].as_str().unwrap_or("no reason given");
        bail!("the supervisor refused the question: {why}");
    }
    match answer["current"].as_bool() {
        Some(true) => Ok(Verdict::Current),
        Some(false) => {
            let reason = answer["reason"].as_str().unwrap_or("no reason given");
            Ok(Verdict::Refused(reason.to_owned()))
        }
        None => bail!("the supervisor's answer says nothing of authority"),
    }
}

/// The bound every question to the supervisor is held to.
pub const PATIENCE: Duration = CONTROL_PATIENCE;

#[cfg(test)]
mod tests {
    use super::super::token;
    use super::*;
    use crate::service::authority::{ACTIVE, AttemptReader, AttemptView, Held};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    /// A coordinator whose one attempt is in the state the test sets.
    struct Switch(Mutex<&'static str>);

    impl AttemptReader for Arc<Switch> {
        fn read(&self, _task: &str, _attempt: &str) -> Result<Option<AttemptView>> {
            let state = (*self.0.lock().unwrap()).to_owned();
            Ok(Some(AttemptView {
                state,
                generation: 1,
            }))
        }
    }

    /// This process's account.
    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// A pipe name no other test or run uses.
    fn unique() -> String {
        format!("agentc-test-authority-{}", uuid::Uuid::new_v4().simple())
    }

    /// An authority holding launch `l` of task `t`, and its coordinator.
    fn authority() -> (Arc<Authority>, Arc<Switch>) {
        let switch = Arc::new(Switch(Mutex::new(ACTIVE)));
        let authority = Arc::new(Authority::new(Box::new(switch.clone())));
        let held = Held {
            task: "t".into(),
            attempt: "a".into(),
            generation: 1,
        };
        authority.hold("l", held);
        (authority, switch)
    }

    /// Serves the authority pipe `name` on a thread until `done` is set.
    fn serving(
        name: &str,
        authority: Arc<Authority>,
        done: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        let mut server = AuthorityServer::new(name, &me(), &me(), authority, None).unwrap();
        std::thread::spawn(move || {
            let stop = || done.load(Ordering::SeqCst);
            while !stop() {
                server.serve_one(Duration::from_millis(100), &stop).unwrap();
            }
        })
    }

    /// Whether `verdict` refuses for a reason containing `why`.
    fn refused_for(verdict: &Verdict, why: &str) -> bool {
        matches!(verdict, Verdict::Refused(reason) if reason.contains(why))
    }

    #[test]
    fn the_broker_hears_the_supervisors_verdict_asked_afresh_each_time() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let (authority, switch) = authority();
        let server = serving(&name, authority, done.clone());
        let route = AuthorityRoute::new(&name, &me()).unwrap();
        assert_eq!(route.ask("l", "t"), Verdict::Current);
        *switch.0.lock().unwrap() = "submitted";
        let ended = route.ask("l", "t");
        assert!(refused_for(&ended, "submitted, not active"), "{ended:?}");
        let stranger = route.ask("m", "t");
        assert!(refused_for(&stranger, "holds no attempt"), "{stranger:?}");
        let other = route.ask("l", "u");
        assert!(refused_for(&other, "another task"), "{other:?}");
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn a_supervisor_that_is_missing_or_another_accounts_refuses_the_push() {
        let missing = AuthorityRoute::new(&unique(), &me()).unwrap();
        let gone = missing.ask("l", "t");
        assert!(refused_for(&gone, "could not be asked"), "{gone:?}");
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = serving(&name, authority().0, done.clone());
        let foreign = AuthorityRoute::new(&name, "S-1-5-80-1-2-3-4-5").unwrap();
        let squatted = foreign.ask("l", "t");
        assert!(
            refused_for(&squatted, "not by the serving account"),
            "{squatted:?}"
        );
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn a_supervisor_that_never_answers_refuses_within_the_patience() {
        let name = unique();
        let mut pipe = Pipe::control(&name, &me(), &me(), None).unwrap();
        std::thread::spawn(move || {
            if let Ok(Accepted::Client(mut session)) = pipe.accept(Duration::from_secs(10)) {
                let _ = session.read_line(Duration::from_secs(10));
                std::thread::sleep(Duration::from_secs(30));
            }
        });
        let route = AuthorityRoute::new(&name, &me()).unwrap();
        let started = Instant::now();
        let silent = route.ask("l", "t");
        assert!(refused_for(&silent, "did not answer"), "{silent:?}");
        assert!(started.elapsed() < PATIENCE + Duration::from_secs(1));
    }

    #[test]
    fn a_line_that_is_no_question_is_answered_as_such_and_judges_nothing() {
        let (authority, _) = authority();
        for line in [
            &b"{\"op\":\"authority\",\"launch\":\"l\"}"[..],
            b"{\"op\":\"authority\",\"launch\":\"l\",\"task\":\"t\",\"role\":\"implementer\"}",
            b"not json",
        ] {
            let answer = respond(&authority, line);
            assert_eq!(answer["ok"], false, "{answer}");
            assert!(verdict_of(&answer).is_err());
        }
        let malformed = verdict_of(&json!({"ok": true}));
        assert!(malformed.is_err(), "an answer without a verdict");
    }

    #[test]
    fn a_route_takes_only_a_plain_pipe_name_and_sid_text() {
        assert!(AuthorityRoute::new(r"..\x", &me()).is_err());
        assert!(AuthorityRoute::new(DEFAULT_PIPE, "WD").is_err());
    }
}
