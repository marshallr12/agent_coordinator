//! The publication broker with real AppContainer launches (phase 6, step
//! 6c).
//!
//! Each test runs in a new logon session of the same account ([`support`]).
//! There the supervisor's authority pipe and the publication broker's
//! control pipe are served, the test opens each launch through the control
//! pipe as the runner would, and the broker's client is this test program
//! run again inside the launch. The remote is a bare repository in the
//! test's directory, so nothing leaves the host. A launch sends a bundle
//! the host made, with the request line of the candidate push protocol:
//! what it can publish is decided by its grant and the supervisor, and the
//! remote's refs afterwards show what was published.
#![cfg(windows)]

mod support;

use agentc_supervisor::host_runtime::windows::authority_pipe::{AuthorityRoute, AuthorityServer};
use agentc_supervisor::host_runtime::windows::broker::{Broker, Role};
use agentc_supervisor::host_runtime::windows::broker_control::{
    Control, ControlClient, Opened, control_pipe_name,
};
use agentc_supervisor::host_runtime::windows::egress_admission::PUBLICATION_VARIABLE;
use agentc_supervisor::host_runtime::windows::launcher::Launcher;
use agentc_supervisor::host_runtime::windows::publication_broker::{Publication, SERVICE};
use agentc_supervisor::host_runtime::windows::token;
use agentc_supervisor::service::authority::{ACTIVE, AttemptReader, AttemptView, Authority, Held};
use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use support::{
    Launch, Trees, child_main, client_tools, finish, in_a_new_session, outcome, prepare,
    start_client,
};

/// Which probe the client in a launch runs: `push` or `open`.
const PROBE: &str = "AGENTC_PUBLICATION_TEST_PROBE";
/// The pushes a `push` probe makes, in order, comma-separated.
const STEPS: &str = "AGENTC_PUBLICATION_TEST_STEPS";
/// The bundle the launch sends, the commit it names and the commit's tree.
const BUNDLE: &str = "AGENTC_PUBLICATION_TEST_BUNDLE";
const REVISION: &str = "AGENTC_PUBLICATION_TEST_REVISION";
const TREE: &str = "AGENTC_PUBLICATION_TEST_TREE";
/// A tree that is not the commit's, for the wrong-tree push.
const WRONG_TREE: &str = "AGENTC_PUBLICATION_TEST_WRONG_TREE";
/// The pipe an `open` probe tries.
const TARGET: &str = "AGENTC_PUBLICATION_TEST_TARGET";
/// The task every launch of these tests works on.
const TASK: &str = "task-1";
/// The ref the bundle carries, which nothing may publish.
const BUNDLED_REF: &str = "refs/heads/evil";

/// An implementer launch with authority publishes its candidate to its own
/// ref and nothing else, the same commit again is accepted unchanged and a
/// wrong tree is refused;
/// a launch the supervisor holds no attempt for and a reviewer launch are
/// refused; a foreign launch cannot open another launch's pipe; and once
/// the launch is closed its pipe is gone.
#[test]
fn only_an_authorized_implementer_launch_publishes_and_only_its_own_ref() {
    in_a_new_session("publication");
}

/// A launch whose attempt ended, and one whose supervisor is not serving
/// its authority pipe, publish nothing.
#[test]
fn a_launch_whose_authority_ended_or_cannot_be_asked_publishes_nothing() {
    in_a_new_session("lost");
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session or inside a launch.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    child_main(|role| match role {
        "publication" => publication(),
        "lost" => lost(),
        "client" => client(),
        other => panic!("no such role: {other}"),
    });
}

// ---- The host's side, in the new logon session ----

/// This process's account, which the supervisor, the broker and the
/// launches all run as here.
fn me() -> String {
    token::current_user_sid().unwrap()
}

/// A launch id unique on the host: pipe names are global.
fn id(name: &str) -> String {
    format!("pub-{name}-{}", std::process::id())
}

/// Runs Git in `directory` and returns its trimmed output.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output();
    let output = output.unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A bare remote holding a base commit, and a bundle of a candidate on it
/// whose only ref is [`BUNDLED_REF`].
struct Repository {
    remote: PathBuf,
    bundle: PathBuf,
    candidate: String,
    tree: String,
    wrong_tree: String,
}

impl Repository {
    /// Makes the remote and the bundle in `trees`, the bundle in `tools`.
    fn new(trees: &Trees, tools: &Path) -> Self {
        let (remote, checkout) = (trees.at("remote.git"), trees.at("checkout"));
        let root = trees.at("");
        git(
            &root,
            &["init", "--bare", "--quiet", remote.to_str().unwrap()],
        );
        let paths = [remote.to_str().unwrap(), checkout.to_str().unwrap()];
        git(&root, &["clone", "--quiet", paths[0], paths[1]]);
        let base = commit(&checkout, "base.txt");
        git(
            &checkout,
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
        );
        let candidate = commit(&checkout, "feature.txt");
        let bundle = tools.join("candidate.bundle");
        git(&checkout, &["update-ref", BUNDLED_REF, &candidate]);
        let excluded = format!("^{base}");
        git(
            &checkout,
            &[
                "bundle",
                "create",
                bundle.to_str().unwrap(),
                BUNDLED_REF,
                &excluded,
            ],
        );
        let tree_of = |commit: &str| git(&checkout, &["rev-parse", &format!("{commit}^{{tree}}")]);
        let (tree, wrong_tree) = (tree_of(&candidate), tree_of(&base));
        Self {
            remote,
            bundle,
            candidate,
            tree,
            wrong_tree,
        }
    }

    /// The remote's refs, one `name commit` per line, in name order.
    fn refs(&self) -> String {
        git(
            &self.remote,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    /// What a launch needs to send the bundle.
    fn variables(&self) -> Vec<(&'static str, String)> {
        vec![
            (BUNDLE, self.bundle.display().to_string()),
            (REVISION, self.candidate.clone()),
            (TREE, self.tree.clone()),
            (WRONG_TREE, self.wrong_tree.clone()),
        ]
    }
}

/// Commits a file named `name` and returns the commit.
fn commit(checkout: &Path, name: &str) -> String {
    std::fs::write(checkout.join(name), name).unwrap();
    git(checkout, &["add", "--", name]);
    let identity = [
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
    ];
    let mut arguments = identity.to_vec();
    arguments.extend(["commit", "--quiet", "-m", name]);
    git(checkout, &arguments);
    git(checkout, &["rev-parse", "HEAD"])
}

/// A coordinator whose attempts are active until the test ends them.
#[derive(Default)]
struct Coordinator {
    ended: Mutex<bool>,
}

/// The authority's view of [`Coordinator`].
struct Reads(Arc<Coordinator>);

impl AttemptReader for Reads {
    fn read(&self, _task: &str, _attempt: &str) -> anyhow::Result<Option<AttemptView>> {
        let state = if *self.0.ended.lock().unwrap() {
            "released"
        } else {
            ACTIVE
        };
        Ok(Some(AttemptView {
            state: state.into(),
            generation: 1,
        }))
    }
}

/// A pipe served on a thread until it is dropped.
struct Serving {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Serving {
    /// Calls `serve_one` on a thread until the value is dropped.
    fn start(mut serve_one: impl FnMut(&dyn Fn() -> bool) + Send + 'static) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let thread = std::thread::spawn(move || {
            let stopping = || stop.load(Ordering::SeqCst);
            while !stopping() {
                serve_one(&stopping);
            }
        });
        Self {
            done,
            thread: Some(thread),
        }
    }
}

impl Drop for Serving {
    /// Stops the thread, which removes its pipe.
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The supervisor's authority, its pipe, and the broker's control pipe.
struct Services {
    authority: Arc<Authority>,
    coordinator: Arc<Coordinator>,
    supervisor: Option<Serving>,
    _broker: Serving,
    control: String,
}

impl Services {
    /// Serves both pipes, the broker publishing to `remote`.
    fn start(trees: &Trees, remote: &Path) -> Self {
        let coordinator = Arc::new(Coordinator::default());
        let authority = Arc::new(Authority::new(Box::new(Reads(coordinator.clone()))));
        let unique = uuid::Uuid::new_v4().simple();
        let (asked, control) = (
            format!("agentc-test-authority-{unique}"),
            format!("{}-{unique}", control_pipe_name("test")),
        );
        let mut server =
            AuthorityServer::new(&asked, &me(), &me(), authority.clone(), None).unwrap();
        let supervisor =
            Serving::start(move |stop| server.serve_one(Duration::from_millis(100), stop).unwrap());
        let route = Arc::new(AuthorityRoute::new(&asked, &me()).unwrap());
        let publication = Publication::new(remote.to_str().unwrap(), trees.at("broker"), route);
        let broker = Broker::new(SERVICE, &me(), Arc::new(publication)).unwrap();
        let mut served = Control::new(&control, broker, &me(), None).unwrap();
        let broker =
            Serving::start(move |stop| served.serve_one(Duration::from_millis(100), stop).unwrap());
        Self {
            authority,
            coordinator,
            supervisor: Some(supervisor),
            _broker: broker,
            control,
        }
    }

    /// A runner's connection to the broker's control pipe.
    fn control(&self) -> ControlClient {
        ControlClient::connect(&self.control, &me()).unwrap()
    }

    /// Has the broker admit `launch` as `role` working on [`TASK`].
    fn admit(&self, launch: &Launch, role: Role) -> Opened {
        let opened = self
            .control()
            .open_for(&launch.id, role, &launch.package, 1, Some(TASK));
        opened.unwrap()
    }

    /// Records, as the supervisor's loop does at its claim, that `launch`
    /// holds an attempt of [`TASK`].
    fn hold(&self, launch: &Launch) {
        let held = Held {
            task: TASK.into(),
            attempt: format!("attempt-{}", launch.id),
            generation: 1,
        };
        self.authority.hold(&launch.id, held);
    }
}

/// Starts the client in `launch` with `probe` and `extra` variables, and
/// its publication pipe when it has one.
fn start_probe(
    launcher: &mut Launcher,
    (launch, tools): (&Launch, &Path),
    probe: &str,
    (opened, mut extra): (Option<&Opened>, Vec<(&'static str, String)>),
) -> PathBuf {
    extra.push((PROBE, probe.to_owned()));
    if let Some(opened) = opened {
        extra.push((PUBLICATION_VARIABLE, pipe_path(&opened.pipe).unwrap()));
    }
    start_client(launcher, launch, tools, extra)
}

/// Runs the `push` probe in `launch` with `steps` and returns its report.
fn push(
    launcher: &mut Launcher,
    (launch, tools): (&Launch, &Path),
    (opened, repository): (&Opened, &Repository),
    steps: &str,
) -> Vec<String> {
    let mut variables = repository.variables();
    variables.push((STEPS, steps.to_owned()));
    let report = start_probe(launcher, (launch, tools), "push", (Some(opened), variables));
    finish(launcher, launch, &report)
}

/// The candidate ref of `launch`.
fn candidate_ref(launch: &Launch) -> String {
    format!("refs/agent-coordinator/candidates/{TASK}/{}", launch.id)
}

/// Test (publication): who may publish what.
fn publication() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
    let ids = ["a", "b", "rev", "foreign"].map(id);
    let launches = ids
        .each_ref()
        .map(|name| prepare(&mut launcher, &trees, name));
    let packages = launches.each_ref().map(|launch| launch.package.as_str());
    let tools = client_tools(&trees, &packages);
    let repository = Repository::new(&trees, &tools);
    let services = Services::start(&trees, &repository.remote);
    let [own, unheld, reviewer, foreign] = &launches;
    let said = [
        authorized(&mut launcher, &services, (own, &tools), &repository),
        unauthorized(&mut launcher, &services, (unheld, &tools), &repository),
        reviewing(&mut launcher, &services, (reviewer, &tools), &repository),
        foreign_open(&mut launcher, &services, (foreign, own), &tools),
    ];
    check_refs(&repository, own);
    for launch in &launches {
        launcher.cleanup(&launch.id).unwrap();
    }
    said.join("; ")
}

/// The authorized implementer: a wrong tree is refused, the candidate is
/// accepted to its own ref, the same commit again is accepted unchanged.
fn authorized(
    launcher: &mut Launcher,
    services: &Services,
    (own, tools): (&Launch, &Path),
    repository: &Repository,
) -> String {
    services.hold(own);
    let opened = services.admit(own, Role::Implementer);
    let lines = push(
        launcher,
        (own, tools),
        (&opened, repository),
        "wrong_tree,good,again",
    );
    assert_eq!(
        outcome(&lines, "wrong_tree"),
        "refused revision_mismatch",
        "{lines:?}"
    );
    let accepted = format!("accepted {} {}", candidate_ref(own), repository.candidate);
    assert_eq!(outcome(&lines, "good"), accepted, "{lines:?}");
    assert_eq!(outcome(&lines, "again"), accepted, "{lines:?}");
    lines.join(", ")
}

/// An implementer launch the supervisor holds no attempt for.
fn unauthorized(
    launcher: &mut Launcher,
    services: &Services,
    (launch, tools): (&Launch, &Path),
    repository: &Repository,
) -> String {
    let opened = services.admit(launch, Role::Implementer);
    let lines = push(launcher, (launch, tools), (&opened, repository), "good");
    assert_eq!(
        outcome(&lines, "good"),
        "refused not_authorized",
        "{lines:?}"
    );
    lines.join(", ")
}

/// A reviewer launch, even one the supervisor holds an attempt for.
fn reviewing(
    launcher: &mut Launcher,
    services: &Services,
    (launch, tools): (&Launch, &Path),
    repository: &Repository,
) -> String {
    services.hold(launch);
    let opened = services.admit(launch, Role::Reviewer);
    let lines = push(launcher, (launch, tools), (&opened, repository), "good");
    assert_eq!(
        outcome(&lines, "good"),
        "refused not_authorized",
        "{lines:?}"
    );
    lines.join(", ")
}

/// A launch opening another launch's pipe is denied; once that launch is
/// closed its pipe is gone even for this account.
fn foreign_open(
    launcher: &mut Launcher,
    services: &Services,
    (foreign, own): (&Launch, &Launch),
    tools: &Path,
) -> String {
    let theirs =
        pipe_path(&agentc_supervisor::host_runtime::windows::broker::pipe_name(SERVICE, &own.id))
            .unwrap();
    let report = start_probe(
        launcher,
        (foreign, tools),
        "open",
        (None, vec![(TARGET, theirs.clone())]),
    );
    let lines = finish(launcher, foreign, &report);
    assert_eq!(outcome(&lines, "open"), "error 5", "{lines:?}");
    assert!(services.control().close(&own.id).unwrap());
    let gone = open_pipe(&theirs).unwrap_err();
    assert_eq!(gone.raw_os_error(), Some(2), "{gone}");
    format!("{}, closed: error 2", lines.join(", "))
}

/// Only the candidate ref of the authorized launch was published, at the
/// candidate; the bundle's own ref never was.
fn check_refs(repository: &Repository, own: &Launch) {
    let refs = repository.refs();
    let expected = format!("{} {}", candidate_ref(own), repository.candidate);
    assert!(refs.lines().any(|line| line == expected), "{refs}");
    let others = refs
        .lines()
        .filter(|line| line.starts_with("refs/agent-coordinator/"));
    assert_eq!(others.count(), 1, "{refs}");
    assert!(!refs.contains(BUNDLED_REF), "{refs}");
}

/// Test (lost): authority that ended, and a supervisor that is not there.
fn lost() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
    let launches = ["ended", "down"]
        .map(id)
        .map(|name| prepare(&mut launcher, &trees, &name));
    let packages = launches.each_ref().map(|launch| launch.package.as_str());
    let tools = client_tools(&trees, &packages);
    let repository = Repository::new(&trees, &tools);
    let mut services = Services::start(&trees, &repository.remote);
    let mut said = Vec::new();
    for (launch, step) in launches.iter().zip(["ended", "down"]) {
        services.hold(launch);
        match step {
            "ended" => *services.coordinator.ended.lock().unwrap() = true,
            _ => {
                *services.coordinator.ended.lock().unwrap() = false;
                drop(services.supervisor.take());
            }
        }
        let opened = services.admit(launch, Role::Implementer);
        let lines = push(
            &mut launcher,
            (launch, &tools),
            (&opened, &repository),
            "good",
        );
        assert_eq!(
            outcome(&lines, "good"),
            "refused not_authorized",
            "{step}: {lines:?}"
        );
        said.push(format!("{step}: {}", lines.join(", ")));
    }
    let refs = repository.refs();
    assert!(!refs.contains("refs/agent-coordinator/"), "{refs}");
    for launch in &launches {
        launcher.cleanup(&launch.id).unwrap();
    }
    said.join("; ")
}

// ---- The client's side, inside a launch ----

/// Opens the pipe at `path` with a client's rights, for identification.
fn open_pipe(path: &str) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    options.security_qos_flags(windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION);
    options.open(path)
}

/// The variable `name`, which the host set.
fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// The client inside a launch: runs the probe [`PROBE`] names.
fn client() -> String {
    match variable(PROBE).as_str() {
        "push" => variable(STEPS)
            .split(',')
            .map(|step| format!("{step} = {}", push_once(step)))
            .collect::<Vec<_>>()
            .join("\n"),
        "open" => match open_pipe(&variable(TARGET)) {
            Ok(_) => "open = opened".into(),
            Err(error) => format!("open = error {}", error.raw_os_error().unwrap_or(-1)),
        },
        other => panic!("no such probe: {other}"),
    }
}

/// One push on a new session: the request line, the bundle's length and
/// bytes, then the reply, as `accepted <ref> <commit>` or `refused <code>`.
fn push_once(step: &str) -> String {
    let tree = if step == "wrong_tree" {
        variable(WRONG_TREE)
    } else {
        variable(TREE)
    };
    let request = serde_json::json!({"version": 1, "revision": variable(REVISION), "tree": tree});
    let bundle = std::fs::read(variable(BUNDLE)).unwrap();
    let mut pipe = match open_pipe(&variable(PUBLICATION_VARIABLE)) {
        Ok(pipe) => pipe,
        Err(error) => return format!("error {}", error.raw_os_error().unwrap_or(-1)),
    };
    let sent = send(&mut pipe, &request, &bundle);
    let reply = read_line(&mut pipe);
    match (sent, reply) {
        (_, Some(line)) => describe(&line),
        (Err(error), None) => format!("send failed: {error}"),
        (Ok(()), None) => "no reply".into(),
    }
}

/// Writes the request line, the big-endian length and the bundle.
fn send(pipe: &mut File, request: &serde_json::Value, bundle: &[u8]) -> std::io::Result<()> {
    pipe.write_all(format!("{request}\n").as_bytes())?;
    pipe.write_all(&(bundle.len() as u64).to_be_bytes())?;
    pipe.write_all(bundle)
}

/// One reply line, or `None` if the broker sent none.
fn read_line(pipe: &mut File) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while pipe.read_exact(&mut byte).is_ok() {
        if byte[0] == b'\n' {
            return String::from_utf8(line).ok();
        }
        line.push(byte[0]);
    }
    None
}

/// A reply line as the report says it.
fn describe(line: &str) -> String {
    let reply: serde_json::Value = serde_json::from_str(line).unwrap();
    match reply["ok"].as_bool() {
        Some(true) => format!(
            "accepted {} {}",
            reply["reference"].as_str().unwrap(),
            reply["revision"].as_str().unwrap()
        ),
        _ => format!("refused {}", reply["code"].as_str().unwrap_or("?")),
    }
}
