//! The Linux host runtime: the supervisor runs as root and every step that
//! touches the implementer's files (`clone`, `prepare`, the coordinator CLI)
//! runs as the role account. `launch-root` itself runs as root in a process
//! group of its own, which a stop request or a kill signals as a whole; it
//! starts the candidate-push helper beside the launch and reaps what the
//! harness leaves behind ([`crate::push_helper`], [`crate::reaper`]). Root's
//! own reads and writes below the role's directories go through [`rooted`].
//!
//! Nothing here decides what to launch or when: that is the loop's, through
//! [`HostRuntime`].
use super::{HostRuntime, LaunchIdentity, Observation, Recovery, StartIntent, rooted};
use crate::clone;
use crate::config::Config;
use crate::profile::Role;
use crate::push_helper::accounts::{self, Account};
use crate::run_loop::Launch;
use crate::run_loop::binding::{self, Binding};
use crate::run_loop::record::{self, LaunchRecord};
use crate::run_loop::renewal::CliError;
use anyhow::{Context, Result, ensure};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// The implementer's coordinator credential file, relative to its role
/// directory.
const CREDENTIALS: &str = "coordinator/credentials.toml";

/// One project a host launches for: its binding and the mirror its clones
/// start from.
#[derive(Debug, Clone)]
pub struct Source {
    pub binding: Binding,
    pub mirror: PathBuf,
}

/// Runs one implementer launch at a time on a Linux host.
pub struct LinuxHost {
    config: Config,
    /// Passed on to `agentc-supervisor` children as `--config`.
    config_arg: Vec<String>,
    /// Whether coordinator calls may use plain HTTP to loopback.
    insecure: bool,
    account: Account,
    /// The projects served, the main one first.
    sources: Vec<Source>,
    /// The mirror revision the current launch was cloned at.
    revision: String,
    /// The running `launch-root`, if any.
    child: Option<Child>,
}

impl LinuxHost {
    /// A runtime that launches as `account` for `sources` (the main project
    /// first), handing `config_path` to the `agentc-supervisor` children.
    pub fn new(
        config: &Config,
        config_path: Option<&Path>,
        account: Account,
        insecure: bool,
        sources: Vec<Source>,
    ) -> Self {
        Self {
            config: config.clone(),
            config_arg: config_path
                .map(|p| format!("--config={}", p.display()))
                .into_iter()
                .collect(),
            insecure,
            account,
            sources,
            revision: String::new(),
            child: None,
        }
    }

    /// The role account launches run as.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// The mirror revision the current launch was cloned at (empty before
    /// the first [`HostRuntime::prepare`]).
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The source serving `project`; the main one if none does.
    fn source_of(&self, project: &str) -> &Source {
        (self.sources.iter())
            .find(|source| source.binding.project_id == project)
            .unwrap_or(&self.sources[0])
    }

    /// The mirror `project`'s clones start from.
    pub fn mirror_of(&self, project: &str) -> &Path {
        &self.source_of(project).mirror
    }

    /// Runs `program args` as the implementer in `cwd` and returns its
    /// stdout; on a non-zero exit fails with the exit code and the CLI's
    /// error code and message (or stderr), bounded.
    pub fn as_role(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        launch: &Launch,
    ) -> Result<Vec<u8>> {
        let output = self.run_role(program, args, cwd, launch, b"")?;
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.join(" "),
            CliError::of(&output)
        );
        Ok(output.stdout)
    }

    /// Runs `program args` as the implementer in `cwd` with `input` on its
    /// standard input and returns its output, whatever its exit.
    pub fn run_role(
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

    /// `path` relative to the implementer's directory: `$RUN` or the clone.
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

    /// Where the implementer's credential file is, for messages.
    pub fn credentials_path(&self) -> PathBuf {
        role_dir(&self.config).join(CREDENTIALS)
    }

    /// The implementer's credential file, read without following symlinks.
    pub fn read_credentials(&self) -> Result<Vec<u8>> {
        read_credentials(&self.config, &self.account)
    }

    /// Writes `contents` to a new role-owned mode 0600 file at `name` in
    /// the launch's `$RUN`, refusing symlinks on the way.
    pub fn write_run_file(&self, launch: &Launch, name: &str, contents: &[u8]) -> Result<()> {
        let relative = self.below_role(&launch.run)?.join(name);
        let (uid, gid) = (self.account.uid, self.account.gid);
        rooted::write(&role_dir(&self.config), &relative, contents, uid, gid)
    }

    /// Copies the role's credential into the launch's coordinator state.
    fn place_credential(&self, launch: &Launch, credential: &[u8]) -> Result<()> {
        let run = self.below_role(&launch.run)?;
        let owner = (self.account.uid, self.account.gid);
        let base = role_dir(&self.config);
        let binding = &self.source_of(&launch.project).binding;
        place_credential(&base, &run, binding, credential, owner)
    }

    /// Whether the launch's `$RUN` holds the regular file `name`, never
    /// following a symlink.
    pub fn has_marker(&self, launch: &Launch, name: &str) -> bool {
        let base = role_dir(&self.config);
        let relative = self.below_role(&launch.run).map(|run| run.join(name));
        relative.is_ok_and(|relative| rooted::is_regular(&base, &relative))
    }

    /// The modification time of `$RUN/events.jsonl`, the harness's event
    /// stream, which `launch` creates as the implementer (ms since the
    /// epoch). The agent could touch it, but renewal stays bounded by the
    /// budget and the service's `max_attempt_seconds`.
    pub fn last_event_ms(&self, launch: &Launch) -> Option<i64> {
        let relative = self.below_role(&launch.run).ok()?.join("events.jsonl");
        let modified =
            rooted::modified(&role_dir(&self.config), &relative, self.account.uid).ok()?;
        let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(since.as_millis()).ok()
    }

    /// Bytes available to unprivileged users on the state filesystem.
    pub fn free_bytes(&self) -> Result<u64> {
        free_bytes(&self.config.state_dir)
    }

    /// Clones `sha` of the launch's mirror as the role and prepares `$RUN`.
    fn clone_and_prepare(&self, launch: &Launch, mirror: &Path, sha: &str) -> Result<()> {
        let origin = clone::git_output(mirror, &["config", "--get", "remote.origin.url"])?;
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
        let clone = self.supervisor_args("clone", clone);
        self.as_role(&program, &clone, root, launch)?;
        let prepare = self.supervisor_args("prepare", spec_flags(&self.config, launch));
        self.as_role(&program, &prepare, root, launch).map(drop)
    }
}

impl HostRuntime for LinuxHost {
    /// Fetches the mirror, clones its branch head, prepares `$RUN`, and
    /// copies the role's coordinator credential into the run's state where
    /// the binding's CLI looks for it. The intent carries the boot id only,
    /// so the launch record on a Linux host is what it always was.
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent> {
        let mirror = self.mirror_of(&launch.project).to_owned();
        clone::git_output(&mirror, &["fetch", "--prune", "--quiet"])?;
        let head = format!("refs/heads/{}", self.config.run.branch);
        let sha = clone::git_output(&mirror, &["rev-parse", "--verify", &head])?;
        self.revision = sha.clone();
        self.clone_and_prepare(launch, &mirror, &sha)?;
        let credential = self.read_credentials()?;
        self.place_credential(launch, &credential)?;
        Ok(StartIntent {
            boot_id: record::boot_id(),
            ..StartIntent::default()
        })
    }

    /// Spawns `launch-root` as root in a process group of its own.
    fn start(&mut self, launch: &Launch) -> Result<LaunchIdentity> {
        use std::os::unix::process::CommandExt;
        let args = self.supervisor_args("launch-root", spec_flags(&self.config, launch));
        let program = crate::relay::program(&self.config);
        let mut command = Command::new(&program);
        command.args(&args).stdin(Stdio::null()).process_group(0);
        let child = command.spawn().context("spawn launch-root")?;
        let pid = child.id();
        self.child = Some(child);
        Ok(LaunchIdentity {
            pid,
            started: record::start_ticks(pid),
        })
    }

    /// Reaps `launch-root` once it has exited; a wait error keeps waiting.
    fn observe(&mut self, _launch: &Launch) -> Observation {
        let Some(child) = self.child.as_mut() else {
            return Observation::Running;
        };
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return Observation::Running,
            Err(error) => {
                eprintln!("agentc-supervisor run: wait for launch-root: {error}");
                return Observation::Running;
            }
        };
        self.child = None;
        Observation::Exited {
            code: status.code().unwrap_or(-1),
        }
    }

    /// SIGTERM to `launch-root`'s process group.
    fn request_stop(&mut self, _launch: &Launch) {
        self.signal(libc::SIGTERM);
    }

    /// SIGKILL to `launch-root`'s process group.
    fn terminate(&mut self, _launch: &Launch) {
        self.signal(libc::SIGKILL);
    }

    /// A record may still run when it was written on this boot and its pid
    /// lives with the recorded start time ([`record::may_be_alive`]).
    fn recover(&self, launch: &LaunchRecord, now_ms: i64) -> Recovery {
        let boot = record::boot_id();
        if record::may_be_alive(launch, &boot, record::start_ticks, now_ms) {
            Recovery::MayBeRunning
        } else {
            Recovery::Ended
        }
    }

    /// Removes the clone and `$RUN` without following a role-owned symlink.
    fn cleanup(&mut self, launch: &Launch) -> Result<()> {
        let base = role_dir(&self.config);
        for path in [&launch.clone, &launch.run] {
            rooted::remove_tree(&base, &self.below_role(path)?)?;
        }
        Ok(())
    }
}

impl LinuxHost {
    /// Signals `launch-root`'s process group while it is unreaped, so the
    /// group id cannot have been reused.
    fn signal(&mut self, signal: libc::c_int) {
        let Some(group) = self
            .child
            .as_ref()
            .and_then(|c| libc::pid_t::try_from(c.id()).ok())
        else {
            return;
        };
        // SAFETY: kill takes plain integers and touches no memory.
        unsafe { libc::kill(-group, signal) };
    }
}

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
            binding::installed_path_for(config, &launch.project)
                .display()
                .to_string(),
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

    /// A planned launch of task `task` in `project` under `config`.
    fn planned(config: &Config, project: &str, task: &str) -> Launch {
        let suggestion = Suggestion {
            task: task.into(),
            revision: 1,
            title: String::new(),
        };
        Launch::plan(config, project, suggestion)
    }

    #[test]
    fn spec_flags_cannot_become_options() {
        let launch = planned(&Config::default(), "p", "-x");
        let flags = spec_flags(&Config::default(), &launch);
        assert!(flags.iter().all(|flag| flag.starts_with("--")));
        assert!(flags.contains(&"--task=-x".to_owned()));
        assert!(flags.contains(&"--harness=claude".to_owned()));
    }

    #[test]
    fn role_commands_use_the_launch_session_and_proxy() {
        let launch = planned(&Config::default(), "p", "t");
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
    fn role_commands_of_a_canary_launch_read_the_canary_binding_copy() {
        let mut config = Config::default();
        config.run.canary_binding = Some(binding::CanaryBinding {
            service_url: "https://agents.example.com".into(),
            project_id: "canary".into(),
            project_name: None,
            mirror: None,
        });
        let repo = |project: &str| {
            let launch = planned(&config, project, "t");
            let env = role_env(&config, &launch, false);
            let (_, path) = env
                .into_iter()
                .find(|(name, _)| *name == binding::REPO_CONFIG_ENV)
                .unwrap();
            path
        };
        assert_eq!(repo("p"), "/var/lib/agentc/coordinator-binding.toml");
        assert_eq!(
            repo("canary"),
            "/var/lib/agentc/coordinator-binding-canary.toml"
        );
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
    fn free_bytes_reads_the_filesystem() {
        assert!(free_bytes(Path::new("/")).unwrap() > 0);
        assert!(free_bytes(Path::new("/no/such/path")).is_err());
    }
}
