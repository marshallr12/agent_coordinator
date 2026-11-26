//! The installer's part of a coordinator project as the supervisor's source
//! of work (phase 8, step 8a): the owner's coordinator inputs, the pinned
//! CLI beside the binary, the credential in a directory only the
//! supervisor's account may read, and the launch root the runner makes the
//! coordinator's launch directories in.
//!
//! The credential is handled as the push App's key is: the model holds the
//! path of the file the owner downloaded, never its bytes, and the copy has
//! its own access list from its first byte. The CLI is handled as the
//! binary is: copied into the program directory, whose access list it
//! inherits, and known by its SHA-256, which the manifest records.
use super::access::{ADMINISTRATORS, AccessList, FULL, MODIFY, READ, SYSTEM};
use super::install::Host;
use super::model::{DirectorySpec, Inputs, KeySpec, Layout, Sids, directory};
use super::plan::Refusal;
use anyhow::{Result, ensure};

/// The coordinator CLI's file name in the program directory.
pub const CLI: &str = "agent-coordinator.exe";

impl Layout {
    /// The coordinator's protected directory: the credential, the binding
    /// the supervisor writes and the CLI's session state.
    pub fn coordinator_dir(&self) -> String {
        self.state(r"supervisor\coordinator")
    }

    /// The coordinator credential.
    pub fn credential(&self) -> String {
        self.state(r"supervisor\coordinator\credentials.toml")
    }

    /// The installed coordinator CLI.
    pub fn cli(&self) -> String {
        self.program(CLI)
    }

    /// The directory the runner makes each coordinator launch's own in.
    pub fn coordinator_launch_root(&self) -> String {
        self.state(r"runner\fixture-launches")
    }
}

/// The coordinator CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSpec {
    /// Where the CLI is installed.
    pub path: String,
    /// The SHA-256, in hex, the installed CLI must have: the manifest's.
    pub sha256: String,
    /// The file this run copies the CLI from, when one was given.
    pub source: Option<String>,
}

/// What one `install` run's command line says about the coordinator. Each
/// `None` keeps what the manifest records, or the default on a first
/// install.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides {
    /// The coordinator's HTTPS origin.
    pub url: Option<String>,
    /// The project the supervisor claims from.
    pub project: Option<String>,
    /// This run's credential file. Giving one also asks for the credential
    /// to be installed.
    pub credential_source: Option<String>,
    /// This run's CLI file. `install_from` records its digest.
    pub cli_source: Option<String>,
    /// The command every coordinator launch runs.
    pub fixture: Option<Vec<String>>,
    /// Turns the coordinator off: every coordinator input is cleared.
    pub off: bool,
}

impl Overrides {
    /// Sets in `inputs` what this run's command line says.
    pub(super) fn apply(&self, inputs: &mut Inputs) {
        if self.off {
            return clear(inputs);
        }
        let keep = |given: &Option<String>, recorded: &mut String| {
            if let Some(value) = given {
                value.clone_into(recorded);
            }
        };
        keep(&self.url, &mut inputs.coordinator_url);
        keep(&self.project, &mut inputs.coordinator_project);
        if let Some(fixture) = &self.fixture {
            fixture.clone_into(&mut inputs.coordinator_fixture);
        }
        inputs.coordinator_credential |= self.credential_source.is_some();
    }
}

/// Clears every coordinator input, which turns the coordinator off.
fn clear(inputs: &mut Inputs) {
    inputs.coordinator_url.clear();
    inputs.coordinator_project.clear();
    inputs.coordinator_fixture.clear();
    inputs.coordinator_credential = false;
    inputs.coordinator_cli_sha256.clear();
}

/// Refuses coordinator inputs that are only partly given, an origin that is
/// not HTTPS, and a coordinator together with the acceptance stand-in.
pub(super) fn validate(inputs: &Inputs) -> Result<()> {
    if !whole(inputs)? {
        return Ok(());
    }
    let url = &inputs.coordinator_url;
    ensure!(
        url.starts_with("https://"),
        "the coordinator origin {url:?} is not an https:// origin (--coordinator-url)"
    );
    ensure!(
        inputs.stand_in_fixture.is_empty(),
        "the acceptance stand-in and a coordinator are never the source of work together: \
         give --no-stand-in to use the coordinator, or --no-coordinator to keep the stand-in"
    );
    Ok(())
}

/// Whether every coordinator input is given; `false` when none is, and an
/// error when only some are.
fn whole(inputs: &Inputs) -> Result<bool> {
    let given = [
        !inputs.coordinator_url.is_empty(),
        !inputs.coordinator_project.is_empty(),
        inputs.coordinator_credential,
        !inputs.coordinator_cli_sha256.is_empty(),
        !inputs.coordinator_fixture.is_empty(),
    ];
    if !given.contains(&true) {
        return Ok(false);
    }
    ensure!(
        !given.contains(&false),
        "a coordinator's origin, project, credential, CLI and command go together: give \
         --coordinator-url, --coordinator-project, --coordinator-credential-source, \
         --coordinator-cli-source and --coordinator-fixture, or none of them"
    );
    Ok(true)
}

/// Records in `inputs` the digest of this run's CLI file, when one was
/// given; or why the file cannot be used. A link, a directory and a missing
/// file are refused.
pub(super) fn record_cli(
    host: &dyn Host,
    overrides: &Overrides,
    inputs: &mut Inputs,
) -> Result<(), Refusal> {
    let Some(source) = &overrides.cli_source else {
        return Ok(());
    };
    inputs.coordinator_cli_sha256 = match host.sha256(source) {
        Ok(Some(digest)) => digest,
        Ok(None) => return Err(Refusal::CliSource(format!("{source} does not exist"))),
        Err(error) => return Err(Refusal::CliSource(format!("{error:#}"))),
    };
    Ok(())
}

/// The coordinator's protected directory and launch root, when there is a
/// coordinator. The supervisor may modify its protected directory, where
/// it writes the binding and the CLI keeps its state; the runner may modify
/// the launch root, as the stand-in's. Besides the system and
/// administrators, nobody else is granted anything on either.
pub(super) fn directories(layout: &Layout, inputs: &Inputs, sids: &Sids) -> Vec<DirectorySpec> {
    if !inputs.coordinator() {
        return Vec::new();
    }
    let (runner, supervisor) = (sids.runner.as_str(), sids.supervisor.as_str());
    vec![
        directory(layout.coordinator_dir(), &[(supervisor, MODIFY)], &[]),
        directory(
            layout.coordinator_launch_root(),
            &[(runner, MODIFY)],
            &[runner],
        ),
    ]
}

/// The credential's place and access list: the supervisor's account may
/// read it, the system and administrators have full control, nobody else
/// anything.
pub(super) fn credential(layout: &Layout, inputs: &Inputs, sids: &Sids) -> Option<KeySpec> {
    let grants = [
        (SYSTEM, FULL),
        (ADMINISTRATORS, FULL),
        (sids.supervisor.as_str(), READ),
    ];
    inputs.coordinator_credential.then(|| KeySpec {
        path: layout.credential(),
        access: AccessList::file(&grants),
        source: None,
    })
}

/// The CLI's place and the digest the manifest records for it.
pub(super) fn cli(layout: &Layout, inputs: &Inputs) -> Option<CliSpec> {
    inputs.coordinator().then(|| CliSpec {
        path: layout.cli(),
        sha256: inputs.coordinator_cli_sha256.clone(),
        source: None,
    })
}
