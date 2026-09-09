use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use coordinator_client::{HttpMethod, SessionAuth};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::config::coordinator_home;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingMutation {
    pub key: String,
    pub method: HttpMethod,
    pub path: String,
    pub body: Value,
    #[serde(default = "default_include_session_id")]
    pub include_session_id: bool,
}

fn default_include_session_id() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrientationVersion {
    pub policy_revision: u64,
    pub instruction_version: String,
    pub sections: Vec<String>,
    #[serde(default)]
    pub instructions_complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionState {
    pub service_origin: String,
    pub project_id: String,
    pub local_session: String,
    pub credential_digest: String,
    pub session: SessionAuth,
    pub workstation_id: String,
    pub harness: String,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub orientation: Option<OrientationVersion>,
    #[serde(default)]
    pub acknowledged: Option<OrientationVersion>,
    #[serde(default)]
    pub pending: Option<PendingMutation>,
}

impl SessionState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        service_origin: String,
        project_id: String,
        local_session: String,
        credential_digest: String,
        session: SessionAuth,
        workstation_id: String,
        harness: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            service_origin,
            project_id,
            local_session,
            credential_digest,
            session,
            workstation_id,
            harness,
            capabilities,
            orientation: None,
            acknowledged: None,
            pending: None,
        }
    }

    pub fn set_pending(&mut self, mutation: PendingMutation) -> Result<()> {
        match &self.pending {
            Some(existing) if existing == &mutation => Ok(()),
            Some(existing) => bail!(
                "an earlier mutation is unresolved ({} {}); run `agent-coordinator retry --session {}` before another write",
                method_name(&existing.method),
                existing.path,
                self.local_session
            ),
            None => {
                self.pending = Some(mutation);
                Ok(())
            }
        }
    }
}

pub struct SessionLock {
    _file: File,
}

pub fn lock(state_path: &Path) -> Result<SessionLock> {
    let parent = state_path
        .parent()
        .ok_or_else(|| anyhow!("session state path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create local state directory {}", parent.display()))?;
    protect_directory(parent)?;
    let lock_path = state_path.with_extension("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open session lock {}", lock_path.display()))?;
    protect_file(&lock_path)?;
    file.try_lock().with_context(|| {
        format!(
            "session is busy in another process (lock {})",
            lock_path.display()
        )
    })?;
    Ok(SessionLock { _file: file })
}

fn method_name(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Patch => "PATCH",
    }
}

pub fn path_for(origin: &str, project_id: &str, local_session: &str) -> Result<PathBuf> {
    let mut digest = Sha256::new();
    digest.update(origin.as_bytes());
    digest.update([0]);
    digest.update(project_id.as_bytes());
    digest.update([0]);
    digest.update(local_session.as_bytes());
    let name = hex::encode(digest.finalize());
    Ok(coordinator_home()?
        .join("sessions")
        .join(format!("{name}.json")))
}

pub fn load(path: &Path) -> Result<Option<SessionState>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse local session state {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read local session state {}", path.display()))
        }
    }
}

pub fn save(path: &Path, state: &SessionState) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("session state path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create local state directory {}", parent.display()))?;
    protect_directory(parent)?;

    let bytes = serde_json::to_vec_pretty(state).context("serialize local session state")?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary state file in {}", parent.display()))?;
    protect_file(temporary.path())?;
    temporary
        .write_all(&bytes)
        .context("write local session state")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync local session state")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace local session state {}", path.display()))?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open local state directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync local state directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect local state directory {}", path.display()))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect local state file {}", path.display()))
}

#[cfg(not(unix))]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SessionState {
        SessionState::new(
            "https://example.test".into(),
            "project-1".into(),
            "harness-a".into(),
            "credential-digest".into(),
            SessionAuth {
                id: "session-1".into(),
                proof: "secret".into(),
            },
            "workstation".into(),
            "test".into(),
            vec![],
        )
    }

    #[test]
    fn separate_harness_sessions_get_separate_files() {
        let first = path_for_with_home(Path::new("/tmp/config"), "origin", "project", "one");
        let second = path_for_with_home(Path::new("/tmp/config"), "origin", "project", "two");
        assert_ne!(first, second);
    }

    fn path_for_with_home(home: &Path, origin: &str, project: &str, session: &str) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(origin.as_bytes());
        digest.update([0]);
        digest.update(project.as_bytes());
        digest.update([0]);
        digest.update(session.as_bytes());
        home.join("sessions")
            .join(format!("{}.json", hex::encode(digest.finalize())))
    }

    #[test]
    fn same_pending_mutation_reuses_key_and_different_one_is_blocked() {
        let mut session_state = state();
        let pending = PendingMutation {
            key: "original-key".into(),
            method: HttpMethod::Post,
            path: "/api/v1/projects/p/claims".into(),
            body: serde_json::json!({"mode": "work"}),
            include_session_id: true,
        };
        session_state.set_pending(pending.clone()).unwrap();
        session_state.set_pending(pending.clone()).unwrap();
        assert_eq!(session_state.pending.unwrap().key, "original-key");

        let mut session_state = state();
        session_state.set_pending(pending).unwrap();
        let other = PendingMutation {
            key: "different-key".into(),
            method: HttpMethod::Post,
            path: "/api/v1/projects".into(),
            body: serde_json::json!({}),
            include_session_id: true,
        };
        assert!(session_state.set_pending(other).is_err());
    }

    #[test]
    fn pending_request_survives_save_and_reload_with_the_same_key_and_body() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions/state.json");
        let mut session_state = state();
        session_state.pending = Some(PendingMutation {
            key: "do-not-replace".into(),
            method: HttpMethod::Post,
            path: "/api/v1/projects/p/claims".into(),
            body: serde_json::json!({"policy_revision": 1}),
            include_session_id: true,
        });
        save(&path, &session_state).unwrap();
        let restored = load(&path).unwrap().unwrap();
        let pending = restored.pending.unwrap();
        assert_eq!(pending.key, "do-not-replace");
        assert_eq!(pending.body, serde_json::json!({"policy_revision": 1}));
    }

    #[test]
    fn same_session_lock_rejects_a_concurrent_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions/state.json");
        let _first = lock(&path).unwrap();
        assert!(lock(&path).is_err());
    }
}
