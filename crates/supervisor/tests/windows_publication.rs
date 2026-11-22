//! The publication broker with real AppContainer launches (phase 6, steps
//! 6c and 6d).
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
    Admission, Control, ControlClient, Opened, control_pipe_name,
};
use agentc_supervisor::host_runtime::windows::egress_admission::PUBLICATION_VARIABLE;
use agentc_supervisor::host_runtime::windows::launcher::Launcher;
use agentc_supervisor::host_runtime::windows::publication_broker::{Publication, SERVICE};
use agentc_supervisor::host_runtime::windows::token;
use agentc_supervisor::service::authority::{ACTIVE, AttemptReader, AttemptView, Authority, Held};
use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use sha2::{Digest, Sha256};
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
/// refused; a foreign launch cannot open another launch's pipe; once the
/// launch is closed its pipe is gone; and a candidate adding a credential
/// the launch was given is refused by the secret scan.
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

/// The runner refuses to start an implementer launch while its publication
/// broker is missing and gives back the launch's egress admission, starts it
/// with its pipe once the broker serves, and never offers a reviewer one.
#[test]
fn the_runner_starts_an_implementer_only_once_the_publication_broker_admits_it() {
    in_a_new_session("runner");
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session or inside a launch.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    child_main(|role| match role {
        "publication" => publication(),
        "lost" => lost(),
        "runner" => runner::admitted_starts(),
        "installed" => installed::host(),
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
/// whose only ref is [`BUNDLED_REF`]; beside it a second candidate on the
/// base that adds a line holding [`LEAKED_TOKEN`].
struct Repository {
    remote: PathBuf,
    bundle: PathBuf,
    candidate: String,
    tree: String,
    wrong_tree: String,
    leak: Bundled,
}

/// A candidate bundle, its commit and the commit's tree.
struct Bundled {
    bundle: PathBuf,
    commit: String,
    tree: String,
}

/// A coordinator token as a launch might be given one: 64 hex digits, the
/// form whose digest the secret scan checks.
const LEAKED_TOKEN: &str = "5ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7";

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
        bundle_of(&checkout, (&candidate, &base), &bundle);
        let tree_of = |commit: &str| git(&checkout, &["rev-parse", &format!("{commit}^{{tree}}")]);
        let (tree, wrong_tree) = (tree_of(&candidate), tree_of(&base));
        git(&checkout, &["checkout", "--quiet", "--detach", &base]);
        let leaked = commit_text(&checkout, "notes.txt", &format!("token {LEAKED_TOKEN}\n"));
        let leak = Bundled {
            bundle: tools.join("leak.bundle"),
            tree: tree_of(&leaked),
            commit: leaked,
        };
        bundle_of(&checkout, (&leak.commit, &base), &leak.bundle);
        Self {
            remote,
            bundle,
            candidate,
            tree,
            wrong_tree,
            leak,
        }
    }

    /// What a launch needs to send the leaking candidate.
    fn leak_variables(&self) -> Vec<(&'static str, String)> {
        vec![
            (BUNDLE, self.leak.bundle.display().to_string()),
            (REVISION, self.leak.commit.clone()),
            (TREE, self.leak.tree.clone()),
            (WRONG_TREE, self.wrong_tree.clone()),
        ]
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

/// Writes a bundle of `commit` over `base` to `path`, carrying only
/// [`BUNDLED_REF`].
fn bundle_of(checkout: &Path, (commit, base): (&str, &str), path: &Path) {
    git(checkout, &["update-ref", BUNDLED_REF, commit]);
    let (path, excluded) = (path.to_str().unwrap(), format!("^{base}"));
    git(
        checkout,
        &["bundle", "create", path, BUNDLED_REF, &excluded],
    );
}

/// Commits a file named `name` holding its name and returns the commit.
fn commit(checkout: &Path, name: &str) -> String {
    commit_text(checkout, name, name)
}

/// Commits a file named `name` holding `text` and returns the commit.
fn commit_text(checkout: &Path, name: &str, text: &str) -> String {
    std::fs::write(checkout.join(name), text).unwrap();
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
        self.admit_with(launch, role, &[])
    }

    /// As [`Services::admit`], for a launch given the credentials
    /// `known_digests` are of.
    fn admit_with(&self, launch: &Launch, role: Role, known_digests: &[String]) -> Opened {
        let admission = Admission {
            launch: &launch.id,
            role,
            package: &launch.package,
            generation: 1,
            task: Some(TASK),
            known_digests,
        };
        self.control().admit(&admission).unwrap()
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
    push_with(
        launcher,
        (launch, tools),
        opened,
        repository.variables(),
        steps,
    )
}

/// Runs the `push` probe in `launch` with `steps`, sending what
/// `variables` name, and returns its report.
fn push_with(
    launcher: &mut Launcher,
    (launch, tools): (&Launch, &Path),
    opened: &Opened,
    mut variables: Vec<(&'static str, String)>,
    steps: &str,
) -> Vec<String> {
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
    let ids = ["a", "b", "rev", "foreign", "leak"].map(id);
    let launches = ids
        .each_ref()
        .map(|name| prepare(&mut launcher, &trees, name));
    let packages = launches.each_ref().map(|launch| launch.package.as_str());
    let tools = client_tools(&trees, &packages);
    let repository = Repository::new(&trees, &tools);
    let services = Services::start(&trees, &repository.remote);
    let [own, unheld, reviewer, foreign, leaker] = &launches;
    let said = [
        authorized(&mut launcher, &services, (own, &tools), &repository),
        unauthorized(&mut launcher, &services, (unheld, &tools), &repository),
        reviewing(&mut launcher, &services, (reviewer, &tools), &repository),
        foreign_open(&mut launcher, &services, (foreign, own), &tools),
        leaking(&mut launcher, &services, (leaker, &tools), &repository),
    ];
    check_refs(&repository, own);
    let work = trees.at("broker");
    assert!(!work.join(&own.id).exists(), "a closed launch's repository");
    let open = work.join(&leaker.id).join("HEAD");
    assert!(open.exists(), "an open launch's repository went");
    drop(services);
    let left: Vec<_> = std::fs::read_dir(&work).unwrap().flatten().collect();
    assert!(left.is_empty(), "the stopped broker left {left:?}");
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

/// An implementer launch with authority, admitted with the digest of the
/// credential it was given, whose candidate adds that credential: the
/// secret scan refuses it and nothing is published.
fn leaking(
    launcher: &mut Launcher,
    services: &Services,
    (launch, tools): (&Launch, &Path),
    repository: &Repository,
) -> String {
    services.hold(launch);
    let digest = hex::encode(Sha256::digest(LEAKED_TOKEN.as_bytes()));
    let opened = services.admit_with(launch, Role::Implementer, &[digest]);
    let variables = repository.leak_variables();
    let lines = push_with(launcher, (launch, tools), &opened, variables, "good");
    let refused = outcome(&lines, "good");
    assert_eq!(refused, "refused secret_detected", "{lines:?}");
    lines.join(", ")
}

/// A launch opening another launch's pipe is denied; once that launch is
/// closed its pipe is gone even for this account, and the publication
/// test then finds its private repository removed.
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
    let pushed = tools.parent().unwrap().join("broker").join(&own.id);
    assert!(pushed.join("HEAD").exists(), "the launch's repository");
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
        "read" => match File::open(variable(TARGET)) {
            Ok(_) => "read = opened".into(),
            Err(error) => format!("read = error {}", error.raw_os_error().unwrap_or(-1)),
        },
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

// ---- The runner's side ----

mod runner {
    use super::*;
    use agentc_supervisor::host_runtime::windows::broker::{Grant, Halt, Handler, pipe_name};
    use agentc_supervisor::host_runtime::windows::egress_admission::BrokerRoute;
    use agentc_supervisor::host_runtime::windows::pipe::Session;
    use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
    use serde_json::{Value, json};

    /// A broker whose launches never connect in this test.
    struct Unused;

    impl Handler for Unused {
        fn serve(&self, _: &Grant, _: &mut Session<'_>, _: &Halt) -> anyhow::Result<()> {
            anyhow::bail!("no launch connects in this test")
        }
    }

    /// Serves a broker `service` with no live handler on the control pipe
    /// `name` until dropped.
    fn broker(service: &str, name: &str) -> Serving {
        let broker = Broker::new(service, &me(), Arc::new(Unused)).unwrap();
        let mut control = Control::new(name, broker, &me(), None).unwrap();
        Serving::start(move |stop| control.serve_one(Duration::from_millis(100), stop).unwrap())
    }

    /// Sends one request and returns the runner's answer line.
    fn ask(runner: &mut Runner, request: Value) -> Value {
        let parsed = serde_json::from_value::<Request>(request).map_err(anyhow::Error::from);
        answer(parsed.and_then(|request| runner.handle(request)))
    }

    /// `ask`, failing unless the runner answered `ok`.
    fn ok(runner: &mut Runner, request: Value) -> Value {
        let reply = ask(runner, request.clone());
        assert_eq!(reply["ok"], true, "{request} -> {reply}");
        reply
    }

    /// The `prepare` request of `launch` working as `role` on [`TASK`].
    fn prepare(root: &Path, launch: &str, role: &str) -> Value {
        std::fs::create_dir_all(root.join(launch)).unwrap();
        let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
        let limits = json!({"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512});
        json!({"op": "prepare", "launch": launch, "role": role, "task": TASK,
               "spec": {"profile": profile, "less_privileged": true,
                        "capabilities": ["registryRead"], "limits": limits,
                        "control": root.join(launch).join("control"),
                        "work": root.join(launch).join("work")}})
    }

    /// The `start` request of `launch`, printing its publication pipe.
    fn start(root: &Path, launch: &str) -> Value {
        let system = std::env::var("SystemRoot").unwrap();
        let cmd = format!("{system}\\System32\\cmd.exe");
        let script = format!("echo pipe=%{PUBLICATION_VARIABLE}%");
        let work = root.join(launch).join("work");
        json!({"op": "start", "launch": launch, "argv": [cmd, "/d", "/c", script],
               "directory": work, "output": root.join(format!("{launch}.log")),
               "environment": {"SystemRoot": system, "LOCALAPPDATA": work}})
    }

    /// Starts `launch`, waits for it to end and returns its log.
    fn run(runner: &mut Runner, root: &Path, launch: &str) -> String {
        ok(runner, start(root, launch));
        let wait = json!({"op": "observe", "launch": launch, "wait_ms": 30_000});
        assert_eq!(ok(runner, wait)["state"], "exited");
        ok(runner, json!({"op": "terminate", "launch": launch}));
        std::fs::read_to_string(root.join(format!("{launch}.log"))).unwrap()
    }

    /// The whole test.
    pub fn admitted_starts() -> String {
        let dir = tempfile::tempdir().unwrap();
        let (root, unique) = (dir.path(), uuid::Uuid::new_v4().simple());
        let names = [
            format!("agentc-test-egress-{unique}"),
            format!("agentc-test-pub-{unique}"),
        ];
        let _egress = broker("egress", &names[0]);
        let routes = (
            BrokerRoute::new(&names[0], &me()),
            BrokerRoute::publication(&names[1], &me()),
        );
        let runner = Runner::new(&root.join("state"))
            .unwrap()
            .with_egress(Some(routes.0.unwrap()));
        let mut runner = runner.with_publication(Some(routes.1.unwrap()));
        let [implementer, reviewer] = ["run-impl", "run-rev"].map(id);
        ok(&mut runner, prepare(root, &implementer, "implementer"));
        let refused = ask(&mut runner, start(root, &implementer)).to_string();
        assert!(
            refused.contains("publication broker") && refused.contains("not started"),
            "{refused}"
        );
        let egress = ControlClient::connect(&names[0], &me())
            .unwrap()
            .close(&implementer);
        assert!(!egress.unwrap(), "the egress admission was kept");
        let _publication = broker("publication", &names[1]);
        let log = run(&mut runner, root, &implementer);
        let expected = pipe_path(&pipe_name(SERVICE, &implementer)).unwrap();
        assert!(log.contains(&format!("pipe={expected}")), "{log}");
        ok(&mut runner, prepare(root, &reviewer, "reviewer"));
        let unoffered = run(&mut runner, root, &reviewer);
        assert!(
            unoffered.contains(&format!("pipe=%{PUBLICATION_VARIABLE}%")),
            "{unoffered}"
        );
        for launch in [&implementer, &reviewer] {
            let cleanup = json!({"op": "cleanup", "launch": launch, "remove_directories": true});
            let cleaned = ok(&mut runner, cleanup);
            assert_eq!(cleaned["publication_detail"], Value::Null, "{cleaned}");
        }
        format!(
            "implementer: {}; reviewer: {}",
            log.trim(),
            unoffered.trim()
        )
    }
}

/// Phase 6d, owner-attended: the installed `agentc-publication` service,
/// under its own virtual service account and with the push App's key,
/// publishes an authorized implementer launch's candidate to the real
/// remote under that launch's own ref, and three further launches
/// (implementer, reviewer and candidate) cannot read the key. This test plays the
/// supervisor (it serves the authority pipe) and the runner, so the
/// service is configured with this account as both. The runbook sets
/// `AGENTC_6D_PUBLICATION_PIPE`, `AGENTC_6D_PUBLICATION_SID`,
/// `AGENTC_6D_AUTHORITY_PIPE`, `AGENTC_6D_CHECKOUT` (a clone of the remote
/// whose `origin/main` the candidate is made on), `AGENTC_6D_KEY` and
/// `AGENTC_6D_TASK`, then deletes the published ref.
#[test]
#[ignore = "owner-attended: needs the installed agentc-publication service and the push key"]
fn the_installed_publication_service_publishes_to_the_real_remote() {
    in_a_new_session("installed");
}

mod installed {
    //! The host's side of the installed-service test.
    use super::*;

    /// One of the runbook's variables.
    fn setting(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }

    /// A candidate commit on the checkout's `origin/main` and its bundle.
    fn candidate(tools: &Path) -> Bundled {
        let checkout = PathBuf::from(setting("AGENTC_6D_CHECKOUT"));
        git(
            &checkout,
            &["checkout", "--quiet", "--detach", "origin/main"],
        );
        let text = format!("Step 6d acceptance candidate {}\n", uuid::Uuid::new_v4());
        let commit = commit_text(&checkout, "daa42f71-acceptance.txt", &text);
        let bundle = tools.join("acceptance.bundle");
        bundle_of(&checkout, (&commit, "origin/main"), &bundle);
        let tree = git(&checkout, &["rev-parse", &format!("{commit}^{{tree}}")]);
        Bundled {
            bundle,
            commit,
            tree,
        }
    }

    /// The supervisor's authority pipe for the service's account, with
    /// `launch` holding an attempt of `task`.
    fn supervisor(task: &str, launch: &Launch) -> Serving {
        let authority = Arc::new(Authority::new(Box::new(Reads(Arc::default()))));
        let held = Held {
            task: task.into(),
            attempt: format!("attempt-{}", launch.id),
            generation: 1,
        };
        authority.hold(&launch.id, held);
        let (pipe, broker) = (
            setting("AGENTC_6D_AUTHORITY_PIPE"),
            setting("AGENTC_6D_PUBLICATION_SID"),
        );
        let mut server = AuthorityServer::new(&pipe, &me(), &broker, authority, None).unwrap();
        Serving::start(move |stop| server.serve_one(Duration::from_millis(100), stop).unwrap())
    }

    /// The host's side: publish through the service, then probe the key.
    /// A launch starts once, so the push has a launch of its own and the
    /// three key probes one each.
    pub fn host() -> String {
        let dir = tempfile::tempdir().unwrap();
        let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
        let launches = ["pub-push", "pub-impl", "pub-rev", "pub-cand"]
            .map(|name| prepare(&mut launcher, &trees, &id(name)));
        let packages = launches.each_ref().map(|launch| launch.package.as_str());
        let tools = client_tools(&trees, &packages);
        let task = setting("AGENTC_6D_TASK");
        let _supervisor = supervisor(&task, &launches[0]);
        let published = publish(&mut launcher, (&launches[0], &tools), &task);
        let read = launches[1..]
            .iter()
            .map(|launch| read_key(&mut launcher, (launch, &tools)))
            .collect::<Vec<_>>();
        for launch in &launches {
            launcher.cleanup(&launch.id).unwrap();
        }
        format!("{published}; key: {}", read.join(", "))
    }

    /// Admits `launch` working on `task` and has it push the candidate;
    /// the reply must accept it at the launch's own ref.
    fn publish(launcher: &mut Launcher, (launch, tools): (&Launch, &Path), task: &str) -> String {
        let candidate = candidate(tools);
        let (pipe, sid) = (
            setting("AGENTC_6D_PUBLICATION_PIPE"),
            setting("AGENTC_6D_PUBLICATION_SID"),
        );
        let mut control = ControlClient::connect(&pipe, &sid).unwrap();
        let opened = control
            .open_for(
                &launch.id,
                Role::Implementer,
                &launch.package,
                1,
                Some(task),
            )
            .unwrap();
        let variables = vec![
            (BUNDLE, candidate.bundle.display().to_string()),
            (REVISION, candidate.commit.clone()),
            (TREE, candidate.tree.clone()),
            (WRONG_TREE, candidate.tree.clone()),
        ];
        let lines = push_with(launcher, (launch, tools), &opened, variables, "good");
        let reference = format!("refs/agent-coordinator/candidates/{task}/{}", launch.id);
        let accepted = format!("accepted {reference} {}", candidate.commit);
        assert_eq!(outcome(&lines, "good"), accepted, "{lines:?}");
        assert!(control.close(&launch.id).unwrap());
        format!("published {reference} {}", candidate.commit)
    }

    /// Whether `launch` can open the push key for reading.
    fn read_key(launcher: &mut Launcher, (launch, tools): (&Launch, &Path)) -> String {
        let target = vec![(TARGET, setting("AGENTC_6D_KEY"))];
        let report = start_probe(launcher, (launch, tools), "read", (None, target));
        let lines = finish(launcher, launch, &report);
        let read = outcome(&lines, "read").to_owned();
        assert_eq!(read, "error 5", "{}: {lines:?}", launch.id);
        format!("{} {read}", launch.id)
    }
}
