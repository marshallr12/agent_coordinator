//! The live [`Driver`]: runs as root, reads `next` with the implementer's
//! coordinator credential, refreshes the host mirror, and runs every step
//! that touches the role's files (`clone`, `prepare`, the coordinator CLI) as
//! the role account. `launch-root` itself runs as root, as by hand. Root's
//! own reads and writes below the role's directories go through `rooted`;
//! instruction files come from the root-owned mirror, never the clone.
//! SIGTERM and SIGINT set a flag the loop polls, so it drains rather than
//! dies; `launch-root` runs in its own process group, which the drain
//! signals as a whole.
use super::binding::{self, Binding};
use super::lease::Lease;
use super::live_review::LiveReviewer;
use super::record::{self, LaunchRecord};
use super::review::ReviewDriver;
use super::{Driver, Launch, rooted};
use crate::clone;
use crate::config::Config;
use crate::profile::{Harness, Role};
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, ensure};
use coordinator_client::CoordinatorClient;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by SIGTERM or SIGINT: stop claiming, drain the launch and exit.
static STOP: AtomicBool = AtomicBool::new(false);
/// The CLI's exit code for an HTTP 409: the attempt is no longer active.
const CONFLICT_EXIT: i32 = 5;

/// Talks to the coordinator and runs commands for one host.
pub struct LiveDriver {
    config: Config,
    /// Passed on to `agentc-supervisor` children as `--config`.
    config_arg: Vec<String>,
    runtime: tokio::runtime::Runtime,
    client: CoordinatorClient,
    /// The coordinator, project and credential selector worked under.
    binding: Binding,
    /// Whether coordinator calls may use plain HTTP to loopback.
    insecure: bool,
    account: Account,
    /// The mirror revision the current launch was cloned at.
    revision: String,
    /// The running `launch-root`, if any.
    child: Option<Child>,
    /// The reviewer side when `[run] reviewer` is on.
    reviewer: Option<LiveReviewer>,
}

/// Runs the live loop as root until stopped (or once).
pub fn run(config: &Config, config_path: Option<&Path>, once: bool) -> Result<()> {
    accounts::require_root(accounts::effective_uid())?;
    let mut driver = LiveDriver::new(config, config_path)?;
    on_stop_requests();
    super::run(&mut driver, config, once)
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
    /// Resolves the binding (`[run.binding]`, else the mirror's), installs
    /// its root-owned copy and connects with the implementer's credential.
    fn new(config: &Config, config_path: Option<&Path>) -> Result<Self> {
        let binding = resolve_binding(config)?;
        let insecure = binding::insecure(&binding.service_url, config.run.allow_insecure_loopback)?;
        install_binding(config, &binding.to_toml()?)?;
        let account = Account::lookup(Role::Implementer.user(config))?;
        let credentials = read_credentials(config, &account)?;
        let shown = role_dir(config).join(CREDENTIALS);
        let text = String::from_utf8(credentials).context("credentials are not UTF-8")?;
        let reviewer = reviewer(config, config_path, &binding, insecure)?;
        Ok(Self {
            config: config.clone(),
            config_arg: config_path
                .map(|p| format!("--config={}", p.display()))
                .into_iter()
                .collect(),
            runtime: tokio::runtime::Runtime::new()?,
            client: crate::shadow::client_from(&text, &shown, &binding.service_url, insecure)?,
            binding,
            insecure,
            account,
            revision: String::new(),
            child: None,
            reviewer,
        })
    }

    /// Runs `program args` as the implementer in `cwd` and returns its
    /// stdout; fails with its stderr on a non-zero exit.
    fn as_role(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        launch: &Launch,
    ) -> Result<Vec<u8>> {
        let output = self.run_role(program, args, cwd, launch, b"")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.join(" "),
            stderr.trim()
        );
        Ok(output.stdout)
    }

    /// Runs `program args` as the implementer in `cwd` with `input` on its
    /// standard input and returns its output, whatever its exit.
    fn run_role(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        launch: &Launch,
        input: &[u8],
    ) -> Result<Output> {
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd).env_clear();
        command.envs(role_env(&self.config, launch, self.insecure));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        accounts::run_as(&mut command, &self.account);
        let mut child = command
            .spawn()
            .with_context(|| format!("run {}", program.display()))?;
        child.stdin.take().context("stdin")?.write_all(input)?;
        Ok(child.wait_with_output()?)
    }

    /// Runs the coordinator CLI as the implementer in the launch's clone,
    /// with the launch's session, and returns its JSON stdout.
    fn cli(&self, launch: &Launch, args: &[String]) -> Result<Value> {
        let cli = self.config.bin_dir.join("agent-coordinator");
        let stdout = self.as_role(&cli, args, &launch.clone, launch)?;
        serde_json::from_slice(&stdout).context("the CLI printed no JSON")
    }

    /// `relative` below the implementer's directory: `$RUN` or the clone.
    fn below_role(&self, path: &Path) -> Result<PathBuf> {
        Ok(path.strip_prefix(role_dir(&self.config))?.to_owned())
    }

    /// `agentc-supervisor` arguments: `--config`, `subcommand`, then `rest`.
    fn supervisor_args(&self, subcommand: &str, rest: Vec<String>) -> Vec<String> {
        let mut args = self.config_arg.clone();
        args.push(subcommand.into());
        args.extend(rest);
        args
    }

    /// Writes `contents` to a new role-owned mode 0600 file at `name` in
    /// the launch's `$RUN`, refusing symlinks on the way.
    fn write_run_file(&self, launch: &Launch, name: &str, contents: &[u8]) -> Result<()> {
        let relative = self.below_role(&launch.run)?.join(name);
        let (uid, gid) = (self.account.uid, self.account.gid);
        rooted::write(&role_dir(&self.config), &relative, contents, uid, gid)
    }

    /// Copies the role's credential into the launch's coordinator state.
    fn place_credential(&self, launch: &Launch, credential: &[u8]) -> Result<()> {
        let run = self.below_role(&launch.run)?;
        let owner = (self.account.uid, self.account.gid);
        let base = role_dir(&self.config);
        place_credential(&base, &run, &self.binding, credential, owner)
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
        &self.binding.project_id
    }

    fn next(&mut self, role: Role) -> Result<Value> {
        let name = format!("{role:?}").to_lowercase();
        let call = crate::shadow::fetch_next(&self.client, &self.binding.project_id, &name);
        self.runtime.block_on(call)
    }

    fn free_bytes(&self) -> Result<u64> {
        free_bytes(&self.config.state_dir)
    }

    /// Fetches the mirror, clones its branch head, prepares `$RUN`, and
    /// copies the role's coordinator credential into the run's state where
    /// the binding's CLI looks for it.
    fn create(&mut self, launch: &Launch) -> Result<()> {
        let mirror = mirror(&self.config);
        clone::git_output(&mirror, &["fetch", "--prune", "--quiet"])?;
        let head = format!("refs/heads/{}", self.config.run.branch);
        let sha = clone::git_output(&mirror, &["rev-parse", "--verify", &head])?;
        self.revision = sha.clone();
        let origin = clone::git_output(&mirror, &["config", "--get", "remote.origin.url"])?;
        let program = crate::relay::program(&self.config);
        let clone = vec![
            format!("--url={}", mirror.display()),
            format!("--revision={sha}"),
            format!("--dest={}", launch.clone.display()),
            format!("--origin-url={origin}"),
            format!("--user-name={}", self.config.run.git_name),
            format!("--user-email={}", self.config.run.git_email),
        ];
        let root = Path::new("/");
        self.as_role(
            &program,
            &self.supervisor_args("clone", clone),
            root,
            launch,
        )?;
        let prepare = self.supervisor_args("prepare", spec_flags(&self.config, launch));
        self.as_role(&program, &prepare, root, launch)?;
        let credential = read_credentials(&self.config, &self.account)?;
        self.place_credential(launch, &credential)
    }

    fn instructions(&mut self, _launch: &Launch) -> Vec<(String, String)> {
        let mirror = mirror(&self.config);
        let read = |name: &&str| {
            Some((
                (*name).to_owned(),
                instruction(&mirror, &self.revision, name)?,
            ))
        };
        super::INSTRUCTION_FILES.iter().filter_map(read).collect()
    }

    fn install_prompt(&mut self, launch: &Launch, prompt: &str) -> Result<()> {
        let name = crate::profile::run_files::PROMPT;
        self.write_run_file(launch, name, prompt.as_bytes())
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
        let args = checkout_args(launch, lease, &self.revision);
        self.cli(launch, &args).map(drop)
    }

    /// Spawns `launch-root` as root in a process group of its own.
    fn start(&mut self, launch: &Launch) -> Result<(u32, Option<u64>)> {
        use std::os::unix::process::CommandExt;
        let args = self.supervisor_args("launch-root", spec_flags(&self.config, launch));
        let program = crate::relay::program(&self.config);
        let mut command = Command::new(&program);
        command.args(&args).stdin(Stdio::null()).process_group(0);
        let child = command.spawn().context("spawn launch-root")?;
        let pid = child.id();
        self.child = Some(child);
        Ok((pid, record::start_ticks(pid)))
    }

    /// Reaps `launch-root` once it has exited; a wait error keeps waiting.
    fn exited(&mut self) -> Option<i32> {
        let status = match self.child.as_mut()?.try_wait() {
            Ok(status) => status?,
            Err(error) => {
                eprintln!("agentc-supervisor run: wait for launch-root: {error}");
                return None;
            }
        };
        self.child = None;
        Some(status.code().unwrap_or(-1))
    }

    /// Signals `launch-root`'s process group while it is unreaped, so the
    /// group id cannot have been reused.
    fn signal(&mut self, kill: bool) {
        let Some(group) = self
            .child
            .as_ref()
            .and_then(|c| libc::pid_t::try_from(c.id()).ok())
        else {
            return;
        };
        let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: kill takes plain integers and touches no memory.
        unsafe { libc::kill(-group, signal) };
    }

    /// The modification time of `$RUN/events.jsonl`, the harness's event
    /// stream, which `launch` creates as the implementer. The agent could
    /// touch it, but renewal stays bounded by the budget and the service's
    /// `max_attempt_seconds`.
    fn last_event_ms(&self, launch: &Launch) -> Option<i64> {
        let relative = self.below_role(&launch.run).ok()?.join("events.jsonl");
        let modified =
            rooted::modified(&role_dir(&self.config), &relative, self.account.uid).ok()?;
        let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(since.as_millis()).ok()
    }

    fn renew(&mut self, launch: &Launch, lease: &Lease) -> Result<Lease> {
        let args = ["--json", "renew", &format!("--attempt={}", lease.attempt)];
        let generation = format!("--generation={}", lease.generation);
        let args = [&args.map(String::from)[..], &[generation]].concat();
        Lease::parse(&self.cli(launch, &args)?)
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
        let output = self.run_role(&cli, &args, &launch.clone, launch, &input)?;
        let code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success() || code == Some(CONFLICT_EXIT),
            "release failed: {}",
            stderr.trim()
        );
        Ok(())
    }

    fn boot_id(&self) -> String {
        record::boot_id()
    }

    fn may_be_alive(&self, launch: &LaunchRecord) -> bool {
        let now = self.now_ms();
        record::may_be_alive(launch, &record::boot_id(), record::start_ticks, now)
    }

    fn has_marker(&self, launch: &Launch, name: &str) -> bool {
        let base = role_dir(&self.config);
        let relative = self.below_role(&launch.run).map(|run| run.join(name));
        relative.is_ok_and(|relative| rooted::is_regular(&base, &relative))
    }

    /// Removes the clone and `$RUN` without following a role-owned symlink.
    fn discard(&mut self, launch: &Launch) -> Result<()> {
        let base = role_dir(&self.config);
        for path in [&launch.clone, &launch.run] {
            rooted::remove_tree(&base, &self.below_role(path)?)?;
        }
        Ok(())
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

    fn reviewer(&mut self) -> Option<&mut dyn ReviewDriver> {
        self.reviewer.as_mut().map(|r| r as &mut dyn ReviewDriver)
    }

    fn harness_status(&mut self, harness: Harness) -> Result<()> {
        super::live_health::status(&self.config, &self.account, harness)
    }

    fn credential_expiry_ms(&self, harness: Harness) -> Option<i64> {
        super::live_health::expiry_ms(&self.config, harness)
    }

    fn events(&self, launch: &Launch) -> Option<Vec<u8>> {
        super::live_health::events(&self.config, &self.account, launch)
    }
}

/// Whether SIGTERM or SIGINT asked the loop to stop.
pub(super) fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The reviewer side, built only when `[run] reviewer` is on, working
/// under the same binding as the implementer side.
fn reviewer(
    config: &Config,
    path: Option<&Path>,
    binding: &Binding,
    insecure: bool,
) -> Result<Option<LiveReviewer>> {
    if !config.run.reviewer {
        return Ok(None);
    }
    let arg: Vec<String> = path
        .map(|p| format!("--config={}", p.display()))
        .into_iter()
        .collect();
    LiveReviewer::new(config, &arg, binding, insecure).map(Some)
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

/// Writes `credential` below `base` into the run `run` (relative to `base`)
/// at `state/coordinator/` plus [`Binding::credentials`], creating a
/// `project_name` directory chain first, all owned by `owner` (uid, gid)
/// and reached without following symlinks.
fn place_credential(
    base: &Path,
    run: &Path,
    binding: &Binding,
    credential: &[u8],
    owner: (u32, u32),
) -> Result<()> {
    let home = run.join("state/coordinator");
    if let Some(name) = &binding.project_name {
        for directory in [home.join(name), home.join(name).join("config")] {
            rooted::make_dir(base, &directory, owner.0, owner.1)?;
        }
    }
    let target = home.join(binding.credentials());
    rooted::write(base, &target, credential, owner.0, owner.1)
}

/// The host mirror clones come from, `<state_dir>/mirror.git`.
pub(super) fn mirror(config: &Config) -> PathBuf {
    config.state_dir.join("mirror.git")
}

/// Writes the binding to [`binding::installed_path`] (root-owned, mode
/// 0644 so the role can read it), replacing it atomically. Renewals,
/// releases and a `[run.binding]` launch's own CLI then never read the
/// clone's role-writable `.agent-coordinator.toml`.
fn install_binding(config: &Config, text: &str) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = binding::installed_path(config);
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
    std::fs::rename(&temp, &path).with_context(|| format!("replace {}", path.display()))
}

/// The implementer's coordinator credential file, relative to its role
/// directory.
const CREDENTIALS: &str = "coordinator/credentials.toml";
/// The largest instruction blob read from the mirror.
const MAX_BLOB: usize = 1024 * 1024;

/// `<state_dir>/impl`, root-owned.
fn role_dir(config: &Config) -> PathBuf {
    config.state_dir.join(Role::Implementer.slug())
}

/// The implementer's credential file, read without following symlinks.
fn read_credentials(config: &Config, account: &Account) -> Result<Vec<u8>> {
    let relative = Path::new(CREDENTIALS);
    rooted::read(
        &role_dir(config),
        relative,
        account.uid,
        rooted::MAX_CREDENTIALS,
    )
}

/// `text` cut to at most `max` bytes on a character boundary.
fn truncate(mut text: String, max: usize) -> String {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// The launch's coordinator state, `$RUN/state/coordinator`.
fn coordinator_home(launch: &Launch) -> PathBuf {
    crate::confine::StatePaths::new(&launch.run).coordinator
}

/// The environment role commands get: the pinned binaries, the role's home,
/// the egress proxy, the launch's coordinator home and session, the
/// installed binding, and the insecure-loopback flag when `insecure`.
fn role_env(config: &Config, launch: &Launch, insecure: bool) -> Vec<(&'static str, String)> {
    let proxy = config.egress_proxy_url();
    let home = config.state_dir.join(Role::Implementer.slug()).join("home");
    let mut env = vec![
        (
            "PATH",
            format!("{}:/usr/bin:/bin", config.bin_dir.display()),
        ),
        ("HOME", home.display().to_string()),
        ("LANG", "C.UTF-8".into()),
        ("NO_PROXY", "127.0.0.1,localhost".into()),
        ("HTTPS_PROXY", proxy.clone()),
        ("https_proxy", proxy),
        (
            "AGENT_COORDINATOR_HOME",
            coordinator_home(launch).display().to_string(),
        ),
        ("AGENT_COORDINATOR_SESSION", launch.session_id.to_string()),
        (
            binding::REPO_CONFIG_ENV,
            binding::installed_path(config).display().to_string(),
        ),
    ];
    if insecure {
        env.push((binding::INSECURE_ENV, "true".into()));
    }
    env
}

/// The launch arguments `prepare` and `launch-root` share, in the
/// `--name=value` form no value can turn into another option.
fn spec_flags(_config: &Config, launch: &Launch) -> Vec<String> {
    let vendor = &launch.vendor;
    vec![
        "--role=implementer".into(),
        format!("--harness={:?}", vendor.harness).to_lowercase(),
        format!("--clone={}", launch.clone.display()),
        format!("--run={}", launch.run.display()),
        format!("--model={}", vendor.model),
        format!("--effort={}", vendor.effort),
        format!("--project={}", launch.project),
        format!("--task={}", launch.suggestion.task),
        format!("--session-id={}", launch.session_id),
    ]
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

/// Bytes available to unprivileged users on the filesystem holding `path`.
#[allow(clippy::useless_conversion)] // The field types differ between targets.
fn free_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: statvfs is plain old data; all-zero is a valid value to overwrite.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `stats` is live, writable storage.
    let status = unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) };
    let error = std::io::Error::last_os_error();
    ensure!(status == 0, "statvfs {}: {error}", path.display());
    Ok(u64::from(stats.f_bavail).saturating_mul(u64::from(stats.f_frsize)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_loop::Suggestion;

    #[test]
    fn spec_flags_cannot_become_options() {
        let suggestion = Suggestion {
            task: "-x".into(),
            revision: 1,
            title: String::new(),
        };
        let launch = Launch::plan(&Config::default(), "p", suggestion);
        let flags = spec_flags(&Config::default(), &launch);
        assert!(flags.iter().all(|flag| flag.starts_with("--")));
        assert!(flags.contains(&"--task=-x".to_owned()));
        assert!(flags.contains(&"--harness=claude".to_owned()));
    }

    #[test]
    fn role_commands_use_the_launch_session_and_proxy() {
        let suggestion = Suggestion {
            task: "t".into(),
            revision: 1,
            title: String::new(),
        };
        let launch = Launch::plan(&Config::default(), "p", suggestion);
        let env = role_env(&Config::default(), &launch, false);
        let session = launch.session_id.to_string();
        assert!(env.contains(&("AGENT_COORDINATOR_SESSION", session)));
        assert!(env.contains(&("HTTPS_PROXY", "http://127.0.0.1:3128".into())));
        assert!(env.iter().all(|(name, _)| !name.contains("TOKEN")));
        let binding = "/var/lib/agentc/coordinator-binding.toml".to_owned();
        assert!(env.contains(&("AGENT_COORDINATOR_REPO_CONFIG", binding)));
        assert!(env.iter().all(|(name, _)| *name != binding::INSECURE_ENV));
        let insecure = role_env(&Config::default(), &launch, true);
        assert!(insecure.contains(&(binding::INSECURE_ENV, "true".into())));
    }

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
    fn credentials_land_where_the_bindings_cli_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let run = Path::new("runs/s1");
        std::fs::create_dir_all(base.join(run).join("state/coordinator")).unwrap();
        // SAFETY: getuid and getgid take no arguments and touch no memory.
        let owner = unsafe { (libc::getuid(), libc::getgid()) };
        let mut binding = Binding {
            service_url: "http://127.0.0.1:18080".into(),
            project_id: "p".into(),
            project_name: None,
        };
        place_credential(&base, run, &binding, b"legacy", owner).unwrap();
        let home = base.join(run).join("state/coordinator");
        assert_eq!(
            std::fs::read(home.join("credentials.toml")).unwrap(),
            b"legacy"
        );
        binding.project_name = Some("Staging".into());
        place_credential(&base, run, &binding, b"named", owner).unwrap();
        let named = home.join("Staging/config/credentials.toml");
        assert_eq!(std::fs::read(named).unwrap(), b"named");
        std::fs::remove_dir_all(home.join("Staging")).unwrap();
        std::os::unix::fs::symlink(base.join("runs"), home.join("Staging")).unwrap();
        assert!(place_credential(&base, run, &binding, b"x", owner).is_err());
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
        install_binding(&config, "old").unwrap();
        install_binding(&config, "project_id = \"p\"").unwrap();
        let path = binding::installed_path(&config);
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

    #[test]
    fn free_bytes_reads_the_filesystem() {
        assert!(free_bytes(Path::new("/")).unwrap() > 0);
        assert!(free_bytes(Path::new("/no/such/path")).is_err());
    }
}
