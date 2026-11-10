//! The host runtime of a supervisor that runs no launch itself: every
//! operation is a typed request to the role runner service, sent over its
//! pipe ([`super::pipe_client`]). The supervisor's account never holds a
//! launch's job, token or directories; the runner's does.
//!
//! What a launch runs comes from a [`Planner`], as in [`super::WindowsHost`].
//!
//! Three rules keep an unreliable connection from being mistaken for
//! knowledge:
//!
//! - An exchange that fails, a refusal and an answer that cannot be read
//!   are never "the launch ended". [`RemoteHost::observe`] answers running
//!   and [`RemoteHost::recover`] answers that the launch may be running.
//! - A launch is started by one `start` request and never by a second: the
//!   runner refuses to prepare an id it has journaled, and this runtime
//!   forgets a launch's command once it asked for the start.
//! - Nothing is removed for a launch the runner says may still run.
use super::Planner;
use super::pipe_client::Client;
use super::process::Command;
use crate::host_runtime::{HostRuntime, LaunchIdentity, Observation, Recovery, StartIntent};
use crate::run_loop::Launch;
use crate::run_loop::record::LaunchRecord;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use uuid::Uuid;

/// One request to the runner and its answer. An error means the exchange
/// failed and the request's outcome is unknown.
pub trait Link {
    fn ask(&mut self, request: &Value) -> Result<Value>;
}

/// The link over the runner service's pipe. It connects when first needed
/// and again after a failed exchange; every connection verifies the pipe's
/// owner anew.
#[derive(Debug)]
pub struct PipeLink {
    pipe_name: String,
    runner_sid: String,
    client: Option<Client>,
}

impl PipeLink {
    /// A link to the pipe `pipe_name`, which the account `runner_sid` must
    /// own. Nothing is opened yet.
    pub fn new(pipe_name: &str, runner_sid: &str) -> Self {
        Self {
            pipe_name: pipe_name.to_owned(),
            runner_sid: runner_sid.to_owned(),
            client: None,
        }
    }
}

impl Link for PipeLink {
    /// Asks over the current connection, connecting first if there is
    /// none. A failed exchange drops the connection; the request is not
    /// sent again here.
    fn ask(&mut self, request: &Value) -> Result<Value> {
        if self.client.is_none() {
            self.client = Some(Client::connect(&self.pipe_name, &self.runner_sid)?);
        }
        let client = self.client.as_mut().expect("the client was just set");
        let answer = client.ask(request);
        if answer.is_err() {
            self.client = None;
        }
        answer
    }
}

/// What the runner's journal says about one launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// The journal does not name it: nothing of it exists at the runner.
    Absent,
    /// The running runner holds it; `started` says whether it has a root.
    Held { started: bool },
    /// An earlier runner's, and Windows says it ended.
    Ended,
    /// An earlier runner's that may still run.
    MayRun,
}

/// Runs launches through a role runner reached over `L`.
pub struct RemoteHost<P: Planner, L: Link> {
    planner: P,
    /// Behind a cell because judging a saved record ([`HostRuntime::recover`])
    /// has to ask the runner and takes the runtime by shared reference.
    link: RefCell<L>,
    /// The command of each prepared launch, until its start is asked for.
    planned: BTreeMap<Uuid, Command>,
}

impl<P: Planner, L: Link> RemoteHost<P, L> {
    /// A runtime whose launches follow `planner` and run at the runner
    /// behind `link`.
    pub fn new(planner: P, link: L) -> Self {
        Self {
            planner,
            link: RefCell::new(link),
            planned: BTreeMap::new(),
        }
    }

    /// Sends `request` and returns the runner's answer if it says `ok`. A
    /// refusal is an error that carries the runner's reason.
    fn call(&self, request: Value) -> Result<Value> {
        let answer = self.link.borrow_mut().ask(&request)?;
        if answer["ok"] == json!(true) {
            return Ok(answer);
        }
        let reason = answer["error"].as_str().unwrap_or("no reason given");
        bail!("the runner refused {}: {reason}", request["op"])
    }

    /// The request `op` about `launch`.
    fn about(op: &str, launch: &Launch) -> Value {
        json!({ "op": op, "launch": launch.session_id.to_string() })
    }

    /// Where the runner's journal has the launch `id`. An unreadable entry
    /// could be this launch's, so the launch then counts as one that may
    /// run.
    fn standing(&self, id: &str) -> Result<Standing> {
        let answer = self.call(json!({ "op": "recover" }))?;
        let launches = answer["launches"].as_array();
        let listed = launches.context("the runner's journal answer names no launches")?;
        let Some(entry) = listed.iter().find(|entry| entry["launch"] == id) else {
            let torn = answer["unreadable"]
                .as_array()
                .is_none_or(|u| !u.is_empty());
            return Ok(if torn {
                Standing::MayRun
            } else {
                Standing::Absent
            });
        };
        let started = entry["stage"] != "prepared" && entry["stage"] != "ended";
        Ok(match entry["verdict"].as_str() {
            Some("held") => Standing::Held { started },
            Some("ended") => Standing::Ended,
            _ => Standing::MayRun,
        })
    }

    /// After a start that failed or whose answer was lost, the launch may
    /// run all the same, and the loop is about to release its attempt. The
    /// runner is asked to end whatever started; the start stays failed.
    fn undo_start(&self, launch: &Launch, error: anyhow::Error) -> anyhow::Error {
        match self.call(Self::about("terminate", launch)) {
            Ok(_) => error.context("the start failed; the runner ended whatever had started"),
            Err(second) => error.context(format!(
                "the start failed and the runner could not be asked to end it ({second:#})"
            )),
        }
    }

    /// Whether the root of a launch the runner holds has been seen to end.
    fn root_exited(&self, id: &str) -> bool {
        let observed = self.call(json!({ "op": "observe", "launch": id, "wait_ms": 0 }));
        observed.is_ok_and(|answer| answer["state"] == "exited")
    }
}

impl<P: Planner, L: Link> HostRuntime for RemoteHost<P, L> {
    /// Has the runner create the launch's profile, directories and desktop,
    /// and says which package, job owner and executable identify it.
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent> {
        let (spec, command) = self.planner.plan(launch)?;
        let mut request = Self::about("prepare", launch);
        request["spec"] = serde_json::to_value(&spec)?;
        let answer = self.call(request)?;
        let text = |field: &str| answer[field].as_str().map(str::to_owned);
        let intent = StartIntent {
            boot_id: String::new(),
            source: None,
            executable: command.argv.first().cloned(),
            container: text("package_sid"),
            job: text("job_owner"),
        };
        self.planned.insert(launch.session_id, command);
        Ok(intent)
    }

    /// Asks the runner to start the planned command, once. The command is
    /// forgotten before the request goes out, so a failed exchange cannot
    /// be followed by a second start.
    fn start(&mut self, launch: &Launch) -> Result<LaunchIdentity> {
        let command = self.planned.remove(&launch.session_id);
        let command = command.context("the launch was not prepared, or its start was asked for")?;
        let mut request = Self::about("start", launch);
        request["argv"] = json!(command.argv);
        request["directory"] = json!(command.directory);
        request["environment"] = json!(command.environment);
        request["output"] = json!(command.output);
        let answer = match self.call(request) {
            Ok(answer) => answer,
            Err(error) => return Err(self.undo_start(launch, error)),
        };
        let pid = answer["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok());
        Ok(LaunchIdentity {
            pid: pid.context("the runner's answer names no root process")?,
            started: answer["created"].as_u64(),
        })
    }

    /// The root's exit, once the runner reports it. Anything else,
    /// including every failure to ask, is running.
    fn observe(&mut self, launch: &Launch) -> Observation {
        let mut request = Self::about("observe", launch);
        request["wait_ms"] = json!(0);
        match self.call(request) {
            Ok(answer) if answer["state"] == "exited" => match answer["code"].as_u64() {
                Some(code) => Observation::Exited {
                    code: code as u32 as i32,
                },
                None => Observation::Running,
            },
            Ok(_) => Observation::Running,
            Err(error) => {
                eprintln!("agentc-supervisor run: observe the launch: {error:#}");
                Observation::Running
            }
        }
    }

    /// Has the runner put the stop marker in the launch's control directory.
    fn request_stop(&mut self, launch: &Launch) {
        if let Err(error) = self.call(Self::about("request_stop", launch)) {
            eprintln!("agentc-supervisor run: ask the launch to stop: {error:#}");
        }
    }

    /// Has the runner end every process in the launch's job.
    fn terminate(&mut self, launch: &Launch) {
        match self.call(Self::about("terminate", launch)) {
            Ok(answer) if answer["quiescent"] == json!(true) => {}
            Ok(_) => eprintln!("agentc-supervisor run: the launch's job did not empty"),
            Err(error) => eprintln!("agentc-supervisor run: terminate the launch: {error:#}"),
        }
    }

    /// Asks the runner where the recorded launch stands. It has ended when
    /// the runner's journal does not name it, when Windows says an earlier
    /// runner's launch ended, or when the running runner holds it and its
    /// root is not running. Whatever the runner cannot say, or a runner
    /// that cannot be asked, leaves the launch possibly running.
    fn recover(&self, record: &LaunchRecord, _now_ms: i64) -> Recovery {
        let id = record.session_id.to_string();
        let ended = match self.standing(&id) {
            Ok(Standing::Absent | Standing::Ended) => true,
            Ok(Standing::Held { started }) => !started || self.root_exited(&id),
            Ok(Standing::MayRun) => false,
            Err(error) => {
                eprintln!("agentc-supervisor run: ask the runner about {id}: {error:#}");
                false
            }
        };
        if ended {
            Recovery::Ended
        } else {
            Recovery::MayBeRunning
        }
    }

    /// Has the runner end the launch and remove its desktop, profile and
    /// directories. A launch that may still run is left whole.
    fn cleanup(&mut self, launch: &Launch) -> Result<()> {
        let id = launch.session_id.to_string();
        self.planned.remove(&launch.session_id);
        let op = match self.standing(&id)? {
            Standing::Absent => return Ok(()),
            Standing::Held { .. } => "cleanup",
            Standing::Ended => "reclaim",
            Standing::MayRun => bail!("launch {id} may still run; nothing of it is removed"),
        };
        let mut request = Self::about(op, launch);
        request["remove_directories"] = json!(true);
        self.call(request).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::super::job::Limits;
    use super::super::launcher::LaunchSpec;
    use super::super::pipe::{Accepted, Line, Pipe};
    use super::super::runner::{Runner, answer};
    use super::*;
    use crate::config::Config;
    use crate::run_loop::Suggestion;
    use crate::run_loop::lease::Lease;
    use std::rc::Rc;

    /// A runner that answers from a script and remembers what it was asked.
    #[derive(Default)]
    struct Script {
        asked: Vec<Value>,
        /// The journal `recover` reports.
        journal: Vec<Value>,
        unreadable: Vec<String>,
        /// What `observe` answers.
        state: &'static str,
        /// Requests whose exchange fails, by `op`.
        broken: Vec<&'static str>,
        /// Requests the runner refuses, by `op`.
        refused: Vec<&'static str>,
    }

    /// A link to a shared [`Script`].
    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<Script>>);

    impl Link for Fake {
        fn ask(&mut self, request: &Value) -> Result<Value> {
            let mut script = self.0.borrow_mut();
            script.asked.push(request.clone());
            let op = request["op"].as_str().unwrap_or("");
            if script.broken.contains(&op) {
                bail!("the pipe broke");
            }
            if script.refused.contains(&op) {
                return Ok(json!({"ok": false, "error": "refused by the test"}));
            }
            Ok(match op {
                "prepare" => json!({"ok": true, "package_sid": "S-1-15-2-7",
                                    "job_owner": "runner:10:500", "desktop": "s\\d"}),
                "start" => json!({"ok": true, "pid": 4242, "created": 77}),
                "observe" => json!({"ok": true, "state": script.state, "code": 3_221_225_786_u64}),
                "terminate" => json!({"ok": true, "quiescent": true}),
                "recover" => json!({"ok": true, "launches": script.journal,
                                    "unreadable": script.unreadable}),
                _ => json!({"ok": true}),
            })
        }
    }

    impl Fake {
        /// The `op` of every request so far.
        fn ops(&self) -> Vec<String> {
            let script = self.0.borrow();
            let op = |request: &Value| request["op"].as_str().unwrap_or("").to_owned();
            script.asked.iter().map(op).collect()
        }
    }

    /// Plans the same fixed boundary and command for every launch.
    struct Fixed;

    impl Planner for Fixed {
        fn plan(&self, launch: &Launch) -> Result<(LaunchSpec, Command)> {
            let spec = LaunchSpec {
                profile: format!("agentc.test.{}", launch.session_id.simple()),
                less_privileged: true,
                capabilities: vec![],
                limits: Limits {
                    processes: 4,
                    process_memory_mb: 64,
                    job_memory_mb: 128,
                },
                control: "c".into(),
                work: "w".into(),
            };
            let command = Command {
                argv: vec!["fixture.exe".into(), "run".into()],
                directory: "w".into(),
                environment: BTreeMap::new(),
                output: "w/out.log".into(),
            };
            Ok((spec, command))
        }
    }

    fn launch() -> Launch {
        let suggestion = Suggestion {
            task: "t".into(),
            revision: 1,
            title: String::new(),
        };
        Launch::plan(&Config::default(), "p", suggestion)
    }

    fn host() -> (RemoteHost<Fixed, Fake>, Fake) {
        let fake = Fake::default();
        fake.0.borrow_mut().state = "running";
        (RemoteHost::new(Fixed, fake.clone()), fake)
    }

    /// The record of `launch` once it started.
    fn record(launch: &Launch) -> LaunchRecord {
        let lease = Lease {
            attempt: "a1".into(),
            generation: 1,
            renew_after_seconds: 60,
            progress_age_ms: 0,
        };
        LaunchRecord::new(launch, &lease, &StartIntent::default(), 0)
    }

    /// A journal line for `launch` as `recover` reports it.
    fn journaled(launch: &Launch, stage: &str, verdict: &str) -> Value {
        json!({"launch": launch.session_id.to_string(), "stage": stage, "verdict": verdict})
    }

    #[test]
    fn a_launch_is_prepared_and_started_by_typed_requests_and_identified() {
        let ((mut host, fake), launch) = (host(), launch());
        let intent = host.prepare(&launch).unwrap();
        assert_eq!(intent.container.as_deref(), Some("S-1-15-2-7"));
        assert_eq!(intent.job.as_deref(), Some("runner:10:500"));
        assert_eq!(intent.executable.as_deref(), Some("fixture.exe"));
        let identity = host.start(&launch).unwrap();
        assert_eq!((identity.pid, identity.started), (4242, Some(77)));
        let asked = fake.0.borrow().asked.clone();
        assert_eq!(asked[0]["spec"]["less_privileged"], true);
        assert_eq!(asked[1]["argv"], json!(["fixture.exe", "run"]));
        assert_eq!(asked[1]["launch"], launch.session_id.to_string());
    }

    #[test]
    fn a_launch_is_never_started_twice_even_after_a_failed_exchange() {
        let ((mut host, fake), launch) = (host(), launch());
        host.prepare(&launch).unwrap();
        fake.0.borrow_mut().broken = vec!["start"];
        assert!(host.start(&launch).is_err());
        fake.0.borrow_mut().broken.clear();
        let again = host.start(&launch).unwrap_err().to_string();
        assert!(again.contains("its start was asked for"), "{again}");
        let starts = fake.ops().iter().filter(|op| *op == "start").count();
        assert_eq!(starts, 1);
        // The lost start may have run, so the runner was asked to end it.
        assert_eq!(fake.ops(), ["prepare", "start", "terminate"]);
        assert!(host.start(&self::launch()).is_err(), "an unprepared launch");
    }

    #[test]
    fn only_a_reported_exit_is_an_exit() {
        let ((mut host, fake), launch) = (host(), launch());
        assert_eq!(host.observe(&launch), Observation::Running);
        for failing in ["broken", "refused"] {
            let mut script = fake.0.borrow_mut();
            (script.broken, script.refused) = (vec![], vec![]);
            match failing {
                "broken" => script.broken = vec!["observe"],
                _ => script.refused = vec!["observe"],
            }
            drop(script);
            assert_eq!(host.observe(&launch), Observation::Running, "{failing}");
        }
        let mut script = fake.0.borrow_mut();
        (script.broken, script.refused, script.state) = (vec![], vec![], "exited");
        drop(script);
        // An NTSTATUS exit code shows as the same 32 bits, negative.
        let exited = Observation::Exited {
            code: 0xC000_013A_u32 as i32,
        };
        assert_eq!(host.observe(&launch), exited);
    }

    #[test]
    fn a_runner_that_cannot_be_asked_never_makes_a_recorded_launch_ended() {
        let ((host, fake), launch) = (host(), launch());
        let record = record(&launch);
        for failing in ["broken", "refused"] {
            let mut script = fake.0.borrow_mut();
            (script.broken, script.refused) = (vec![], vec![]);
            match failing {
                "broken" => script.broken = vec!["recover"],
                _ => script.refused = vec!["recover"],
            }
            drop(script);
            assert_eq!(
                host.recover(&record, 0),
                Recovery::MayBeRunning,
                "{failing}"
            );
        }
    }

    #[test]
    fn a_recorded_launch_is_judged_from_the_runners_journal() {
        let ((host, fake), launch) = (host(), launch());
        let record = record(&launch);
        let judge = |journal: Vec<Value>, unreadable: &[&str], state: &'static str| {
            let mut script = fake.0.borrow_mut();
            script.journal = journal;
            script.unreadable = unreadable.iter().map(|name| name.to_string()).collect();
            script.state = state;
            drop(script);
            host.recover(&record, 0)
        };
        // Nothing of it is left at the runner.
        assert_eq!(judge(vec![], &[], "running"), Recovery::Ended);
        // A torn entry could be this launch's.
        assert_eq!(
            judge(vec![], &["torn.json"], "running"),
            Recovery::MayBeRunning
        );
        let earlier = |verdict| vec![journaled(&launch, "started", verdict)];
        assert_eq!(judge(earlier("ended"), &[], "running"), Recovery::Ended);
        assert_eq!(
            judge(earlier("may_be_running"), &[], "running"),
            Recovery::MayBeRunning
        );
        assert_eq!(
            judge(earlier("held"), &[], "running"),
            Recovery::MayBeRunning
        );
        assert_eq!(judge(earlier("held"), &[], "exited"), Recovery::Ended);
        // Held and never started: there is no root to run.
        let prepared = vec![journaled(&launch, "prepared", "held")];
        assert_eq!(judge(prepared, &[], "running"), Recovery::Ended);
    }

    #[test]
    fn cleanup_removes_what_ended_and_leaves_what_may_run() {
        let ((mut host, fake), launch) = (host(), launch());
        let mut clean = |journal: Vec<Value>| {
            let mut script = fake.0.borrow_mut();
            (script.journal, script.asked) = (journal, vec![]);
            drop(script);
            let outcome = host.cleanup(&launch);
            let last = fake.0.borrow().asked.last().cloned().unwrap();
            (outcome.is_ok(), last)
        };
        let (ok, last) = clean(vec![]);
        assert!(ok && last["op"] == "recover", "nothing to remove: {last}");
        let (ok, last) = clean(vec![journaled(&launch, "started", "held")]);
        assert!(ok && last["op"] == "cleanup" && last["remove_directories"] == true);
        let (ok, last) = clean(vec![journaled(&launch, "started", "ended")]);
        assert!(ok && last["op"] == "reclaim" && last["remove_directories"] == true);
        let (ok, last) = clean(vec![journaled(&launch, "started", "may_be_running")]);
        assert!(
            !ok && last["op"] == "recover",
            "nothing was asked to go: {last}"
        );
    }

    /// Serves one client of `pipe` with a real runner until it leaves.
    fn serve_one(pipe: &mut Pipe, runner: &mut Runner) {
        let patience = std::time::Duration::from_secs(10);
        let Accepted::Client(mut session) = pipe.accept(patience).unwrap() else {
            panic!("the runtime was not admitted");
        };
        while let Line::Request(bytes) = session.read_line(patience).unwrap() {
            let request = serde_json::from_slice(&bytes).map_err(anyhow::Error::from);
            let mut reply = answer(request.and_then(|r| runner.handle(r))).to_string();
            reply.push('\n');
            session.write_all(reply.as_bytes()).unwrap();
        }
    }

    // No launch is made here, so this runs on an interactive desktop too:
    // the real link, the real pipe and a real runner's journal answers.
    #[test]
    fn a_real_runner_behind_its_pipe_answers_the_runtime() {
        let me = super::super::token::current_user_sid().unwrap();
        let name = format!("agentc-test-remote-{}", Uuid::new_v4().simple());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut runner = Runner::new(state.path()).unwrap();
        let asker = std::thread::spawn(move || {
            let launch = launch();
            let mut host = RemoteHost::new(Fixed, PipeLink::new(&name, &me));
            let judged = host.recover(&record(&launch), 0);
            (judged, host.observe(&launch), host.cleanup(&launch).is_ok())
        });
        serve_one(&mut pipe, &mut runner);
        // Unknown to the runner: ended, nothing to see running, nothing to
        // remove.
        let expected = (Recovery::Ended, Observation::Running, true);
        assert_eq!(asker.join().unwrap(), expected);
    }

    #[test]
    fn stop_and_terminate_are_requests_and_their_failures_are_not_fatal() {
        let ((mut host, fake), launch) = (host(), launch());
        fake.0.borrow_mut().broken = vec!["request_stop"];
        host.request_stop(&launch);
        host.terminate(&launch);
        assert_eq!(fake.ops(), ["request_stop", "terminate"]);
    }
}
