//! The pipes through which AppContainer launches reach a host broker
//! (phase 6, step 6a).
//!
//! A [`Broker`] is one service, such as egress or publication. For each
//! launch it serves, [`Broker::open`] records the launch's [`Grant`] in its
//! registry, keyed by the launch's package SID, and creates the launch's
//! pipe [`pipe_name`] with [`INSTANCES`] instances, each served by its own
//! thread. The broker's account owns the pipe, and only a token of the
//! launch's account that carries exactly the launch's package is admitted
//! ([`Pipe::broker_instances`]). An admitted session goes to the broker's
//! [`Handler`] with the grant the registry holds for the token's package:
//! who the client is comes from Windows and the registry, never from what
//! the client sent.
//!
//! [`Broker::close`] removes the grant, wakes the threads, ends a session
//! that is still going and joins them; the pipe's name is gone once it
//! returns. Opening a launch again gives it a higher generation.
use super::pipe::{Accepted, Pipe, Session};
use super::sys::{Handle, check};
use crate::service::pipe_policy::{Refusal, checked_sid, pipe_path};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};

/// How many clients of one launch a broker serves at once.
pub const INSTANCES: u32 = 4;
/// How long one wait for a client lasts before the thread looks whether its
/// pipe is closing.
const TICK: Duration = Duration::from_millis(500);
/// The longest service name, so a pipe name stays within its limit.
const SERVICE_LIMIT: usize = 32;

/// Which side of a review a launch works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Implementer,
    Reviewer,
}

/// What a broker knows of one launch it serves. It is built by the broker's
/// owner and handed to the handler as is; a client cannot change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    launch: String,
    role: Role,
    account: String,
    package: String,
    generation: u64,
    /// The coordinator task the launch works on, when its owner named one;
    /// the publication broker names the launch's candidate ref after it.
    task: Option<String>,
    /// SHA-256 digests (lowercase hex) of the coordinator credentials the
    /// launch was given, which the publication broker's secret scan
    /// refuses to publish.
    known_digests: Vec<String>,
}

/// The most credential digests one launch's grant may carry.
pub const MAX_KNOWN_DIGESTS: usize = 16;

/// Refuses `digests` unless each is a SHA-256 digest as 64 lowercase hex
/// digits and there are at most [`MAX_KNOWN_DIGESTS`] of them.
pub fn checked_digests(digests: &[String]) -> Result<()> {
    ensure!(
        digests.len() <= MAX_KNOWN_DIGESTS,
        "a launch may carry at most {MAX_KNOWN_DIGESTS} credential digests"
    );
    let digest =
        |d: &String| d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ensure!(
        digests.iter().all(digest),
        "a credential digest is 64 lowercase hexadecimal digits"
    );
    Ok(())
}

impl Grant {
    /// The grant of `launch`, which works as `role`, runs as `account` and
    /// carries `package` (both SID text). `generation` is the least
    /// generation [`Broker::open`] gives it.
    pub fn new(
        launch: &str,
        role: Role,
        account: &str,
        package: &str,
        generation: u64,
    ) -> Result<Self> {
        pipe_path(launch).with_context(|| format!("{launch:?} cannot name a launch"))?;
        Ok(Self {
            launch: launch.to_owned(),
            role,
            account: checked_sid(account)?.to_owned(),
            package: checked_sid(package)?.to_owned(),
            generation,
            task: None,
            known_digests: Vec::new(),
        })
    }

    /// This grant for a launch working on `task`, which must be plain
    /// enough to name a pipe (letters, digits, `-`, `_`, `.`).
    pub fn with_task(mut self, task: &str) -> Result<Self> {
        pipe_path(task).with_context(|| format!("{task:?} cannot name a task"))?;
        self.task = Some(task.to_owned());
        Ok(self)
    }

    /// The task the launch works on, if its owner named one.
    pub fn task(&self) -> Option<&str> {
        self.task.as_deref()
    }

    /// This grant for a launch given the credentials `digests` are of
    /// ([`checked_digests`]).
    pub fn with_known_digests(mut self, digests: Vec<String>) -> Result<Self> {
        checked_digests(&digests)?;
        self.known_digests = digests;
        Ok(self)
    }

    /// The digests of the credentials the launch was given.
    pub fn known_digests(&self) -> &[String] {
        &self.known_digests
    }

    /// The launch's id.
    pub fn launch(&self) -> &str {
        &self.launch
    }

    /// The side of the review the launch works on.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The account the launch runs as.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The launch's AppContainer package SID.
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Which opening of the launch's pipe this is; it only grows.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The name of `launch`'s pipe to the broker `service`.
pub fn pipe_name(service: &str, launch: &str) -> String {
    format!("agentc-broker-{service}-{launch}")
}

/// What a broker does with the sessions of the launches it serves.
pub trait Handler: Send + Sync {
    /// Serves one admitted session of the launch `grant` names. It must
    /// return once `halt` is asked: a read of the session ends at once then,
    /// as does a write through a [`super::pipe::Writer`].
    fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> Result<()>;

    /// Hears of a refused client of `launch`'s pipe, to which nothing is
    /// written; by default the refusal goes to standard error.
    fn refused(&self, launch: &str, refusal: &Refusal) {
        eprintln!("agentc-supervisor: broker pipe of {launch}: {refusal}");
    }
}

/// What the threads of one launch's pipe share to learn that it is closing.
pub struct Halt {
    /// A manual-reset event, so every thread's wait ends once it is set.
    event: Handle,
    asked: AtomicBool,
}

// SAFETY: an event handle is a process-wide value; setting it and waiting
// on it are thread-safe.
unsafe impl Sync for Halt {}

impl Halt {
    /// An event nobody has set.
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: no attributes and no name; a manual-reset event, not set.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        Ok(Self {
            event: Handle::owned(event, "CreateEventW")?,
            asked: AtomicBool::new(false),
        })
    }

    /// Asks every thread to end and wakes the ones that wait.
    pub(crate) fn ask(&self) -> Result<()> {
        self.asked.store(true, Ordering::SeqCst);
        // SAFETY: the event handle is live.
        check(unsafe { SetEvent(self.event.raw()) }, "SetEvent")
    }

    /// Whether the launch's pipe is closing.
    pub fn asked(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }

    /// The event [`Halt::ask`] sets, which a pipe of this launch is made
    /// with as its wake event.
    pub(crate) fn wake(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.event.raw()
    }
}

/// The grants of the launches a broker serves, by package SID.
type Grants = Mutex<BTreeMap<String, Arc<Grant>>>;

/// One launch's pipe being served.
struct Server {
    package: String,
    halt: Arc<Halt>,
    threads: Vec<JoinHandle<()>>,
}

impl Server {
    /// Wakes the threads and waits for them to end; their pipe instances
    /// close as they end.
    fn stop(self) -> Result<()> {
        let asked = self.halt.ask();
        for thread in self.threads {
            let _ = thread.join();
        }
        asked
    }
}

/// One broker service and the launches it serves.
pub struct Broker {
    service: String,
    /// The account the broker runs as, which owns every pipe.
    owner: String,
    handler: Arc<dyn Handler>,
    grants: Arc<Grants>,
    /// The pipe of each open launch, by launch id.
    open: BTreeMap<String, Server>,
    /// The highest generation each launch has had, open or not.
    generations: BTreeMap<String, u64>,
}

impl Broker {
    /// The broker `service` (lower-case letters and digits) running as the
    /// account `owner` (SID text), whose sessions go to `handler`. It
    /// serves no launch yet.
    pub fn new(service: &str, owner: &str, handler: Arc<dyn Handler>) -> Result<Self> {
        let plain = !service.is_empty()
            && service.len() <= SERVICE_LIMIT
            && (service.bytes()).all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        ensure!(plain, "{service:?} cannot name a broker");
        Ok(Self {
            service: service.to_owned(),
            owner: checked_sid(owner)?.to_owned(),
            handler,
            grants: Arc::default(),
            open: BTreeMap::new(),
            generations: BTreeMap::new(),
        })
    }

    /// Serves the launch `grant` names on a pipe of its own and returns the
    /// grant as recorded. Its generation is at least the one asked for and
    /// above every earlier one of the launch. A launch that is open already
    /// is closed first; a package another open launch carries is refused.
    pub fn open(&mut self, grant: Grant) -> Result<Arc<Grant>> {
        let holder = self.holder(&grant.package);
        ensure!(
            holder.is_none_or(|launch| launch == grant.launch),
            "the package {} is another launch's",
            grant.package
        );
        let generation = self.next_generation(&grant)?;
        self.close(&grant.launch)?;
        let grant = Arc::new(Grant {
            generation,
            ..grant
        });
        let server = self.serve(&grant)?;
        self.generations.insert(grant.launch.clone(), generation);
        self.open.insert(grant.launch.clone(), server);
        Ok(grant)
    }

    /// Stops serving `launch`: its grant goes, a session still going ends
    /// and its pipe is gone once this returns. False when it is not open.
    pub fn close(&mut self, launch: &str) -> Result<bool> {
        let Some(server) = self.open.remove(launch) else {
            return Ok(false);
        };
        self.lock().remove(&server.package);
        server.stop()?;
        Ok(true)
    }

    /// The generation `grant` is recorded with: the one it asks for, raised
    /// above every earlier one of its launch. Computed before anything is
    /// closed, so a launch at the last generation is refused and stays as
    /// it was rather than reopened at a generation it already had.
    fn next_generation(&self, grant: &Grant) -> Result<u64> {
        let Some(&last) = self.generations.get(&grant.launch) else {
            return Ok(grant.generation);
        };
        let above = last.checked_add(1);
        let above = above.with_context(|| format!("{} has no generation left", grant.launch))?;
        Ok(grant.generation.max(above))
    }

    /// The broker's service name, which its pipes' names carry.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The account the broker runs as, which owns its pipes.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The grant of the open launch `launch`, as the broker recorded it.
    pub fn grant(&self, launch: &str) -> Option<Arc<Grant>> {
        let package = &self.open.get(launch)?.package;
        self.lock().get(package).cloned()
    }

    /// The launches this broker serves, in id order.
    pub fn launches(&self) -> Vec<String> {
        self.open.keys().cloned().collect()
    }

    /// The registry, even when a thread panicked while holding it: each
    /// change to it is a single insert or remove.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Grant>>> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The open launch whose grant names `package`, if any.
    fn holder(&self, package: &str) -> Option<String> {
        let grants = self.lock();
        grants.get(package).map(|grant| grant.launch.clone())
    }

    /// Records `grant` and starts a thread for each instance of its pipe.
    /// A pipe that cannot be made as specified is not made, nothing is
    /// recorded and no thread starts.
    fn serve(&self, grant: &Arc<Grant>) -> Result<Server> {
        let halt = Arc::new(Halt::new()?);
        let name = pipe_name(&self.service, &grant.launch);
        let clients = (grant.account.as_str(), grant.package.as_str());
        let wake = Some(halt.wake());
        let pipes = Pipe::broker_instances(&name, &self.owner, clients, INSTANCES, wake)
            .with_context(|| format!("create the broker pipe {name}"))?;
        self.lock().insert(grant.package.clone(), grant.clone());
        let threads = pipes.into_iter().map(|pipe| self.spawn(pipe, grant, &halt));
        Ok(Server {
            package: grant.package.clone(),
            halt: halt.clone(),
            threads: threads.collect(),
        })
    }

    /// Starts the thread that serves one instance of `grant`'s pipe.
    fn spawn(&self, pipe: Pipe, grant: &Grant, halt: &Arc<Halt>) -> JoinHandle<()> {
        let (grants, handler, halt) = (self.grants.clone(), self.handler.clone(), halt.clone());
        let launch = grant.launch.clone();
        std::thread::spawn(move || serve(pipe, &launch, &grants, handler.as_ref(), &halt))
    }
}

impl Drop for Broker {
    /// Closes every launch, so no thread outlives the broker.
    fn drop(&mut self) {
        for launch in self.launches() {
            if let Err(error) = self.close(&launch) {
                eprintln!("agentc-supervisor: broker pipe of {launch}: {error:#}");
            }
        }
    }
}

/// Serves one instance of `launch`'s pipe until it closes. A refused
/// client, or a session that fails, is reported and never ends the thread.
fn serve(mut pipe: Pipe, launch: &str, grants: &Grants, handler: &dyn Handler, halt: &Halt) {
    while !halt.asked() {
        let served = match pipe.accept(TICK) {
            Ok(Accepted::Client(mut session)) => {
                hand_over(&mut session, launch, grants, handler, halt)
            }
            Ok(Accepted::Refused(refusal)) => {
                handler.refused(launch, &refusal);
                Ok(())
            }
            Ok(Accepted::Idle) => Ok(()),
            Err(error) => {
                std::thread::sleep(TICK);
                Err(error)
            }
        };
        if let Err(error) = served {
            eprintln!("agentc-supervisor: broker pipe of {launch}: {error:#}");
        }
    }
}

/// Hands an admitted session to `handler` with the grant the registry
/// holds for the package Windows read from the client's token. A grant
/// that is gone (the launch is closing) or names another launch serves
/// nothing.
fn hand_over(
    session: &mut Session<'_>,
    launch: &str,
    grants: &Grants,
    handler: &dyn Handler,
    halt: &Halt,
) -> Result<()> {
    let package = session.client().package.clone();
    let package = package.context("an admitted broker client has no package")?;
    let grant = registered(grants, &package).context("the launch's grant is gone")?;
    ensure!(
        grant.launch == launch,
        "the package {package} is registered to another launch"
    );
    handler.serve(&grant, session, halt)
}

/// The grant `grants` holds for `package`.
fn registered(grants: &Grants, package: &str) -> Option<Arc<Grant>> {
    let grants = grants.lock().unwrap_or_else(PoisonError::into_inner);
    grants.get(package).cloned()
}

#[cfg(test)]
mod tests {
    use super::super::pipe::Received;
    use super::super::token;
    use super::*;
    use crate::service::pipe_policy::CLIENT_ACCESS;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Instant;

    /// A package no token on this host carries.
    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";
    /// Another such package.
    const OTHER_PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-8";
    /// Windows' code for a name that does not exist.
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    /// Windows' code for an access that is denied.
    const ERROR_ACCESS_DENIED: i32 = 5;

    /// Records what the broker handed it.
    #[derive(Default)]
    struct Recorder {
        served: Mutex<Vec<Grant>>,
        refused: Mutex<Vec<Refusal>>,
    }

    impl Handler for Recorder {
        /// Records the grant and holds the session until the client leaves
        /// or the pipe closes.
        fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> Result<()> {
            self.served.lock().unwrap().push(grant.clone());
            while !halt.asked() {
                if session.read(1024, TICK)? == Received::Closed {
                    break;
                }
            }
            Ok(())
        }

        /// Keeps the refusal so a test can assert why a client was refused.
        fn refused(&self, _launch: &str, refusal: &Refusal) {
            self.refused.lock().unwrap().push(refusal.clone());
        }
    }

    /// This process's account, which the test launches and brokers run as.
    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// A launch id no other test or run uses.
    fn launch() -> String {
        format!("t{}", uuid::Uuid::new_v4().simple())
    }

    /// A broker running as this account, recording into `recorder`.
    fn broker(recorder: &Arc<Recorder>) -> Broker {
        Broker::new("test", &me(), recorder.clone()).unwrap()
    }

    /// This account's grant of `launch` with `package`.
    fn grant(launch: &str, package: &str, generation: u64) -> Grant {
        Grant::new(launch, Role::Reviewer, &me(), package, generation).unwrap()
    }

    /// Opens the pipe at `path` with a client's rights.
    fn client(path: &str) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.access_mode(CLIENT_ACCESS);
        options.open(path)
    }

    /// The local path of `launch`'s pipe to the test broker.
    fn local(launch: &str) -> String {
        pipe_path(&pipe_name("test", launch)).unwrap()
    }

    /// Waits up to ten seconds for `ready`.
    fn eventually(ready: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_grant_carries_only_well_formed_credential_digests() {
        let grant = Grant::new("l", Role::Implementer, "S-1-5-21-1-2-3-1001", PACKAGE, 1).unwrap();
        assert!(grant.known_digests().is_empty(), "none by default");
        let digest = "0123456789abcdef".repeat(4);
        let carried = grant
            .clone()
            .with_known_digests(vec![digest.clone()])
            .unwrap();
        assert_eq!(carried.known_digests(), std::slice::from_ref(&digest));
        let malformed = [
            digest.to_uppercase(),
            digest[1..].to_owned(),
            format!("{digest}0"),
        ];
        for bad in malformed {
            assert!(grant.clone().with_known_digests(vec![bad]).is_err());
        }
        let too_many = vec![digest; MAX_KNOWN_DIGESTS + 1];
        assert!(grant.with_known_digests(too_many).is_err());
    }

    #[test]
    fn a_role_is_written_in_lower_case() {
        assert_eq!(
            serde_json::to_value(Role::Implementer).unwrap(),
            "implementer"
        );
        let reviewer: Role = serde_json::from_str("\"reviewer\"").unwrap();
        assert_eq!(reviewer, Role::Reviewer);
        assert!(serde_json::from_str::<Role>("\"Reviewer\"").is_err());
    }

    #[test]
    fn a_grant_takes_only_a_plain_launch_id_and_sid_text() {
        let me = me();
        assert!(Grant::new("a/b", Role::Reviewer, &me, PACKAGE, 1).is_err());
        assert!(Grant::new("l", Role::Reviewer, "WD", PACKAGE, 1).is_err());
        assert!(Grant::new("l", Role::Reviewer, &me, "S-1-15-2)(A;;GA;;;WD", 1).is_err());
        let grant = Grant::new("l", Role::Implementer, &me, PACKAGE, 3).unwrap();
        let seen = (
            grant.launch(),
            grant.role(),
            grant.account(),
            grant.package(),
        );
        assert_eq!(seen, ("l", Role::Implementer, me.as_str(), PACKAGE));
        assert_eq!(grant.generation(), 3);
        assert_eq!(pipe_name("egress", "l"), "agentc-broker-egress-l");
        assert!(Broker::new("Egress", &me, Arc::new(Recorder::default())).is_err());
        assert!(Broker::new("a-b", &me, Arc::new(Recorder::default())).is_err());
    }

    #[test]
    fn opening_a_launch_again_gives_it_a_higher_generation() {
        let recorder = Arc::new(Recorder::default());
        let (mut broker, id) = (broker(&recorder), launch());
        assert_eq!(broker.open(grant(&id, PACKAGE, 5)).unwrap().generation(), 5);
        let reopened = broker.open(grant(&id, PACKAGE, 5)).unwrap();
        assert_eq!(reopened.generation(), 6, "opened again while open");
        assert!(broker.close(&id).unwrap());
        assert!(!broker.close(&id).unwrap(), "closed twice");
        let after = broker.open(grant(&id, PACKAGE, 1)).unwrap();
        assert_eq!(after.generation(), 7, "opened again after a close");
        let asked = broker.open(grant(&id, PACKAGE, 40)).unwrap();
        assert_eq!(asked.generation(), 40, "a higher generation asked for");
        assert_eq!(broker.launches(), [id]);
    }

    #[test]
    fn a_launch_at_the_last_generation_is_refused_and_stays_open() {
        let recorder = Arc::new(Recorder::default());
        let (mut broker, id) = (broker(&recorder), launch());
        let last = broker.open(grant(&id, PACKAGE, u64::MAX)).unwrap();
        assert_eq!(last.generation(), u64::MAX);
        assert!(broker.open(grant(&id, PACKAGE, u64::MAX)).is_err());
        assert_eq!(broker.launches(), [id.as_str()], "the refusal closed it");
        assert!(client(&local(&id)).is_ok(), "its pipe is gone");
    }

    #[test]
    fn a_package_another_open_launch_carries_is_refused() {
        let recorder = Arc::new(Recorder::default());
        let mut broker = broker(&recorder);
        let (first, second) = (launch(), launch());
        broker.open(grant(&first, PACKAGE, 1)).unwrap();
        assert!(broker.open(grant(&second, PACKAGE, 1)).is_err());
        broker.open(grant(&second, OTHER_PACKAGE, 1)).unwrap();
        assert_eq!(broker.launches().len(), 2);
    }

    #[test]
    fn the_launchs_account_without_its_package_is_refused_and_never_served() {
        // The access list lets this account in, so admission is the wall.
        let recorder = Arc::new(Recorder::default());
        let (mut broker, id) = (broker(&recorder), launch());
        broker.open(grant(&id, PACKAGE, 1)).unwrap();
        let mut file = client(&local(&id)).unwrap();
        file.write_all(b"{\"launch\":\"forged\"}\n").unwrap();
        let refused = || !recorder.refused.lock().unwrap().is_empty();
        assert!(eventually(refused), "no refusal was reported");
        let mut answer = Vec::new();
        let read = std::io::Read::read_to_end(&mut file, &mut answer);
        assert!(
            read.is_err() || answer.is_empty(),
            "the refused client got an answer"
        );
        let refusals = recorder.refused.lock().unwrap().clone();
        assert_eq!(refusals, [Refusal::WrongPackage(None)]);
        assert!(recorder.served.lock().unwrap().is_empty());
    }

    /// Creates a pipe instance with the default access list that accepts
    /// remote clients: the control that shows whether the remote form of a
    /// pipe path reaches this host's pipes at all.
    fn open_pipe(name: &str) -> Handle {
        use super::super::text::wide;
        use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
        let path = wide(&pipe_path(name).unwrap()).unwrap();
        // SAFETY: the name is NUL-terminated; no attributes, so the default
        // access list applies.
        let raw = unsafe {
            #[rustfmt::skip]
            let raw = CreateNamedPipeW(path.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT,
                1, 4096, 4096, 0, std::ptr::null());
            raw
        };
        Handle::owned(raw, "CreateNamedPipeW").unwrap()
    }

    #[test]
    fn a_client_that_opens_the_pipe_by_its_remote_form_is_refused() {
        let recorder = Arc::new(Recorder::default());
        let (mut broker, id) = (broker(&recorder), launch());
        broker.open(grant(&id, PACKAGE, 1)).unwrap();
        let remote = format!(r"\\localhost\pipe\{}", pipe_name("test", &id));
        let error = client(&remote).expect_err("the remote form opened the broker pipe");
        eprintln!("the broker pipe by its remote form: {error}");
        let control = format!("agentc-test-remote-{}", uuid::Uuid::new_v4().simple());
        let _control = open_pipe(&control);
        match client(&format!(r"\\localhost\pipe\{control}")) {
            Ok(_) => assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED), "{error}"),
            Err(why) => eprintln!(
                "the remote form reaches no pipe on this host ({why}); the refusal above \
                 shows only that the path is unusable here, not which wall stopped it"
            ),
        }
        assert!(recorder.served.lock().unwrap().is_empty());
        assert!(
            recorder.refused.lock().unwrap().is_empty(),
            "it was never connected"
        );
    }

    #[test]
    fn closing_a_launch_removes_its_pipe_and_its_grant() {
        let recorder = Arc::new(Recorder::default());
        let (mut broker, id) = (broker(&recorder), launch());
        broker.open(grant(&id, PACKAGE, 1)).unwrap();
        client(&local(&id)).unwrap();
        assert!(broker.holder(PACKAGE).is_some());
        let started = Instant::now();
        assert!(broker.close(&id).unwrap());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let gone = client(&local(&id)).unwrap_err();
        assert_eq!(gone.raw_os_error(), Some(ERROR_FILE_NOT_FOUND), "{gone}");
        assert!(broker.holder(PACKAGE).is_none());
        assert!(broker.launches().is_empty());
    }
}
