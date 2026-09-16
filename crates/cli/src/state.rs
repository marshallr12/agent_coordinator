use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
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
    pub subagent: Option<coordinator_core::SubagentInput>,
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
            subagent: None,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateAccessStage {
    StateDirectoryCreate,
    StateDirectoryProtection,
    LockFileOpen,
    LockFileProtection,
    LockAcquisition,
}

impl StateAccessStage {
    pub fn code(self) -> &'static str {
        match self {
            Self::StateDirectoryCreate => "state_directory_create",
            Self::StateDirectoryProtection => "state_directory_protection",
            Self::LockFileOpen => "lock_file_open",
            Self::LockFileProtection => "lock_file_protection",
            Self::LockAcquisition => "lock_acquisition",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::StateDirectoryCreate => "create session state directory",
            Self::StateDirectoryProtection => "protect session state directory",
            Self::LockFileOpen => "open or create session lock file",
            Self::LockFileProtection => "protect session lock file",
            Self::LockAcquisition => "acquire exclusive session lock",
        }
    }
}

#[derive(Debug)]
pub struct StateAccessError {
    stage: StateAccessStage,
    path: PathBuf,
    source: io::Error,
}

impl StateAccessError {
    fn new(stage: StateAccessStage, path: &Path, source: io::Error) -> Self {
        Self {
            stage,
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn stage(&self) -> StateAccessStage {
        self.stage
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn io_kind(&self) -> io::ErrorKind {
        self.source.kind()
    }

    pub fn raw_os_error(&self) -> Option<i32> {
        self.source.raw_os_error()
    }
}

impl fmt::Display for StateAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {} (I/O kind {:?}",
            self.stage.description(),
            self.path.display(),
            self.source,
            self.source.kind()
        )?;
        if let Some(code) = self.source.raw_os_error() {
            write!(formatter, ", OS code {code}")?;
        }
        write!(formatter, ")")
    }
}

impl StdError for StateAccessError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

pub fn lock(state_path: &Path) -> std::result::Result<SessionLock, StateAccessError> {
    let file = open_lock_file(state_path)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        StateAccessError::new(
            StateAccessStage::LockAcquisition,
            &state_path.with_extension("lock"),
            error,
        )
    })?;
    Ok(SessionLock { _file: file })
}

fn open_lock_file(state_path: &Path) -> std::result::Result<File, StateAccessError> {
    let parent = state_path.parent().ok_or_else(|| {
        StateAccessError::new(
            StateAccessStage::StateDirectoryCreate,
            state_path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "session state path has no parent",
            ),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        StateAccessError::new(StateAccessStage::StateDirectoryCreate, parent, error)
    })?;
    protect_directory(parent).map_err(|error| {
        StateAccessError::new(StateAccessStage::StateDirectoryProtection, parent, error)
    })?;
    let lock_path = state_path.with_extension("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            StateAccessError::new(StateAccessStage::LockFileOpen, &lock_path, error)
        })?;
    protect_file(&lock_path).map_err(|error| {
        StateAccessError::new(StateAccessStage::LockFileProtection, &lock_path, error)
    })?;
    Ok(file)
}

pub fn diagnose(state_path: &Path) -> Value {
    let directory = state_path.parent().unwrap_or(state_path);
    let lock_path = state_path.with_extension("lock");
    let mut checks = Vec::new();
    let mut healthy = true;

    match fs::create_dir_all(directory).map_err(|error| {
        StateAccessError::new(StateAccessStage::StateDirectoryCreate, directory, error)
    }) {
        Ok(()) => match protect_directory(directory).map_err(|error| {
            StateAccessError::new(StateAccessStage::StateDirectoryProtection, directory, error)
        }) {
            Ok(()) => checks.push(json_check("state_directory", "ok", directory, None)),
            Err(error) => {
                healthy = false;
                checks.push(json_check(
                    "state_directory",
                    "failed",
                    directory,
                    Some(&error),
                ));
            }
        },
        Err(error) => {
            healthy = false;
            checks.push(json_check(
                "state_directory",
                "failed",
                directory,
                Some(&error),
            ));
        }
    }

    match load(state_path) {
        Ok(Some(_)) => checks.push(json_check("session_state", "readable", state_path, None)),
        Ok(None) => checks.push(json_check("session_state", "missing", state_path, None)),
        Err(error) => {
            healthy = false;
            checks.push(serde_json::json!({
                "name": "session_state",
                "status": "failed",
                "path": state_path,
                "message": format!("{error:#}")
            }));
        }
    }

    match open_lock_file(state_path) {
        Ok(file) => {
            checks.push(json_check("lock_file", "open", &lock_path, None));
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => checks.push(json_check("exclusive_lock", "acquired", &lock_path, None)),
                Err(source) => {
                    healthy = false;
                    let error = StateAccessError::new(
                        StateAccessStage::LockAcquisition,
                        &lock_path,
                        source,
                    );
                    checks.push(json_check(
                        "exclusive_lock",
                        "busy_or_unavailable",
                        &lock_path,
                        Some(&error),
                    ));
                }
            }
        }
        Err(error) => {
            healthy = false;
            checks.push(json_check("lock_file", "failed", &lock_path, Some(&error)));
            checks.push(json_check(
                "exclusive_lock",
                "not_attempted",
                &lock_path,
                None,
            ));
        }
    }

    serde_json::json!({
        "healthy": healthy,
        "state_directory": directory,
        "state_path": state_path,
        "lock_path": lock_path,
        "checks": checks,
        "note": "A lock filename may remain after a process exits; only failure to acquire the exclusive lock demonstrates current contention."
    })
}

fn json_check(name: &str, status: &str, path: &Path, error: Option<&StateAccessError>) -> Value {
    match error {
        Some(error) => serde_json::json!({
            "name": name,
            "status": status,
            "path": path,
            "operation": error.stage().code(),
            "io_error_kind": format!("{:?}", error.io_kind()),
            "os_error_code": error.raw_os_error(),
            "message": error.to_string()
        }),
        None => serde_json::json!({"name": name, "status": status, "path": path}),
    }
}

fn method_name(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Patch => "PATCH",
    }
}

pub fn state_directory(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        validate_explicit_state_directory(path)?;
        return Ok(path.to_path_buf());
    }
    Ok(coordinator_home()?.join("sessions"))
}

pub fn path_for(
    explicit_state_dir: Option<&Path>,
    origin: &str,
    project_id: &str,
    local_session: &str,
) -> Result<PathBuf> {
    path_for_directory(
        &state_directory(explicit_state_dir)?,
        origin,
        project_id,
        local_session,
    )
}

pub fn path_for_directory(
    directory: &Path,
    origin: &str,
    project_id: &str,
    local_session: &str,
) -> Result<PathBuf> {
    validate_explicit_state_directory(directory)?;
    let mut digest = Sha256::new();
    digest.update(origin.as_bytes());
    digest.update([0]);
    digest.update(project_id.as_bytes());
    digest.update([0]);
    digest.update(local_session.as_bytes());
    let name = hex::encode(digest.finalize());
    Ok(directory.join(format!("{name}.json")))
}

fn validate_explicit_state_directory(path: &Path) -> Result<()> {
    use std::path::Component;

    if !path.is_absolute() {
        bail!("session state directory must be an absolute path");
    }
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "session state directory must be a dedicated absolute directory, not a filesystem root or a path containing . or .."
        );
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect session state directory {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "session state directory {} must be a real directory, not a file or link",
            path.display()
        );
    }
    for entry in fs::read_dir(path)
        .with_context(|| format!("inspect session state directory {}", path.display()))?
    {
        let entry =
            entry.with_context(|| format!("inspect session state directory {}", path.display()))?;
        let entry_path = entry.path();
        let file_type = entry.file_type().with_context(|| {
            format!(
                "inspect entry {} in the session state directory",
                entry_path.display()
            )
        })?;
        let extension = entry_path.extension().and_then(|value| value.to_str());
        if !file_type.is_file() || !matches!(extension, Some("json" | "lock")) {
            bail!(
                "session state directory {} is not dedicated: unexpected entry {}",
                path.display(),
                entry_path.display()
            );
        }
    }
    Ok(())
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
fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn protect_directory(path: &Path) -> io::Result<()> {
    set_private_windows_acl(path, true)
}

#[cfg(unix)]
fn protect_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn protect_file(path: &Path) -> io::Result<()> {
    set_private_windows_acl(path, false)
}

#[cfg(windows)]
fn set_private_windows_acl(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    // Owner Rights means the actual owner of this newly created/current-user
    // state directory, without embedding a localized account name. SYSTEM is
    // retained for machine recovery. The protected DACL prevents broad inherited
    // entries from exposing session proofs or pending mutation journals.
    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let descriptor_text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }

    let result = (|| {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = null_mut();
        let obtained = unsafe {
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
        };
        if obtained == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other(
                "private Windows security descriptor omitted its DACL",
            ));
        }
        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let status = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
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

    #[test]
    fn explicit_state_directory_is_used_without_a_sessions_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let path = path_for(
            Some(directory.path()),
            "https://example.test",
            "project",
            "runner",
        )
        .unwrap();
        assert_eq!(path.parent(), Some(directory.path()));
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("json")
        );
    }

    #[test]
    fn explicit_state_directory_must_be_dedicated() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("unrelated.txt"),
            b"not coordinator state",
        )
        .unwrap();
        assert!(state_directory(Some(directory.path())).is_err());
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
        let error = lock(&path).err().unwrap();
        assert_eq!(error.stage(), StateAccessStage::LockAcquisition);
        assert!(!format!("{error}").contains("secret"));
        #[cfg(windows)]
        assert!(error.raw_os_error().is_some());
    }

    #[test]
    fn lock_file_open_failure_is_distinct_from_contention() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("sessions/state.json");
        fs::create_dir_all(state_path.with_extension("lock")).unwrap();
        let error = lock(&state_path).err().unwrap();
        assert_eq!(error.stage(), StateAccessStage::LockFileOpen);
        assert_eq!(error.stage().code(), "lock_file_open");
        assert_ne!(error.stage(), StateAccessStage::LockFileProtection);
        assert_ne!(error.stage(), StateAccessStage::LockAcquisition);
        assert_ne!(format!("{:?}", error.io_kind()), "");
    }

    #[test]
    fn diagnostics_distinguish_lock_file_access_from_exclusive_contention() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runner/state.json");
        let _first = lock(&path).unwrap();
        let report = diagnose(&path);
        assert_eq!(report["healthy"], false);
        let checks = report["checks"].as_array().unwrap();
        assert!(
            checks
                .iter()
                .any(|check| { check["name"] == "lock_file" && check["status"] == "open" })
        );
        assert!(checks.iter().any(|check| {
            check["name"] == "exclusive_lock"
                && check["status"] == "busy_or_unavailable"
                && check["operation"] == "lock_acquisition"
        }));
        let rendered = report.to_string();
        assert!(!rendered.contains("credential-digest"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("lock filename may remain"));
    }

    #[cfg(windows)]
    #[test]
    fn isolated_windows_runner_directory_preserves_state_and_journal() {
        let directory = tempfile::tempdir().unwrap();
        let runner = directory.path().join("sandboxed-runner");
        let path = path_for(Some(&runner), "https://example.test", "project", "runner").unwrap();
        let _guard = lock(&path).unwrap();
        let mut saved = state();
        saved.pending = Some(PendingMutation {
            key: "same-key".into(),
            method: HttpMethod::Post,
            path: "/api/v1/mutation".into(),
            body: serde_json::json!({"same": "body"}),
            include_session_id: true,
        });
        save(&path, &saved).unwrap();
        let restored = load(&path).unwrap().unwrap();
        assert_eq!(restored.pending.unwrap().key, "same-key");
    }
}
