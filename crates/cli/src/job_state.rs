use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::config::coordinator_home;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProgramInput {
    pub label: String,
    pub program: PathBuf,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default = "default_log_limit")]
    pub log_limit_bytes: u64,
}

fn default_log_limit() -> u64 {
    1_048_576
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunIntent {
    pub service_origin: String,
    pub project_id: String,
    pub session_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub reservation_id: String,
    pub checkout: PathBuf,
    pub source_revision: String,
    pub source_tree: String,
    pub input: ProgramInput,
    pub renew_for_seconds: u16,
    pub watch_pid: Option<u32>,
    pub job_id: String,
    pub producer_id: String,
    pub runner_instance_id: String,
    pub reporter_id: String,
    pub reporter_proof: String,
    pub state_file: PathBuf,
    #[serde(default)]
    pub initialized: bool,
}

pub struct NewRunIntent<'a> {
    pub service_origin: &'a str,
    pub project_id: &'a str,
    pub session_id: &'a str,
    pub attempt_id: &'a str,
    pub generation: u64,
    pub reservation_id: &'a str,
    pub checkout: &'a Path,
    pub source_revision: &'a str,
    pub source_tree: &'a str,
    pub input: ProgramInput,
    pub renew_for_seconds: u16,
    pub watch_pid: Option<u32>,
    pub job_id: String,
    pub producer_id: String,
    pub runner_instance_id: String,
    pub reporter_id: String,
    pub reporter_proof: String,
}

pub fn load_or_create(request: NewRunIntent<'_>) -> Result<(RunIntent, bool)> {
    validate_program(&request.input)?;
    let key = request_key(&request)?;
    let path = coordinator_home()?
        .join("job-intents")
        .join(format!("{key}.json"));
    if let Some(existing) = load(&path)? {
        let desired = comparable(&request);
        if comparable_existing(&existing) != desired {
            bail!("saved job intent does not match this request");
        }
        return Ok((existing, false));
    }
    let state_file = coordinator_home()?
        .join("jobs")
        .join(&request.job_id)
        .join("state.json");
    let intent = RunIntent {
        service_origin: request.service_origin.to_owned(),
        project_id: request.project_id.to_owned(),
        session_id: request.session_id.to_owned(),
        attempt_id: request.attempt_id.to_owned(),
        generation: request.generation,
        reservation_id: request.reservation_id.to_owned(),
        checkout: request.checkout.to_path_buf(),
        source_revision: request.source_revision.to_owned(),
        source_tree: request.source_tree.to_owned(),
        input: request.input,
        renew_for_seconds: request.renew_for_seconds,
        watch_pid: request.watch_pid,
        job_id: request.job_id,
        producer_id: request.producer_id,
        runner_instance_id: request.runner_instance_id,
        reporter_id: request.reporter_id,
        reporter_proof: request.reporter_proof,
        state_file,
        initialized: false,
    };
    save(&path, &intent)?;
    Ok((intent, true))
}

pub fn mark_initialized(intent: &mut RunIntent) -> Result<()> {
    if intent.initialized {
        return Ok(());
    }
    intent.initialized = true;
    let path = path_for_intent(intent)?;
    save(&path, intent)
}

pub fn load_by_job(job_id: &str) -> Result<RunIntent> {
    let directory = coordinator_home()?.join("job-intents");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("no local job state exists for job {job_id}")
        }
        Err(error) => return Err(error).context("read local job intents"),
    };
    let mut found = None;
    for entry in entries {
        let entry = entry.context("read local job intent entry")?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let intent = load(&entry.path())?.ok_or_else(|| anyhow!("job intent disappeared"))?;
        if intent.job_id == job_id {
            if found.is_some() {
                bail!("more than one local job intent names job {job_id}");
            }
            found = Some(intent);
        }
    }
    found.ok_or_else(|| anyhow!("no local job state exists for job {job_id}"))
}

pub fn read_program(path: &Path) -> Result<ProgramInput> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("read job JSON input {}", path.display()))?;
    let value: ProgramInput = serde_json::from_str(&input).context("parse job JSON input")?;
    validate_program(&value)?;
    Ok(value)
}

fn validate_program(input: &ProgramInput) -> Result<()> {
    if input.label.trim().is_empty() {
        bail!("job label must not be empty");
    }
    if !input.program.is_absolute() {
        bail!("job program must be an absolute native path");
    }
    if input.log_limit_bytes == 0 || input.log_limit_bytes > 100 * 1024 * 1024 {
        bail!("log_limit_bytes must be between 1 and 104857600");
    }
    if input
        .environment
        .keys()
        .any(|name| name.to_ascii_uppercase().starts_with("AGENT_COORDINATOR_"))
    {
        bail!("job environment must not contain AGENT_COORDINATOR_* variables");
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct Comparable<'a> {
    service_origin: &'a str,
    project_id: &'a str,
    session_id: &'a str,
    attempt_id: &'a str,
    generation: u64,
    reservation_id: &'a str,
    checkout: &'a Path,
    source_revision: &'a str,
    source_tree: &'a str,
    input: &'a ProgramInput,
    renew_for_seconds: u16,
    watch_pid: Option<u32>,
}

fn comparable<'a>(request: &'a NewRunIntent<'a>) -> Comparable<'a> {
    Comparable {
        service_origin: request.service_origin,
        project_id: request.project_id,
        session_id: request.session_id,
        attempt_id: request.attempt_id,
        generation: request.generation,
        reservation_id: request.reservation_id,
        checkout: request.checkout,
        source_revision: request.source_revision,
        source_tree: request.source_tree,
        input: &request.input,
        renew_for_seconds: request.renew_for_seconds,
        watch_pid: request.watch_pid,
    }
}

fn comparable_existing(intent: &RunIntent) -> Comparable<'_> {
    Comparable {
        service_origin: &intent.service_origin,
        project_id: &intent.project_id,
        session_id: &intent.session_id,
        attempt_id: &intent.attempt_id,
        generation: intent.generation,
        reservation_id: &intent.reservation_id,
        checkout: &intent.checkout,
        source_revision: &intent.source_revision,
        source_tree: &intent.source_tree,
        input: &intent.input,
        renew_for_seconds: intent.renew_for_seconds,
        watch_pid: intent.watch_pid,
    }
}

fn request_key(request: &NewRunIntent<'_>) -> Result<String> {
    #[derive(Serialize)]
    struct Key<'a> {
        service_origin: &'a str,
        project_id: &'a str,
        session_id: &'a str,
        attempt_id: &'a str,
        generation: u64,
        reservation_id: &'a str,
        checkout: &'a Path,
        source_revision: &'a str,
        source_tree: &'a str,
        input: &'a ProgramInput,
        renew_for_seconds: u16,
        watch_pid: Option<u32>,
    }
    let bytes = serde_json::to_vec(&Key {
        service_origin: request.service_origin,
        project_id: request.project_id,
        session_id: request.session_id,
        attempt_id: request.attempt_id,
        generation: request.generation,
        reservation_id: request.reservation_id,
        checkout: request.checkout,
        source_revision: request.source_revision,
        source_tree: request.source_tree,
        input: &request.input,
        renew_for_seconds: request.renew_for_seconds,
        watch_pid: request.watch_pid,
    })
    .context("serialize job intent identity")?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn path_for_intent(intent: &RunIntent) -> Result<PathBuf> {
    let request = NewRunIntent {
        service_origin: &intent.service_origin,
        project_id: &intent.project_id,
        session_id: &intent.session_id,
        attempt_id: &intent.attempt_id,
        generation: intent.generation,
        reservation_id: &intent.reservation_id,
        checkout: &intent.checkout,
        source_revision: &intent.source_revision,
        source_tree: &intent.source_tree,
        input: intent.input.clone(),
        renew_for_seconds: intent.renew_for_seconds,
        watch_pid: intent.watch_pid,
        job_id: intent.job_id.clone(),
        producer_id: intent.producer_id.clone(),
        runner_instance_id: intent.runner_instance_id.clone(),
        reporter_id: intent.reporter_id.clone(),
        reporter_proof: intent.reporter_proof.clone(),
    };
    Ok(coordinator_home()?
        .join("job-intents")
        .join(format!("{}.json", request_key(&request)?)))
}

fn load(path: &Path) -> Result<Option<RunIntent>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse local job intent {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read local job intent {}", path.display()))
        }
    }
}

fn save(path: &Path, intent: &RunIntent) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("job intent path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create job intent directory {}", parent.display()))?;
    protect_directory(parent)?;
    let bytes = serde_json::to_vec_pretty(intent).context("serialize job intent")?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary job intent in {}", parent.display()))?;
    protect_file(temporary.path())?;
    temporary.write_all(&bytes).context("write job intent")?;
    temporary.as_file().sync_all().context("sync job intent")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace job intent {}", path.display()))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open job intent directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync job intent directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect job intent directory {}", path.display()))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect job intent file {}", path.display()))
}

#[cfg(not(unix))]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
}
