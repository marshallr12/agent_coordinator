//! The live [`Driver`]: runs as root and reads `next` with the implementer's
//! coordinator credential. Everything it does to the host (refreshing the
//! mirror, cloning, preparing `$RUN`, running the coordinator CLI as the role
//! account, `launch-root` and its signals, root's reads and writes below the
//! role's directories) goes through its [`LinuxHost`], the Linux
//! [`HostRuntime`]. Instruction files come from the root-owned mirror, never
//! the clone. SIGTERM and SIGINT set a flag the loop polls, so it drains
//! rather than dies.
use super::binding::{self, Binding};
use super::lease::Lease;
use super::live_review::LiveReviewer;
use super::renewal::{self, AttemptEnded, CliError, attempt_state};
use super::review::ReviewDriver;
use super::{Driver, Launch};
use crate::clone;
use crate::config::Config;
use crate::host_runtime::HostRuntime;
use crate::host_runtime::linux::{LinuxHost, Source};
use crate::profile::{Harness, Role};
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, ensure};
use coordinator_client::CoordinatorClient;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by SIGTERM or SIGINT: stop claiming, drain the launch and exit.
static STOP: AtomicBool = AtomicBool::new(false);
/// The CLI's exit code for an HTTP 409: the attempt is no longer active.
const CONFLICT_EXIT: i32 = 5;

/// Talks to the coordinator and runs commands for one host.
pub struct LiveDriver {
    config: Config,
    /// The operating-system side: role commands, role files and the launch.
    host: LinuxHost,
    runtime: tokio::runtime::Runtime,
    client: CoordinatorClient,
    /// The projects worked on: the main one, then the canary's if any.
    slots: Vec<Slot>,
    /// The slot `next`, [`Driver::project`] and the reviewer act on.
    active: usize,
    /// Not before this time (ms since the epoch) does a pass that left cost
    /// rows unsent run again ([`live_report::RETRY_AFTER_MS`]).
    report_after_ms: i64,
    /// When the daily caps are next reported (they change only with the config).
    caps_after_ms: i64,
}

/// One project the loop works on: its binding and its reviewer side when
/// `[run] reviewer` is on. The host runtime holds the project's mirror.
struct Slot {
    /// The coordinator, project and credential selector worked under.
    binding: Binding,
    reviewer: Option<LiveReviewer>,
}

/// Runs the live loop as root until stopped (or once).
pub fn run(config: &Config, config_path: Option<&Path>, once: bool) -> Result<()> {
    accounts::require_root(accounts::effective_uid())?;
    let mut driver = LiveDriver::new(config, config_path)?;
    on_stop_requests();
    super::run(&mut driver, config, once)
}

/// As root, sends every unsent cost-ledger row to the coordinator once
/// (`agentc-supervisor report-costs`), whatever the retry interval.
pub fn report_once(config: &Config, config_path: Option<&Path>) -> Result<super::report::Summary> {
    accounts::require_root(accounts::effective_uid())?;
    let driver = LiveDriver::new(config, config_path)?;
    let main = &driver.slots[0].binding.project_id;
    Ok(super::live_report::pass(
        config,
        &driver.runtime,
        &driver.client,
        main,
    ))
}

/// Records SIGTERM and SIGINT in [`STOP`] instead of dying.
fn on_stop_requests() {
    extern "C" fn request_stop(_signal: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    let handler = request_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe { libc::signal(signal, handler) };
    }
}

impl LiveDriver {
    /// Resolves the binding (`[run.binding]`, else the mirror's) and the
    /// canary one if any, installs their root-owned copies and connects with
    /// the implementer's credential.
    fn new(config: &Config, config_path: Option<&Path>) -> Result<Self> {
        let binding = resolve_binding(config)?;
        let insecure = binding::insecure(&binding.service_url, config.run.allow_insecure_loopback)?;
        install_binding(&binding::installed_path(config), &binding.to_toml()?)?;
        let mut bindings = vec![(binding.clone(), mirror(config))];
        if let Some(canary) = &config.run.canary_binding {
            canary.check_against(&binding)?;
            let extra = canary.binding();
            install_binding(&binding::canary_installed_path(config), &extra.to_toml()?)?;
            bindings.push((extra, canary.mirror_path(config)));
        }
        let account = Account::lookup(Role::Implementer.user(config))?;
        let host = LinuxHost::new(config, config_path, account, insecure, sources(&bindings));
        let shown = host.credentials_path();
        let text =
            String::from_utf8(host.read_credentials()?).context("credentials are not UTF-8")?;
        let all: Vec<&Binding> = bindings.iter().map(|(b, _)| b).collect();
        let mut slots = Vec::new();
        for (binding, mirror) in &bindings {
            let reviewer = reviewer(config, config_path, binding, &all, mirror, insecure)?;
            slots.push(Slot {
                binding: binding.clone(),
                reviewer,
            });
        }
        Ok(Self {
            config: config.clone(),
            host,
            runtime: tokio::runtime::Runtime::new()?,
            client: crate::shadow::client_from(&text, &shown, &binding.service_url, insecure)?,
            slots,
            active: 0,
            report_after_ms: 0,
            caps_after_ms: 0,
        })
    }

    /// The slot `next` and the reviewer side act on.
    fn slot(&self) -> &Slot {
        &self.slots[self.active]
    }

    /// Runs the coordinator CLI as the implementer in the launch's clone,
    /// with the launch's session, and returns its JSON stdout.
    fn cli(&self, launch: &Launch, args: &[String]) -> Result<Value> {
        let cli = self.config.bin_dir.join("agent-coordinator");
        let stdout = self.host.as_role(&cli, args, &launch.clone, launch)?;
        serde_json::from_slice(&stdout).context("the CLI printed no JSON")
    }

    /// The launch attempt's state from its task's detail, if readable.
    fn read_attempt_state(&self, launch: &Launch, lease: &Lease) -> Option<String> {
        let (project, task) = (&launch.project, &launch.suggestion.task);
        let path = format!("/api/v1/projects/{project}/tasks/{task}");
        let call = crate::shadow::get_data(&self.client, &path, &[]);
        match self.runtime.block_on(call) {
            Ok(detail) => attempt_state(&detail, &lease.attempt),
            Err(error) => {
                eprintln!(
                    "agentc-supervisor run: read attempt {}: {error:#}",
                    lease.attempt
                );
                None
            }
        }
    }
}

/// One instruction file at `revision`, read from `mirror` as a blob of
/// bounded size and truncated for the prompt. A symlink's blob is its target
/// path, so nothing outside the repository is ever read.
pub(super) fn instruction(mirror: &Path, revision: &str, name: &str) -> Option<String> {
    let object = format!("{revision}:{name}");
    let size: usize = clone::git_output(mirror, &["cat-file", "-s", &object])
        .ok()?
        .parse()
        .ok()?;
    if size > MAX_BLOB {
        return None;
    }
    let text = clone::git_output(mirror, &["cat-file", "blob", &object]).ok()?;
    Some(truncate(text, super::MAX_INSTRUCTION_BYTES))
}

impl Driver for LiveDriver {
    fn project(&self) -> &str {
        &self.slot().binding.project_id
    }

    fn projects(&self) -> Vec<String> {
        (self.slots.iter())
            .map(|slot| slot.binding.project_id.clone())
            .collect()
    }

    fn select(&mut self, project: &str) {
        if let Some(index) = (self.slots.iter()).position(|s| s.binding.project_id == project) {
            self.active = index;
        }
    }

    fn next(&mut self, role: Role, exclude: &[String]) -> Result<Value> {
        let name = format!("{role:?}").to_lowercase();
        let call = crate::shadow::fetch_next(
            &self.client,
            self.project(),
            &name,
            ("exclude_tasks", exclude),
        );
        self.runtime.block_on(call)
    }

    fn free_bytes(&self) -> Result<u64> {
        self.host.free_bytes()
    }

    fn host(&mut self) -> &mut dyn HostRuntime {
        &mut self.host
    }

    fn instructions(&mut self, launch: &Launch) -> Vec<(String, String)> {
        let mirror = self.host.mirror_of(&launch.project);
        let revision = self.host.revision();
        let read = |name: &&str| Some(((*name).to_owned(), instruction(mirror, revision, name)?));
        super::INSTRUCTION_FILES.iter().filter_map(read).collect()
    }

    fn install_prompt(&mut self, launch: &Launch, prompt: &str) -> Result<()> {
        let name = crate::profile::run_files::PROMPT;
        self.host.write_run_file(launch, name, prompt.as_bytes())
    }

    /// Connects the launch's session and claims the task with the pinned CLI,
    /// which acknowledges the current orientation first.
    fn claim(&mut self, launch: &Launch) -> Result<Lease> {
        let harness = format!("--harness=agentc-supervisor-{:?}", launch.vendor.harness);
        let connect = ["--json".into(), "connect".into(), harness.to_lowercase()];
        self.cli(launch, &connect)?;
        let s = &launch.suggestion;
        let claim = ["--json", "claim", &format!("--task={}", s.task)].map(String::from);
        let revision = format!("--revision={}", s.revision);
        Lease::parse(&self.cli(launch, &[&claim[..], &[revision]].concat())?)
    }

    /// Runs `agent-coordinator worktree prepare` as the implementer in the
    /// launch's session: it adds [`Launch::checkout`] as a worktree of the
    /// clone at the cloned revision and registers it for the attempt.
    fn register(&mut self, launch: &Launch, lease: &Lease) -> Result<()> {
        let args = checkout_args(launch, lease, self.host.revision());
        self.cli(launch, &args).map(drop)
    }

    fn last_event_ms(&self, launch: &Launch) -> Option<i64> {
        self.host.last_event_ms(launch)
    }

    /// Renews through the CLI. A refusal that ends the attempt for good
    /// fails with an [`AttemptEnded`] carrying the attempt's state.
    fn renew(&mut self, launch: &Launch, lease: &Lease) -> Result<Lease> {
        let args = ["--json", "renew", &format!("--attempt={}", lease.attempt)];
        let generation = format!("--generation={}", lease.generation);
        let args = [&args.map(String::from)[..], &[generation]].concat();
        let cli = self.config.bin_dir.join("agent-coordinator");
        let output = self
            .host
            .run_role(&cli, &args, &launch.clone, launch, b"")?;
        if output.status.success() {
            return Lease::parse(&serde_json::from_slice(&output.stdout)?);
        }
        let failure = CliError::of(&output);
        ensure!(
            failure.ends_attempt(),
            "{} failed: {failure}",
            args.join(" ")
        );
        let state = self.attempt_state(launch, lease);
        Err(AttemptEnded {
            code: failure.code,
            state,
        }
        .into())
    }

    /// Reads the attempt's state, once more if the first read fails: an
    /// unreadable state must not make a submitted attempt look unsubmitted.
    fn attempt_state(&mut self, launch: &Launch, lease: &Lease) -> Option<String> {
        renewal::retry_once(|| self.read_attempt_state(launch, lease))
    }

    /// Releases through the CLI with the handoff on standard input; a 409
    /// means the agent already submitted or released the attempt.
    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        let cli = self.config.bin_dir.join("agent-coordinator");
        let args = [
            "--json".into(),
            "release".into(),
            format!("--attempt={}", lease.attempt),
            format!("--generation={}", lease.generation),
            "--input=-".into(),
        ];
        let input = serde_json::to_vec(&serde_json::json!({"summary": summary}))?;
        let output = self
            .host
            .run_role(&cli, &args, &launch.clone, launch, &input)?;
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success() || code == Some(CONFLICT_EXIT),
            "release failed: {}",
            stderr.trim()
        );
        Ok(())
    }

    fn has_marker(&self, launch: &Launch, name: &str) -> bool {
        self.host.has_marker(launch, name)
    }

    fn now_ms(&self) -> i64 {
        i64::try_from(crate::shadow::now_ms()).unwrap_or(i64::MAX)
    }

    fn pause(&mut self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }

    fn stopping(&self) -> bool {
        stop_requested()
    }

    fn heartbeat(&mut self, launch: &Launch) {
        let path = super::heartbeat_path(&self.config);
        if let Err(error) =
            super::beat_busy(&path, Role::Implementer.slug(), &launch.suggestion.task)
        {
            eprintln!("agentc-supervisor run: heartbeat: {error:#}");
        }
    }

    fn reviewer(&mut self) -> Option<&mut dyn ReviewDriver> {
        let active = self.active;
        (self.slots[active].reviewer.as_mut()).map(|r| r as &mut dyn ReviewDriver)
    }

    fn harness_status(&mut self, harness: Harness) -> Result<()> {
        super::live_health::status(&self.config, self.host.account(), harness)
    }

    fn credential_expiry_ms(&self, harness: Harness) -> Option<i64> {
        super::live_health::expiry_ms(&self.config, harness)
    }

    fn report_costs(&mut self, config: &Config) {
        let now = self.now_ms();
        if now >= self.caps_after_ms {
            let projects: Vec<&str> = (self.slots.iter())
                .map(|slot| slot.binding.project_id.as_str())
                .collect();
            let sent =
                super::live_report::caps_pass(config, &self.runtime, &self.client, &projects);
            let wait = if sent {
                super::live_report::CAPS_EVERY_MS
            } else {
                super::live_report::RETRY_AFTER_MS
            };
            self.caps_after_ms = now.saturating_add(wait);
        }
        if now < self.report_after_ms {
            return;
        }
        let main = &self.slots[0].binding.project_id;
        let summary = super::live_report::pass(config, &self.runtime, &self.client, main);
        if summary.pending > 0 {
            self.report_after_ms = now.saturating_add(super::live_report::RETRY_AFTER_MS);
        }
        if summary.accepted
            + summary.duplicates
            + summary.rejected
            + summary.quarantined
            + summary.pending
            > 0
        {
            eprintln!("agentc-supervisor run: report costs: {summary:?}");
        }
    }

    fn events(&self, launch: &Launch) -> Option<Vec<u8>> {
        super::live_health::events(&self.config, self.host.account(), launch)
    }

    fn stderr(&self, launch: &Launch) -> Option<Vec<u8>> {
        super::live_health::stderr(&self.config, self.host.account(), launch)
    }
}

/// Whether SIGTERM or SIGINT asked the loop to stop.
pub(super) fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The reviewer side of one project, built only when `[run] reviewer` is
/// on, working under the same binding as the implementer side. `all` lists
/// every binding the host serves, so a project's credential copy is never
/// taken for a stale one.
fn reviewer(
    config: &Config,
    path: Option<&Path>,
    binding: &Binding,
    all: &[&Binding],
    mirror: &Path,
    insecure: bool,
) -> Result<Option<LiveReviewer>> {
    if !config.run.reviewer {
        return Ok(None);
    }
    let arg: Vec<String> = path
        .map(|p| format!("--config={}", p.display()))
        .into_iter()
        .collect();
    LiveReviewer::new(config, &arg, binding, all, mirror, insecure).map(Some)
}

/// The binding the loop works under: `[run.binding]` from the host
/// configuration when set, else `.agent-coordinator.toml` on the mirror's
/// `[run] branch`. Neither comes from a role-writable clone.
fn resolve_binding(config: &Config) -> Result<Binding> {
    if let Some(binding) = &config.run.binding {
        binding.check().context("[run.binding]")?;
        return Ok(binding.clone());
    }
    let spec = format!("{}:.agent-coordinator.toml", config.run.branch);
    let text = clone::git_output(&mirror(config), &["show", &spec])?;
    Binding::parse(&text).context("the mirror's binding")
}

/// The host runtime's sources: each binding with its mirror, in order.
fn sources(bindings: &[(Binding, PathBuf)]) -> Vec<Source> {
    let source = |(binding, mirror): &(Binding, PathBuf)| Source {
        binding: binding.clone(),
        mirror: mirror.clone(),
    };
    bindings.iter().map(source).collect()
}

/// The host mirror clones come from, `<state_dir>/mirror.git`.
pub(super) fn mirror(config: &Config) -> PathBuf {
    config.state_dir.join("mirror.git")
}

/// Writes the binding to `path`, one of the [`binding::installed_path_for`]
/// copies (root-owned, mode 0644 so the role can read it), replacing it
/// atomically. Renewals, releases and a `[run.binding]` launch's own CLI then
/// never read the clone's role-writable `.agent-coordinator.toml`.
fn install_binding(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let temp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&temp);
    let mut options = std::fs::OpenOptions::new();
    let mut file = options
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temp)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    file.write_all(text.as_bytes())?;
    std::fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))
}

/// The largest instruction blob read from the mirror.
const MAX_BLOB: usize = 1024 * 1024;

/// `text` cut to at most `max` bytes on a character boundary.
fn truncate(mut text: String, max: usize) -> String {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// `worktree prepare` arguments registering the launch's checkout for the
/// attempt: a branch named after the session, based on `revision`.
fn checkout_args(launch: &Launch, lease: &Lease, revision: &str) -> Vec<String> {
    vec![
        "--json".into(),
        "worktree".into(),
        "prepare".into(),
        format!("--attempt={}", lease.attempt),
        format!("--generation={}", lease.generation),
        format!("--source={}", launch.clone.display()),
        format!("--path={}", launch.checkout().display()),
        format!("--branch=agentc/{}", launch.session_id),
        format!("--base={revision}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_loop::Suggestion;
    use std::process::Command;

    #[test]
    fn a_run_binding_overrides_the_mirror_and_is_checked() {
        let mut config = Config::default();
        config.run.binding = Some(Binding {
            service_url: "http://127.0.0.1:18080".into(),
            project_id: "staging-project".into(),
            project_name: Some("Staging".into()),
        });
        let resolved = resolve_binding(&config).unwrap();
        assert_eq!(resolved.project_id, "staging-project");
        config.run.binding.as_mut().unwrap().project_name = Some("../x".into());
        assert!(resolve_binding(&config).is_err());
    }

    #[test]
    fn the_checkout_is_a_session_branch_inside_the_clone_at_the_cloned_revision() {
        let suggestion = Suggestion {
            task: "t".into(),
            revision: 1,
            title: String::new(),
        };
        let launch = Launch::plan(&Config::default(), "p", suggestion);
        let lease = Lease {
            attempt: "a1".into(),
            generation: 2,
            renew_after_seconds: 60,
            progress_age_ms: 0,
        };
        let args = checkout_args(&launch, &lease, "abc123");
        let clone = launch.clone.display();
        let expected = [
            "--json".to_owned(),
            "worktree".into(),
            "prepare".into(),
            "--attempt=a1".into(),
            "--generation=2".into(),
            format!("--source={clone}"),
            format!("--path={clone}/agentc-checkout"),
            format!("--branch=agentc/{}", launch.session_id),
            "--base=abc123".into(),
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn the_lease_comes_from_captured_claim_and_renew_responses() {
        let claim: Value =
            serde_json::from_str(include_str!("fixtures/claim-response.json")).unwrap();
        let lease = Lease::parse(&claim).unwrap();
        assert_eq!(lease.attempt, "64b8dc86-d996-435e-b5ff-7231098a8f9e");
        assert_eq!((lease.generation, lease.renew_after_seconds), (1, 60));
        assert_eq!(lease.progress_age_ms, 0);
        let renew: Value =
            serde_json::from_str(include_str!("fixtures/renew-response.json")).unwrap();
        let renewed = Lease::parse(&renew).unwrap();
        assert_eq!(
            (renewed.attempt, renewed.progress_age_ms),
            (lease.attempt, 500_000)
        );
        assert!(Lease::parse(&serde_json::json!({"data": {"claim": null}})).is_err());
    }

    /// Runs `git args` in `dir`, failing the test on error.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn instructions_come_from_bounded_mirror_blobs_never_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let (work, mirror) = (dir.path().join("work"), dir.path().join("mirror.git"));
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "outside-secret").unwrap();
        git(dir.path(), &["init", "--quiet", "work"]);
        std::os::unix::fs::symlink(&secret, work.join("AGENTS.md")).unwrap();
        std::fs::write(work.join("CONTRIBUTING.md"), "x".repeat(MAX_BLOB + 1)).unwrap();
        std::fs::write(work.join("README.md"), "Run the gate.").unwrap();
        git(&work, &["add", "."]);
        let identity = ["-c", "user.name=t", "-c", "user.email=t@example.com"];
        git(
            &work,
            &[&identity[..], &["commit", "--quiet", "-m", "i"]].concat(),
        );
        git(
            dir.path(),
            &["clone", "--quiet", "--bare", "work", "mirror.git"],
        );
        let head = clone::git_output(&mirror, &["rev-parse", "HEAD"]).unwrap();
        let linked = instruction(&mirror, &head, "AGENTS.md").unwrap();
        assert_eq!(linked, secret.display().to_string());
        assert_eq!(instruction(&mirror, &head, "CONTRIBUTING.md"), None);
        assert_eq!(
            instruction(&mirror, &head, "README.md").unwrap(),
            "Run the gate."
        );
        assert_eq!(instruction(&mirror, &head, "MISSING.md"), None);
    }

    #[test]
    fn the_binding_copy_is_world_readable_and_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            state_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let path = binding::installed_path(&config);
        install_binding(&path, "old").unwrap();
        install_binding(&path, "project_id = \"p\"").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "project_id = \"p\""
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate("a\u{e9}b".into(), 2), "a");
        assert_eq!(truncate("ab".into(), 5), "ab");
    }
}
