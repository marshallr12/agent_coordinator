//! The supervisor service's coordinator access: its credential, read from
//! the protected directory; an HTTP client for reads ([`ServiceApi`]); and
//! the pinned CLI for changes ([`ProcessCli`]).
//!
//! The token is never printed. [`Secret`]'s `Debug` hides it, an unreadable
//! credential file is reported without its text, and the token reaches the
//! CLI only as `AGENT_COORDINATOR_TOKEN` in the environment of a child of
//! the supervisor's own account, which the CLI prefers to any credential
//! file. The CLI's session proofs and mutation journals go to the
//! protected directory's `state`, through `--state-dir`.
use super::{Api, Cli, CliOutput, CoordinatorSettings};
use anyhow::{Context, Result, anyhow};
use coordinator_client::CoordinatorClient;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The variables the CLI reads its credential from.
const TOKEN: &str = "AGENT_COORDINATOR_TOKEN";
const ORIGIN: &str = "AGENT_COORDINATOR_ORIGIN";
/// The supervisor's own variables a CLI child keeps; every other one is
/// cleared.
pub(super) const KEPT: [&str; 6] = ["SystemRoot", "windir", "SystemDrive", "TEMP", "TMP", "PATH"];

/// A token, which `Debug` never shows.
#[derive(Clone)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The supervisor's coordinator credential.
#[derive(Debug)]
pub struct Access {
    origin: String,
    token: Secret,
}

impl Access {
    /// Reads the credential for `settings.service_url` from the protected
    /// directory and writes the binding file the CLI reads there.
    pub fn open(settings: &CoordinatorSettings) -> Result<Self> {
        let path = settings.credentials();
        let text = std::fs::read_to_string(&path)
            .map_err(|error| anyhow!("read {}: {}", path.display(), error.kind()))?;
        let (origin, token) =
            crate::shadow::credential_from(&text, &path, &settings.service_url, false)?;
        write_binding(settings)?;
        Ok(Self {
            origin,
            token: Secret(token),
        })
    }

    /// A client for the coordinator's reads.
    pub fn api(&self) -> Result<ServiceApi> {
        ServiceApi::new(&self.origin, &self.token.0)
    }

    /// The CLI of `settings`, run with this credential.
    pub fn cli(&self, settings: &CoordinatorSettings) -> ProcessCli {
        ProcessCli {
            program: settings.cli.clone(),
            state: settings.state(),
            binding: settings.binding_file(),
            origin: self.origin.clone(),
            token: self.token.clone(),
        }
    }
}

/// Writes the binding the CLI is pointed at with `--repo-config`.
fn write_binding(settings: &CoordinatorSettings) -> Result<()> {
    let path = settings.binding_file();
    let text = settings.binding().to_toml()?;
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
}

/// The coordinator's reads over HTTP.
pub struct ServiceApi {
    client: CoordinatorClient,
    runtime: tokio::runtime::Runtime,
}

impl ServiceApi {
    /// A client of `origin` with `token`, never over plain HTTP.
    fn new(origin: &str, token: &str) -> Result<Self> {
        let client = CoordinatorClient::new(origin, token.to_owned(), false)
            .map_err(|error| anyhow!("coordinator client: {error}"))?;
        let runtime = tokio::runtime::Runtime::new().context("start the client runtime")?;
        Ok(Self { client, runtime })
    }
}

impl Api for ServiceApi {
    fn next(&self, project: &str, exclude: &[String]) -> Result<Value> {
        let held = ("exclude_tasks", exclude);
        let call = crate::shadow::fetch_next(&self.client, project, "implementer", held);
        self.runtime.block_on(call)
    }

    fn task(&self, project: &str, task: &str) -> Result<Value> {
        let path = format!("/api/v1/projects/{project}/tasks/{task}");
        let call = crate::shadow::get_data(&self.client, &path, &[]);
        self.runtime.block_on(call)
    }
}

/// The pinned coordinator CLI, run as a child of the supervisor.
#[derive(Debug)]
pub struct ProcessCli {
    program: PathBuf,
    state: PathBuf,
    binding: PathBuf,
    origin: String,
    token: Secret,
}

impl ProcessCli {
    /// The arguments every call starts with: JSON output, the launch's
    /// session, the protected state and the written binding.
    fn global(&self, session: &str) -> Vec<String> {
        let state = format!("--state-dir={}", self.state.display());
        let binding = format!("--repo-config={}", self.binding.display());
        let session = format!("--session={session}");
        vec!["--json".to_owned(), session, state, binding]
    }

    /// The command for `args` in `session`, with a cleared environment
    /// holding only the kept variables and the credential.
    pub(super) fn command(&self, session: &str, args: &[String]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(self.global(session)).args(args).env_clear();
        let kept = KEPT.iter().filter_map(|k| Some((*k, std::env::var_os(k)?)));
        command.envs(kept);
        command.env(TOKEN, &self.token.0).env(ORIGIN, &self.origin);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        no_window(&mut command);
        command
    }
}

impl Cli for ProcessCli {
    /// Runs the CLI to its end; `input` is written whole, then standard
    /// input is closed.
    fn run(&self, session: &str, args: &[String], input: &[u8]) -> Result<CliOutput> {
        let shown = self.program.display();
        let mut child =
            (self.command(session, args).spawn()).with_context(|| format!("start {shown}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input).context("write the CLI's input")?;
        }
        let output = child.wait_with_output().context("wait for the CLI")?;
        Ok(CliOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// A CLI child gets no console window of its own.
#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_command: &mut Command) {}
