//! The live [`Driver`]: runs as root, reads `next` with the implementer's
//! coordinator credential, refreshes the host mirror, and runs every step
//! that touches the role's files (`clone`, `prepare`, the coordinator CLI) as
//! the role account. `launch-root` itself runs as root, as by hand.
use super::{Driver, Launch};
use crate::clone;
use crate::config::Config;
use crate::profile::Role;
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, ensure};
use coordinator_client::CoordinatorClient;
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The repository binding the mirror's branch carries.
#[derive(Deserialize)]
struct Binding {
    service_url: String,
    project_id: String,
}

/// Talks to the coordinator and runs commands for one host.
pub struct LiveDriver {
    config: Config,
    /// Passed on to `agentc-supervisor` children as `--config`.
    config_arg: Vec<String>,
    runtime: tokio::runtime::Runtime,
    client: CoordinatorClient,
    project: String,
    account: Account,
}

/// Runs the live loop as root until stopped (or once).
pub fn run(config: &Config, config_path: Option<&Path>, once: bool) -> Result<()> {
    accounts::require_root(accounts::effective_uid())?;
    let mut driver = LiveDriver::new(config, config_path)?;
    super::run(&mut driver, config, once)
}

impl LiveDriver {
    /// Resolves the project from the mirror's binding and connects with the
    /// implementer's credential file.
    fn new(config: &Config, config_path: Option<&Path>) -> Result<Self> {
        let spec = format!("{}:.agent-coordinator.toml", config.run.branch);
        let text = clone::git_output(&mirror(config), &["show", &spec])?;
        let binding: Binding = toml::from_str(&text).context("parse the mirror's binding")?;
        let credentials = credential_file(config);
        let insecure = config.run.allow_insecure_loopback;
        Ok(Self {
            config: config.clone(),
            config_arg: config_path
                .map(|p| format!("--config={}", p.display()))
                .into_iter()
                .collect(),
            runtime: tokio::runtime::Runtime::new()?,
            client: crate::shadow::client(&credentials, &binding.service_url, insecure)?,
            project: binding.project_id,
            account: Account::lookup(Role::Implementer.user(config))?,
        })
    }

    /// Runs `program args` as the implementer in `cwd`; fails with its
    /// stderr on a non-zero exit.
    fn as_role(&self, program: &Path, args: &[String], cwd: &Path, launch: &Launch) -> Result<()> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::null());
        command.envs(role_env(&self.config, launch));
        accounts::run_as(&mut command, &self.account);
        let output = command
            .output()
            .with_context(|| format!("run {}", program.display()))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.join(" "),
            stderr.trim()
        );
        Ok(())
    }

    /// `agentc-supervisor` arguments: `--config`, `subcommand`, then `rest`.
    fn supervisor_args(&self, subcommand: &str, rest: Vec<String>) -> Vec<String> {
        let mut args = self.config_arg.clone();
        args.push(subcommand.into());
        args.extend(rest);
        args
    }

    /// Writes `contents` to a new role-owned mode 0600 file at `path`.
    fn write_owned(&self, path: &Path, contents: &[u8]) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(contents)?;
        std::os::unix::fs::fchown(&file, Some(self.account.uid), Some(self.account.gid))?;
        Ok(file.sync_all()?)
    }
}

impl Driver for LiveDriver {
    fn project(&self) -> &str {
        &self.project
    }

    fn next(&mut self, role: Role) -> Result<Value> {
        let name = format!("{role:?}").to_lowercase();
        let call = crate::shadow::fetch_next(&self.client, &self.project, &name);
        self.runtime.block_on(call)
    }

    fn free_bytes(&self) -> Result<u64> {
        free_bytes(&self.config.state_dir)
    }

    /// Fetches the mirror, clones its branch head, prepares `$RUN`, and
    /// copies the role's coordinator credential into the run's state.
    fn create(&mut self, launch: &Launch) -> Result<()> {
        let mirror = mirror(&self.config);
        clone::git_output(&mirror, &["fetch", "--prune", "--quiet"])?;
        let head = format!("refs/heads/{}", self.config.run.branch);
        let sha = clone::git_output(&mirror, &["rev-parse", "--verify", &head])?;
        let origin = clone::git_output(&mirror, &["config", "--get", "remote.origin.url"])?;
        let program = crate::relay::program(&self.config);
        let clone = vec![
            format!("--url={}", mirror.display()),
            format!("--revision={sha}"),
            format!("--dest={}", launch.clone.display()),
            format!("--origin-url={origin}"),
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
        let credential = std::fs::read(credential_file(&self.config))?;
        self.write_owned(
            &coordinator_home(launch).join("credentials.toml"),
            &credential,
        )
    }

    fn install_prompt(&mut self, launch: &Launch, prompt: &str) -> Result<()> {
        let path = launch.run.join(crate::profile::run_files::PROMPT);
        self.write_owned(&path, prompt.as_bytes())
    }

    /// Connects the launch's session and claims the task with the pinned CLI,
    /// which acknowledges the current orientation first.
    fn claim(&mut self, launch: &Launch) -> Result<()> {
        let cli = self.config.bin_dir.join("agent-coordinator");
        let harness = format!("--harness=agentc-supervisor-{:?}", self.config.run.harness);
        let connect = ["--json".into(), "connect".into(), harness.to_lowercase()];
        self.as_role(&cli, &connect, &launch.clone, launch)?;
        let s = &launch.suggestion;
        let claim = ["--json", "claim", &format!("--task={}", s.task)].map(String::from);
        let revision = format!("--revision={}", s.revision);
        self.as_role(
            &cli,
            &[&claim[..], &[revision]].concat(),
            &launch.clone,
            launch,
        )
    }

    /// Runs `launch-root` as root and returns its exit code.
    fn launch(&mut self, launch: &Launch) -> Result<i32> {
        let args = self.supervisor_args("launch-root", spec_flags(&self.config, launch));
        let program = crate::relay::program(&self.config);
        let status = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .status()?;
        Ok(status.code().unwrap_or(-1))
    }
}

/// The host mirror clones come from, `<state_dir>/mirror.git`.
fn mirror(config: &Config) -> PathBuf {
    config.state_dir.join("mirror.git")
}

/// The implementer's coordinator credential file.
fn credential_file(config: &Config) -> PathBuf {
    let role = config.state_dir.join(Role::Implementer.slug());
    role.join("coordinator/credentials.toml")
}

/// The launch's coordinator state, `$RUN/state/coordinator`.
fn coordinator_home(launch: &Launch) -> PathBuf {
    crate::confine::StatePaths::new(&launch.run).coordinator
}

/// The environment role commands get: the pinned binaries, the role's home,
/// the egress proxy, and the launch's coordinator home and session.
fn role_env(config: &Config, launch: &Launch) -> Vec<(&'static str, String)> {
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
    ];
    if config.run.allow_insecure_loopback {
        env.push(("AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK", "true".into()));
    }
    env
}

/// The launch arguments `prepare` and `launch-root` share, in the
/// `--name=value` form no value can turn into another option.
fn spec_flags(config: &Config, launch: &Launch) -> Vec<String> {
    let run = &config.run;
    vec![
        "--role=implementer".into(),
        format!("--harness={:?}", run.harness).to_lowercase(),
        format!("--clone={}", launch.clone.display()),
        format!("--run={}", launch.run.display()),
        format!("--model={}", run.model),
        format!("--effort={}", run.effort),
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
        let env = role_env(&Config::default(), &launch);
        let session = launch.session_id.to_string();
        assert!(env.contains(&("AGENT_COORDINATOR_SESSION", session)));
        assert!(env.contains(&("HTTPS_PROXY", "http://127.0.0.1:3128".into())));
        assert!(env.iter().all(|(name, _)| !name.contains("TOKEN")));
    }

    #[test]
    fn free_bytes_reads_the_filesystem() {
        assert!(free_bytes(Path::new("/")).unwrap() > 0);
        assert!(free_bytes(Path::new("/no/such/path")).is_err());
    }
}
