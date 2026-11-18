//! A broker's control pipe (phase 6, step 6b): how the runner tells a
//! broker service which launches to serve.
//!
//! The broker's account owns the pipe ([`Pipe::control`]) and only the
//! runner's account is served. The runner opens the pipe through
//! [`pipe_client::Client`], which refuses a pipe any other account owns, so
//! a pipe somebody else made first under the broker's name is never told
//! about a launch. One request line gets one answer line, as on the
//! runner's own pipe.
//!
//! An `open` names the launch, its role, its package and the generation it
//! asks for; the launch's account is the account Windows says the control
//! client runs as, never a field of the request. So a runner can only have
//! launches of its own account served. A `close` ends the launch's grant,
//! its sessions and its pipe ([`Broker::close`]).
use super::broker::{Broker, Grant, Role, pipe_name};
use super::pipe::{Accepted, Line, Pipe, Session};
use super::pipe_client;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use windows_sys::Win32::Foundation::HANDLE;

/// How long the control server waits for the next request line of a
/// connected runner before it looks whether it should stop.
const LINE_PATIENCE: Duration = Duration::from_millis(500);

/// The name of the control pipe of the broker `service`, below `\\.\pipe\`.
pub fn control_pipe_name(service: &str) -> String {
    format!("agentc-{service}-control")
}

/// One request on a control pipe.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    /// Serve `launch` on a pipe of its own.
    Open {
        launch: String,
        role: Role,
        package: String,
        generation: u64,
    },
    /// Stop serving `launch`.
    Close { launch: String },
}

/// A broker served through its control pipe.
pub struct Control {
    pipe: Pipe,
    broker: Broker,
}

impl Control {
    /// Serves `broker`'s control pipe `name` to the runner's account
    /// `runner` (SID text); `wake` interrupts waits.
    pub fn new(name: &str, broker: Broker, runner: &str, wake: Option<HANDLE>) -> Result<Self> {
        let pipe = Pipe::control(name, broker.owner(), runner, wake)
            .with_context(|| format!("create the control pipe {name}"))?;
        Ok(Self { pipe, broker })
    }

    /// The broker this pipe controls.
    pub fn broker(&mut self) -> &mut Broker {
        &mut self.broker
    }

    /// Waits up to `patience` for a runner and answers its requests until it
    /// leaves or `stop` says to. A refused client is reported and nothing
    /// is written to it.
    pub fn serve_one(&mut self, patience: Duration, stop: &dyn Fn() -> bool) -> Result<()> {
        match self.pipe.accept(patience)? {
            Accepted::Client(mut session) => answer_all(&mut self.broker, &mut session, stop),
            Accepted::Refused(refusal) => {
                eprintln!("agentc-supervisor: broker control pipe: {refusal}");
                Ok(())
            }
            Accepted::Idle => Ok(()),
        }
    }
}

/// Answers one runner's request lines until it leaves or `stop` says to.
fn answer_all(
    broker: &mut Broker,
    session: &mut Session<'_>,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    let account = session.client().account.clone();
    while !stop() {
        let line = match session.read_line(LINE_PATIENCE)? {
            Line::Request(line) => line,
            Line::Idle => continue,
            Line::Closed => break,
        };
        let mut answer = respond(broker, &account, &line).to_string();
        answer.push('\n');
        session.write_all(answer.as_bytes())?;
    }
    Ok(())
}

/// The answer to one request line of the runner running as `account`:
/// `ok` true with the outcome, or `ok` false with the error.
fn respond(broker: &mut Broker, account: &str, line: &[u8]) -> Value {
    let outcome = serde_json::from_slice(line)
        .context("the request is not one a control pipe takes")
        .and_then(|request| carry_out(broker, account, request));
    match outcome {
        Ok(mut answer) => {
            answer["ok"] = json!(true);
            answer
        }
        Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
    }
}

/// Carries out one parsed request for the runner's `account`.
fn carry_out(broker: &mut Broker, account: &str, request: Request) -> Result<Value> {
    match request {
        Request::Open {
            launch,
            role,
            package,
            generation,
        } => {
            let grant = Grant::new(&launch, role, account, &package, generation)?;
            let granted = broker.open(grant)?;
            let pipe = pipe_name(broker.service(), &launch);
            Ok(json!({"pipe": pipe, "generation": granted.generation()}))
        }
        Request::Close { launch } => Ok(json!({"closed": broker.close(&launch)?})),
    }
}

/// What a broker answered to an `open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The launch's pipe, below `\\.\pipe\`.
    pub pipe: String,
    /// The generation the broker gave the launch.
    pub generation: u64,
}

/// The runner's side of a control pipe.
pub struct ControlClient {
    client: pipe_client::Client,
}

impl ControlClient {
    /// Connects to the control pipe `name` and verifies that the broker's
    /// account `broker` (SID text) owns it.
    pub fn connect(name: &str, broker: &str) -> Result<Self> {
        let client = pipe_client::Client::connect(name, broker)?;
        Ok(Self { client })
    }

    /// Asks the broker to serve `launch`, which works as `role` and carries
    /// `package`, at a generation of at least `generation`.
    pub fn open(
        &mut self,
        launch: &str,
        role: Role,
        package: &str,
        generation: u64,
    ) -> Result<Opened> {
        let request = json!({"op": "open", "launch": launch, "role": role,
            "package": package, "generation": generation});
        let answer = self.ask(&request)?;
        let pipe = answer["pipe"]
            .as_str()
            .context("the broker named no pipe")?;
        let generation = answer["generation"]
            .as_u64()
            .context("the broker gave no generation")?;
        Ok(Opened {
            pipe: pipe.to_owned(),
            generation,
        })
    }

    /// Asks the broker to stop serving `launch`; false when it served none.
    pub fn close(&mut self, launch: &str) -> Result<bool> {
        let answer = self.ask(&json!({"op": "close", "launch": launch}))?;
        answer["closed"]
            .as_bool()
            .context("the broker did not say whether it closed")
    }

    /// One exchange; a refusal by the broker is an error with its reason.
    fn ask(&mut self, request: &Value) -> Result<Value> {
        let answer = self.client.ask(request)?;
        if answer["ok"] != json!(true) {
            let why = answer["error"].as_str().unwrap_or("no reason given");
            bail!("the broker refused: {why}");
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::super::broker::Halt;
    use super::super::broker::Handler;
    use super::super::token;
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";

    /// A handler nobody reaches: no token in these tests carries a package.
    struct Unused;

    impl Handler for Unused {
        fn serve(&self, _: &Grant, _: &mut Session<'_>, _: &Halt) -> Result<()> {
            bail!("no launch reaches this broker")
        }
    }

    /// This process's account.
    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// A control pipe name no other test or run uses.
    fn unique() -> String {
        format!("agentc-test-control-{}", uuid::Uuid::new_v4().simple())
    }

    /// Serves the control pipe `name` of a test broker on a thread until
    /// `done` is set; the thread hands back the broker's launches.
    fn serving(name: &str, done: Arc<AtomicBool>) -> std::thread::JoinHandle<Vec<String>> {
        let broker = Broker::new("ctltest", &me(), Arc::new(Unused)).unwrap();
        let mut control = Control::new(name, broker, &me(), None).unwrap();
        std::thread::spawn(move || {
            let stop = || done.load(Ordering::SeqCst);
            while !stop() {
                control
                    .serve_one(Duration::from_millis(100), &stop)
                    .unwrap();
            }
            control.broker().launches()
        })
    }

    #[test]
    fn the_runner_opens_and_closes_launches_through_the_control_pipe() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = serving(&name, done.clone());
        let mut client = ControlClient::connect(&name, &me()).unwrap();
        let opened = client.open("l1", Role::Reviewer, PACKAGE, 4).unwrap();
        let expected = Opened {
            pipe: pipe_name("ctltest", "l1"),
            generation: 4,
        };
        assert_eq!(opened, expected);
        let again = client.open("l1", Role::Reviewer, PACKAGE, 1).unwrap();
        assert_eq!(again.generation, 5, "opened again");
        client
            .open("l2", Role::Implementer, "S-1-15-2-9", 1)
            .unwrap();
        assert!(client.close("l1").unwrap());
        assert!(!client.close("l1").unwrap(), "closed twice");
        drop(client);
        done.store(true, Ordering::SeqCst);
        assert_eq!(server.join().unwrap(), ["l2"]);
    }

    #[test]
    fn a_request_the_broker_refuses_comes_back_as_an_error_with_its_reason() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = serving(&name, done.clone());
        let mut client = ControlClient::connect(&name, &me()).unwrap();
        let forged = client.open("l", Role::Reviewer, "S-1-15-2)(A;;GA;;;WD", 1);
        assert!(forged.unwrap_err().to_string().contains("not SID text"));
        client.open("l", Role::Reviewer, PACKAGE, 1).unwrap();
        let taken = client.open("m", Role::Reviewer, PACKAGE, 1).unwrap_err();
        assert!(taken.to_string().contains("another launch's"), "{taken:#}");
        let answer = client
            .client
            .ask(&json!({"op": "open", "launch": "x"}))
            .unwrap();
        assert_eq!(answer["ok"], false, "{answer}");
        let account = client.client.ask(&json!({"op": "open", "launch": "x",
            "role": "reviewer", "package": PACKAGE, "generation": 1, "account": "S-1-5-18"}));
        assert_eq!(
            account.unwrap()["ok"],
            false,
            "an account field is not taken"
        );
        drop(client);
        done.store(true, Ordering::SeqCst);
        assert_eq!(server.join().unwrap(), ["l"]);
    }

    #[test]
    fn a_control_pipe_the_broker_does_not_own_is_never_used() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = serving(&name, done.clone());
        let stranger = "S-1-5-80-1-2-3-4-5";
        let refused = ControlClient::connect(&name, stranger).err().unwrap();
        assert!(
            refused.to_string().contains("not by the serving account"),
            "{refused:#}"
        );
        assert!(ControlClient::connect(&unique(), &me()).is_err(), "no pipe");
        done.store(true, Ordering::SeqCst);
        assert!(server.join().unwrap().is_empty());
    }

    #[test]
    fn the_control_pipe_name_carries_the_service() {
        assert_eq!(control_pipe_name("egress"), "agentc-egress-control");
    }
}
