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

/// The supervisor's `prepare` carries the digest of the coordinator
/// credential its plan gives a launch, the runner journals it and admits
/// the launch with it, and the publication broker's secret scan refuses
/// the candidate that adds that credential; the same candidate from a
/// launch given no credential is published.
#[test]
fn a_credential_the_supervisor_gives_a_launch_is_refused_by_the_brokers_scan() {
    in_a_new_session("digests");
}

/// An implementer launch, a reviewer launch and the reviewer's candidate,
/// each made by a runner, read a control file and list a control directory
/// from inside, and are refused a protected key file and the publication
/// broker's work directory; the implementer, once its push is accepted, is
/// refused its own repository there. A development check: the runner and
/// the broker run in the test's process, as this account.
#[test]
fn role_launches_reach_their_controls_and_are_refused_the_key_and_the_brokers_work() {
    if roles::equipped() {
        in_a_new_session("roles");
    }
}

/// Task f71179f5, owner-attended: the same three probes through the
/// installed `agentc-runner` service, whose launches run as its own
/// account and are admitted by the installed brokers, against the push key
/// and the installed publication broker's work directory.
#[test]
#[ignore = "owner-attended: needs the installed runner and broker services"]
fn the_installed_runners_role_launches_are_refused_the_push_key_and_the_brokers_work() {
    eprintln!("{}", roles::installed());
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
        "digests" => digests::sent_and_refused(),
        "roles" => roles::host(),
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

mod digests {
    //! The credential digests' whole path: the supervisor's remote host,
    //! a runner in this process, the publication broker and its scan.
    use super::*;
    use agentc_supervisor::config::Config;
    use agentc_supervisor::host_runtime::windows::Planner;
    use agentc_supervisor::host_runtime::windows::egress_admission::BrokerRoute;
    use agentc_supervisor::host_runtime::windows::launcher::LaunchSpec;
    use agentc_supervisor::host_runtime::windows::process::Command as Planned;
    use agentc_supervisor::host_runtime::windows::remote::{Link, RemoteHost};
    use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
    use agentc_supervisor::host_runtime::{HostRuntime, Observation};
    use agentc_supervisor::run_loop::{Launch as Loop, Suggestion};
    use serde_json::{Value, json};
    use std::time::Instant;
    use support::{CHILD, LAUNCH_PATIENCE, REPORT, ROLE};

    /// The variable a launch's coordinator credential is given in.
    const CREDENTIAL: &str = "AGENT_COORDINATOR_TOKEN";

    /// The supervisor's link straight into a runner in this process.
    pub(super) struct Direct(pub(super) Runner);

    impl Link for Direct {
        /// Has the runner handle `request` as its pipe would.
        fn ask(&mut self, request: &Value) -> anyhow::Result<Value> {
            let parsed = serde_json::from_value::<Request>(request.clone());
            let parsed = parsed.map_err(anyhow::Error::from);
            Ok(answer(parsed.and_then(|request| self.0.handle(request))))
        }
    }

    /// Plans the push client for every launch, sending the leaking
    /// candidate, and gives the launch `given` names [`LEAKED_TOKEN`] as
    /// its coordinator credential.
    struct Pushing {
        root: PathBuf,
        tools: PathBuf,
        variables: Vec<(&'static str, String)>,
        given: uuid::Uuid,
    }

    impl Pushing {
        /// The launch's working directory.
        fn work(&self, launch: &Loop) -> PathBuf {
            self.root.join(format!("{}-work", launch.session_id))
        }

        /// What the client reads besides the bundle's variables.
        fn environment(&self, launch: &Loop) -> Vec<(&'static str, String)> {
            let (root, work) = (std::env::var("SystemRoot").unwrap(), self.work(launch));
            let mut environment = vec![
                ("PATH", format!("{};{root}\\System32", self.tools.display())),
                ("SystemRoot", root),
                ("LOCALAPPDATA", work.display().to_string()),
                (ROLE, "client".to_owned()),
                (REPORT, work.join("report.txt").display().to_string()),
                (PROBE, "push".to_owned()),
                (STEPS, "good".to_owned()),
            ];
            if launch.session_id == self.given {
                environment.push((CREDENTIAL, LEAKED_TOKEN.to_owned()));
            }
            environment
        }
    }

    impl Planner for Pushing {
        /// The client in a less-privileged boundary of the launch's own.
        fn plan(&self, launch: &Loop) -> anyhow::Result<(LaunchSpec, Planned)> {
            let work = self.work(launch);
            let limits = json!({"processes": 16, "process_memory_mb": 512, "job_memory_mb": 1024});
            let spec = LaunchSpec {
                profile: format!("agentc.test.{}", launch.session_id.simple()),
                less_privileged: true,
                capabilities: vec!["registryRead".into()],
                limits: serde_json::from_value(limits)?,
                control: self.root.join(format!("{}-control", launch.session_id)),
                work: work.clone(),
            };
            let program = self.tools.join("client.exe").display().to_string();
            let variables = self.environment(launch).into_iter();
            let variables = variables.chain(self.variables.clone());
            let command = Planned {
                argv: vec![program, CHILD.into(), "--exact".into(), "--ignored".into()],
                directory: work.clone(),
                environment: variables.map(|(k, v)| (k.into(), v)).collect(),
                output: work.join("client.log"),
            };
            Ok((spec, command))
        }

        /// The credential the plan put in the launch's environment.
        fn coordinator_credentials(&self, launch: &Loop) -> anyhow::Result<Vec<String>> {
            let given = launch.session_id == self.given;
            Ok(given.then(|| LEAKED_TOKEN.to_owned()).into_iter().collect())
        }
    }

    /// A launch of the supervisor's loop working on [`TASK`].
    fn planned() -> Loop {
        let suggestion = Suggestion {
            task: TASK.into(),
            revision: 1,
            title: String::new(),
        };
        Loop::plan(&Config::default(), "p", suggestion)
    }

    /// Starts the prepared `launch`, waits for its client and returns what
    /// it reported for its one push; `work` is the launch's directory.
    fn pushed(host: &mut impl HostRuntime, work: &Path, launch: &Loop) -> String {
        host.start(launch).unwrap();
        let started = Instant::now();
        while host.observe(launch) == Observation::Running {
            assert!(
                started.elapsed() < LAUNCH_PATIENCE,
                "the client never ended"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let said = std::fs::read_to_string(work.join("report.txt")).unwrap_or_default();
        let log = std::fs::read_to_string(work.join("client.log")).unwrap_or_default();
        let lines = said.strip_prefix("ok: ");
        let lines = lines.unwrap_or_else(|| panic!("{said}\n{log}"));
        let lines: Vec<String> = lines.lines().map(str::to_owned).collect();
        outcome(&lines, "good").to_owned()
    }

    /// The remote host over a runner that admits launches to `services`'
    /// publication broker, planning with `planner`.
    fn host(trees: &Trees, services: &Services, planner: Pushing) -> RemoteHost<Pushing, Direct> {
        let route = BrokerRoute::publication(&services.control, &me()).unwrap();
        let runner = Runner::new(&trees.at("state")).unwrap();
        RemoteHost::new(planner, Direct(runner.with_publication(Some(route))))
    }

    /// Has the supervisor's authority hold `launch` on [`TASK`].
    fn hold(services: &Services, launch: &Loop) {
        let id = launch.session_id.to_string();
        let held = Held {
            task: TASK.into(),
            attempt: format!("attempt-{id}"),
            generation: 1,
        };
        services.authority.hold(&id, held);
    }

    /// The planner of the test's launches, of which `given` is given the
    /// credential; the bundle is read from the tool directory.
    fn planner(trees: &Trees, repository: &Repository, given: &Loop) -> Pushing {
        let tools = trees.at("tools");
        let mut variables = repository.leak_variables();
        variables[0].1 = tools.join("leak.bundle").display().to_string();
        std::fs::create_dir_all(trees.at("launches")).unwrap();
        Pushing {
            root: trees.at("launches"),
            tools,
            variables,
            given: given.session_id,
        }
    }

    /// The whole test.
    pub fn sent_and_refused() -> String {
        let dir = tempfile::tempdir().unwrap();
        let (trees, staging) = (Trees::new(&dir), dir.path().join("staging"));
        std::fs::create_dir(&staging).unwrap();
        let repository = Repository::new(&trees, &staging);
        let services = Services::start(&trees, &repository.remote);
        let (given, plain) = (planned(), planned());
        let planner = planner(&trees, &repository, &given);
        let works = [&given, &plain].map(|launch| planner.work(launch));
        let mut host = host(&trees, &services, planner);
        let packages = [&given, &plain].map(|launch| {
            hold(&services, launch);
            host.prepare(launch).unwrap().container.unwrap()
        });
        let tools = client_tools(&trees, &packages.each_ref().map(String::as_str));
        std::fs::copy(&repository.leak.bundle, tools.join("leak.bundle")).unwrap();
        let refused = pushed(&mut host, &works[0], &given);
        assert_eq!(refused, "refused secret_detected", "the launch given it");
        let accepted = pushed(&mut host, &works[1], &plain);
        let own = format!("{TASK}/{}", plain.session_id);
        let expected = format!(
            "accepted refs/agent-coordinator/candidates/{own} {}",
            repository.leak.commit
        );
        assert_eq!(accepted, expected, "the launch given no credential");
        for launch in [&given, &plain] {
            host.cleanup(launch).unwrap();
        }
        format!("given: {refused}; given none: {accepted}")
    }
}

mod roles {
    //! An implementer launch, a reviewer launch and the reviewer's
    //! candidate, each made by a runner, run the probe fixture's `brokers`
    //! mode (`deploy/agentc/windows-probe-fixture`): after two reads that
    //! must work inside the launch, a protected key and the publication
    //! broker's work directory must each be refused. The candidate's probe
    //! is a command the reviewer hands its prefix script, so the candidate
    //! runner makes it. [`host`] is the development check, with the runner
    //! and the broker in this process as this account; [`installed`] drives
    //! the installed runner service, whose launches run as its own account.
    use super::*;
    use agentc_supervisor::candidate::form::{git_bash_path, recorded};
    use agentc_supervisor::candidate::shim::SCRIPT_NAME;
    use agentc_supervisor::host_runtime::windows::egress_admission::BrokerRoute;
    use agentc_supervisor::host_runtime::windows::reader::create_directory;
    use agentc_supervisor::host_runtime::windows::remote::{Link, PipeLink};
    use agentc_supervisor::host_runtime::windows::runner::Runner;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    /// Names the probe fixture's executable for the development check.
    pub const FIXTURE: &str = "AGENTC_TEST_PROBE_FIXTURE";
    /// Names the pinned POSIX shell's file for the development check.
    pub const SHELL: &str = "AGENTC_TEST_POSIX_SHELL";
    /// The three probes, in the order they run.
    const ROLES: [&str; 3] = ["implementer", "reviewer", "candidate"];

    /// Where one run's files are.
    struct Layout {
        /// The launches' directories are made below it by the runner.
        root: PathBuf,
        /// `fixture.exe` and `sh.exe`, which every launch may read.
        tools: PathBuf,
        /// The supervisor program a reviewer runs as its shim.
        shim: PathBuf,
        /// The fixture's spec.
        spec: PathBuf,
        /// Where each probe leaves its rows; every launch may write there.
        drop: PathBuf,
        /// What this run's launches and files are named after.
        tag: String,
    }

    /// `path` with forward slashes, as a shell script spells it.
    fn slashed(path: &Path) -> String {
        path.display().to_string().replace('\\', "/")
    }

    impl Layout {
        /// The id of the run's launch `name`.
        fn launch(&self, name: &str) -> String {
            format!("f7-{}-{name}", self.tag)
        }

        /// The directory `kind` of the run's launch `name`.
        fn directory(&self, name: &str, kind: &str) -> PathBuf {
            self.root.join(format!("{}-{kind}", self.launch(name)))
        }

        /// A less-privileged launch spec for `name`. The implementer has no
        /// capability, as a stand-in launch has none; the reviewer and its
        /// candidate have `registryRead`, without which the pinned shell
        /// does not start (`WSAStartup failed, error 18`).
        fn launch_spec(&self, name: &str) -> Value {
            let shell = if name == "implementer" {
                vec![]
            } else {
                vec!["registryRead"]
            };
            let limits = json!({"processes": 16, "process_memory_mb": 512, "job_memory_mb": 1024});
            json!({"profile": format!("agentc.test.{}", uuid::Uuid::new_v4().simple()),
                   "less_privileged": true, "capabilities": shell, "limits": limits,
                   "control": self.directory(name, "control"),
                   "work": self.directory(name, "work")})
        }

        /// The reviewer's candidate: its own launch, the pinned shell and
        /// the shim.
        fn candidate_spec(&self) -> Value {
            let system = std::env::var("SystemRoot").unwrap() + r"\System32";
            json!({"launch": self.launch_spec("candidate"),
                   "source": self.directory("candidate", "source"),
                   "shell": self.tools.join("sh.exe"), "shim": self.shim,
                   "reviewer_temp": self.directory("reviewer", "work").join("tmp"),
                   "path": [self.tools, system]})
        }

        /// The environment of a launch's root process working in `work`.
        fn environment(&self, work: &Path) -> Value {
            let root = std::env::var("SystemRoot").unwrap();
            json!({"PATH": format!("{};{root}\\System32", self.tools.display()),
                   "SystemRoot": root, "LOCALAPPDATA": work})
        }

        /// The fixture's arguments for the probe of `role`, as shell words.
        /// The probe is named as the role's launch is, so the implementer
        /// finds its own repository below the broker's work directory.
        fn probe_words(&self, role: &str) -> String {
            let (spec, drop) = (slashed(&self.spec), slashed(&self.drop));
            let name = self.launch(role);
            format!("fixture.exe brokers \"{spec}\" \"{drop}\" {name}")
        }

        /// The file holding the argument the reviewer hands its prefix
        /// script, in the form the harness records.
        fn argument_file(&self) -> PathBuf {
            self.drop.join(format!("{}-argument", self.tag))
        }

        /// Writes [`Layout::argument_file`]: the candidate's probe.
        fn write_argument(&self) {
            let temp = self.directory("reviewer", "work").join("tmp");
            let temp = temp.display().to_string();
            let inside = format!("{}/claude-1a2b-cwd", git_bash_path(&temp).unwrap());
            let command = format!("'{}'", self.probe_words("candidate"));
            let argument = recorded("/c/none/snapshot.sh", &temp, &command, &inside);
            std::fs::write(self.argument_file(), argument).unwrap();
        }

        /// What the reviewer's shell runs: its own probe, then the
        /// candidate's through the prefix script the runner wrote.
        fn reviewer_script(&self) -> String {
            let prefix = slashed(&self.directory("reviewer", "control").join(SCRIPT_NAME));
            let argument = slashed(&self.argument_file());
            format!(
                "mkdir -p tmp; {}; echo \"reviewer=$?\"; \
                 \"{prefix}\" \"$(cat '{argument}')\"; echo \"candidate=$?\"",
                self.probe_words("reviewer")
            )
        }

        /// The rows the probe of `role` left, by key.
        fn rows(&self, role: &str) -> BTreeMap<String, String> {
            let path = self.drop.join(format!("{}.txt", self.launch(role)));
            let text = std::fs::read_to_string(&path);
            let text = text.unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let pairs = text.lines().filter_map(|line| line.split_once('='));
            pairs.map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
        }
    }

    /// Sends `request` and returns the runner's answer, which must be `ok`.
    fn ok(link: &mut dyn Link, request: Value) -> Value {
        let answer = link.ask(&request).unwrap();
        assert_eq!(answer["ok"], true, "{request} -> {answer}");
        answer
    }

    /// Prepares the implementer and the reviewer with its candidate and
    /// returns the three packages.
    fn prepare(link: &mut dyn Link, layout: &Layout, task: &str) -> [String; 3] {
        let implementer = json!({"op": "prepare", "launch": layout.launch("implementer"),
            "role": "implementer", "task": task, "spec": layout.launch_spec("implementer")});
        let reviewer = json!({"op": "prepare", "launch": layout.launch("reviewer"),
            "role": "reviewer", "task": task, "spec": layout.launch_spec("reviewer"),
            "candidate": layout.candidate_spec()});
        let (first, second) = (ok(link, implementer), ok(link, reviewer));
        let sid = |answer: &Value, name: &str| answer[name].as_str().unwrap().to_owned();
        [
            sid(&first, "package_sid"),
            sid(&second, "package_sid"),
            sid(&second, "candidate_package_sid"),
        ]
    }

    /// Starts `argv` as the root of the launch `name`, waits for it to end
    /// and returns its exit code and what it printed.
    fn run(link: &mut dyn Link, layout: &Layout, name: &str, argv: Value) -> (u64, String) {
        let (launch, work) = (layout.launch(name), layout.directory(name, "work"));
        let log = layout.drop.join(format!("{launch}.log"));
        let start = json!({"op": "start", "launch": launch, "argv": argv, "directory": work,
            "environment": layout.environment(&work), "output": log});
        ok(link, start);
        let seen = ok(
            link,
            json!({"op": "observe", "launch": launch, "wait_ms": 120_000}),
        );
        let said = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(seen["state"], "exited", "{seen}: {said}");
        (seen["code"].as_u64().unwrap(), said)
    }

    /// Runs the three probes and removes the two launches.
    fn probe(link: &mut dyn Link, layout: &Layout) {
        layout.write_argument();
        let fixture = layout.tools.join("fixture.exe");
        let words = [
            "brokers".to_owned(),
            layout.spec.display().to_string(),
            layout.drop.display().to_string(),
            layout.launch("implementer"),
        ];
        let argv = json!([fixture, words[0], words[1], words[2], words[3]]);
        let (code, said) = run(link, layout, "implementer", argv);
        assert_eq!(code, 0, "the implementer's probe: {said}");
        let ready = json!({"op": "candidate_ready", "launch": layout.launch("reviewer")});
        let ready = ok(link, ready);
        assert_eq!(ready["runner"], "ready", "{ready}");
        let shell = layout.tools.join("sh.exe");
        let argv = json!([shell, "-c", layout.reviewer_script()]);
        let (code, said) = run(link, layout, "reviewer", argv);
        let both = said.contains("reviewer=0") && said.contains("candidate=0");
        assert!(code == 0 && both, "the reviewer's script: {said}");
        for name in ["implementer", "reviewer"] {
            let launch = layout.launch(name);
            ok(
                link,
                json!({"op": "cleanup", "launch": launch, "remove_directories": true}),
            );
        }
    }

    /// Checks one role's rows and returns them as one line: it runs in a
    /// less-privileged AppContainer in a job, both controls worked, and
    /// every target it was told to try was refused with error 5.
    fn check(layout: &Layout, role: &str) -> String {
        let rows = layout.rows(role);
        let row = |key: &str| rows.get(key).map(String::as_str).unwrap_or("missing");
        for control in ["control.file_read", "control.dir_list"] {
            assert_eq!(row(control), "ok", "{role} {control}: {rows:?}");
        }
        assert_eq!(row("token.appcontainer"), "ok", "{role}: {rows:?}");
        assert_eq!(row("token.lpac"), "value:yes", "{role}: {rows:?}");
        let refused: Vec<_> = rows
            .iter()
            .filter(|(k, _)| k.starts_with("refused."))
            .collect();
        assert!(refused.len() >= 2, "{role} tried {refused:?}");
        for (target, outcome) in refused {
            assert_eq!(outcome, "error:5", "{role} {target}: {rows:?}");
        }
        let shown: Vec<String> = rows.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{role}: {}", shown.join(" "))
    }

    /// A directory `packages` may read and, when `write`, change, besides
    /// this account and the system.
    fn shared(path: &Path, packages: &[String], write: bool) {
        let rights = if write { "0x1301bf" } else { "0x1200a9" };
        let mut list = format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)", me());
        for package in packages {
            list.push_str(&format!("(A;OICI;{rights};;;{package})"));
        }
        create_directory(path, &list).unwrap();
    }

    /// Fills the development check's tool directory: the fixture, the
    /// shell, the shim, a control file, the bundle and the spec, whose
    /// refused targets are a key file and the broker's work directory.
    fn fill_tools(layout: &Layout, trees: &Trees, repository: &Repository) {
        let copy = |from: PathBuf, name: &str| {
            std::fs::copy(&from, layout.tools.join(name))
                .unwrap_or_else(|error| panic!("copy {}: {error}", from.display()));
        };
        copy(std::env::var_os(FIXTURE).unwrap().into(), "fixture.exe");
        copy(std::env::var_os(SHELL).unwrap().into(), "sh.exe");
        copy(
            env!("CARGO_BIN_EXE_agentc-supervisor").into(),
            "agentc-supervisor.exe",
        );
        copy(repository.bundle.clone(), "candidate.bundle");
        std::fs::write(layout.tools.join("control.txt"), b"control").unwrap();
        let (tools, work) = (layout.tools.display(), trees.at("broker"));
        let spec = format!(
            "control_file={tools}\\control.txt\ncontrol_dir={tools}\n\
             refused_file.push_key={}\nrefused_dir.publication_work={}\n\
             bundle={tools}\\candidate.bundle\nrevision={}\ntree={}\nwork_root={}\n",
            trees.at("key").join("push-app.pem").display(),
            work.display(),
            repository.candidate,
            repository.tree,
            work.display(),
        );
        std::fs::write(&layout.spec, spec).unwrap();
    }

    /// The development check's layout below `trees`.
    fn layout(trees: &Trees) -> Layout {
        let tools = trees.at("tools");
        std::fs::create_dir_all(trees.at("launches")).unwrap();
        Layout {
            root: trees.at("launches"),
            shim: tools.join("agentc-supervisor.exe"),
            spec: tools.join("spec.txt"),
            tools,
            drop: trees.at("drop"),
            tag: format!("{}", std::process::id()),
        }
    }

    /// Whether the development check's two programs are named; says so
    /// where they are not, and nothing is checked there.
    pub fn equipped() -> bool {
        let named = [FIXTURE, SHELL].map(|name| std::env::var_os(name).is_some());
        if named != [true, true] {
            eprintln!("{FIXTURE} or {SHELL} is unset; the role probes were not checked");
        }
        named == [true, true]
    }

    /// The development check: the runner and the publication broker are in
    /// this process, as this account, and the key is a file of the test's.
    pub fn host() -> String {
        let dir = tempfile::tempdir().unwrap();
        let (trees, staging) = (Trees::new(&dir), dir.path().join("staging"));
        std::fs::create_dir(&staging).unwrap();
        let repository = Repository::new(&trees, &staging);
        shared(&trees.at("broker"), &[], false);
        let services = Services::start(&trees, &repository.remote);
        shared(&trees.at("key"), &[], false);
        std::fs::write(trees.at("key").join("push-app.pem"), b"key").unwrap();
        let layout = layout(&trees);
        let route = BrokerRoute::publication(&services.control, &me()).unwrap();
        let runner = Runner::new(&trees.at("state")).unwrap();
        let mut link = digests::Direct(runner.with_publication(Some(route)));
        let packages = prepare(&mut link, &layout, TASK);
        shared(&layout.tools, &packages, false);
        shared(&layout.drop, &packages, true);
        fill_tools(&layout, &trees, &repository);
        let held = Held {
            task: TASK.into(),
            attempt: "attempt-roles".into(),
            generation: 1,
        };
        services.authority.hold(&layout.launch("implementer"), held);
        probe(&mut link, &layout);
        let pushed = layout.rows("implementer");
        let accepted = pushed["publication.push"].contains("\"ok\":true");
        assert!(accepted, "the implementer's push: {pushed:?}");
        for row in ["repository.dir_list", "repository.head_read"] {
            assert_eq!(pushed[row], "error:5", "{row}: {pushed:?}");
        }
        ROLES.map(|role| check(&layout, role)).join(" || ")
    }

    /// The value of the variable `name`, which the runbook sets.
    fn given(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }

    /// Owner-attended: the installed runner service makes the launches,
    /// which run as its account, and the installed brokers admit them. The
    /// runbook installs the tools and the spec, whose refused targets are
    /// the push key and the publication broker's work directory, and sets
    /// the `AGENTC_F7_*` variables.
    pub fn installed() -> String {
        let (runner, tools) = (given("AGENTC_F7_RUNNER_SID"), given("AGENTC_F7_TOOLS"));
        let layout = Layout {
            root: given("AGENTC_F7_LAUNCH_ROOT").into(),
            shim: given("AGENTC_F7_SHIM").into(),
            spec: given("AGENTC_F7_SPEC").into(),
            tools: tools.into(),
            drop: given("AGENTC_F7_DROP").into(),
            tag: format!("{}", std::process::id()),
        };
        let mut link = PipeLink::new(&given("AGENTC_F7_RUNNER_PIPE"), &runner);
        prepare(&mut link, &layout, &given("AGENTC_F7_TASK"));
        probe(&mut link, &layout);
        let said = ROLES.map(|role| {
            let account = layout.rows(role)["token.user_sid"].clone();
            assert_eq!(account, format!("value:{runner}"), "{role} runs as");
            check(&layout, role)
        });
        let implementer = layout.rows("implementer");
        let allowed = &implementer["egress.allowed"];
        assert!(allowed.contains(" 200"), "egress: {implementer:?}");
        said.join(" || ")
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
