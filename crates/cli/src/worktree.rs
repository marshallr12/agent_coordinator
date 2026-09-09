use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::config::coordinator_home;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparationIntent {
    pub service_origin: String,
    pub project_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub repository_url: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub branch: String,
    pub base_selector: String,
    pub base_revision: String,
    #[serde(default)]
    pub prepared: bool,
    #[serde(default)]
    pub git_dir_identity: Option<PathBuf>,
}

pub struct PreparationLock {
    _file: File,
}

pub struct PrepareRequest<'a> {
    pub service_origin: &'a str,
    pub project_id: &'a str,
    pub attempt_id: &'a str,
    pub generation: u64,
    pub repository_url: &'a str,
    pub source: &'a Path,
    pub destination: &'a Path,
    pub branch: &'a str,
    pub base: &'a str,
}

pub fn intent_path(service_origin: &str, project_id: &str, attempt_id: &str) -> Result<PathBuf> {
    let mut digest = Sha256::new();
    digest.update(service_origin.as_bytes());
    digest.update([0]);
    digest.update(project_id.as_bytes());
    digest.update([0]);
    digest.update(attempt_id.as_bytes());
    Ok(coordinator_home()?
        .join("worktrees")
        .join(format!("{}.json", hex::encode(digest.finalize()))))
}

pub fn lock(path: &Path) -> Result<PreparationLock> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("worktree intent path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create worktree state directory {}", parent.display()))?;
    protect_directory(parent)?;
    let lock_path = path.with_extension("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open worktree lock {}", lock_path.display()))?;
    protect_file(&lock_path)?;
    file.try_lock().with_context(|| {
        format!(
            "worktree preparation is busy in another process (lock {})",
            lock_path.display()
        )
    })?;
    Ok(PreparationLock { _file: file })
}

pub fn prepare(request: PrepareRequest<'_>) -> Result<PreparationIntent> {
    let path = intent_path(
        request.service_origin,
        request.project_id,
        request.attempt_id,
    )?;
    prepare_at_path(request, &path)
}

fn prepare_at_path(request: PrepareRequest<'_>, path: &Path) -> Result<PreparationIntent> {
    if request.branch.trim().is_empty() || request.branch.starts_with('-') {
        bail!("--branch must be a non-empty Git branch name without a leading dash");
    }
    git_ok(
        request.source,
        ["check-ref-format", "--branch", request.branch],
    )?;

    let source = canonical_git_root(request.source)?;
    ensure_remote_matches(&source, request.repository_url)?;
    let destination = absolute_destination(request.destination)?;
    if destination == source {
        bail!("the worktree destination must differ from the source checkout");
    }

    let _lock = lock(path)?;
    let mut intent = match load(path)? {
        Some(existing) => {
            if !same_request(&existing, &request, &source, &destination) {
                bail!(
                    "this attempt already has a different saved worktree preparation intent at {}; reuse its original arguments",
                    path.display()
                );
            }
            existing
        }
        None => {
            ensure_clean(&source, "source checkout")?;
            if destination.exists() {
                bail!(
                    "worktree destination {} already exists and no matching saved preparation intent exists",
                    destination.display()
                );
            }
            if local_branch_exists(&source, request.branch)? {
                bail!(
                    "branch {} already exists; no Git state was changed",
                    request.branch
                );
            }
            let base_revision = git_text(
                &source,
                ["rev-parse", &format!("{}^{{commit}}", request.base)],
            )?;
            let desired = PreparationIntent {
                service_origin: request.service_origin.to_owned(),
                project_id: request.project_id.to_owned(),
                attempt_id: request.attempt_id.to_owned(),
                generation: request.generation,
                repository_url: request.repository_url.to_owned(),
                source: source.clone(),
                destination: destination.clone(),
                branch: request.branch.to_owned(),
                base_selector: request.base.to_owned(),
                base_revision,
                prepared: false,
                git_dir_identity: None,
            };
            save(path, &desired)?;
            desired
        }
    };

    if destination.exists() {
        reconcile(&mut intent)?;
    } else {
        if local_branch_exists(&source, &intent.branch)? {
            bail!(
                "branch {} already exists while the saved destination does not; inspect Git worktrees manually and do not delete or reset anything",
                intent.branch
            );
        }
        git_ok_os(
            &source,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("-b"),
                intent.branch.clone().into(),
                destination.as_os_str().to_owned(),
                intent.base_revision.clone().into(),
            ],
        )?;
        reconcile(&mut intent)?;
    }
    intent.prepared = true;
    save(path, &intent)?;
    Ok(intent)
}

pub fn load_for_attempt(
    service_origin: &str,
    project_id: &str,
    attempt_id: &str,
) -> Result<PreparationIntent> {
    let path = intent_path(service_origin, project_id, attempt_id)?;
    let intent = load(&path)?.ok_or_else(|| {
        anyhow!(
            "no prepared worktree is saved for attempt {attempt_id}; run `agent-coordinator worktree prepare` first"
        )
    })?;
    if !intent.prepared {
        bail!("worktree preparation for attempt {attempt_id} is incomplete; rerun prepare");
    }
    Ok(intent)
}

pub fn current_snapshot(intent: &PreparationIntent) -> Result<(String, String)> {
    reconcile_snapshot(intent)
}

fn same_request(
    existing: &PreparationIntent,
    request: &PrepareRequest<'_>,
    source: &Path,
    destination: &Path,
) -> bool {
    existing.service_origin == request.service_origin
        && existing.project_id == request.project_id
        && existing.attempt_id == request.attempt_id
        && existing.generation == request.generation
        && existing.repository_url == request.repository_url
        && existing.source == source
        && existing.destination == destination
        && existing.branch == request.branch
        && existing.base_selector == request.base
}

fn reconcile(intent: &mut PreparationIntent) -> Result<()> {
    let actual_root = canonical_git_root(&intent.destination).with_context(|| {
        format!(
            "the saved destination {} is not the expected Git worktree",
            intent.destination.display()
        )
    })?;
    if actual_root != intent.destination {
        bail!(
            "saved destination {} resolves to a different Git root {}",
            intent.destination.display(),
            actual_root.display()
        );
    }
    let actual_branch = git_text(&intent.destination, ["symbolic-ref", "--short", "HEAD"])?;
    if actual_branch != intent.branch {
        bail!(
            "saved destination is on branch {actual_branch}, expected {}; no Git state was changed",
            intent.branch
        );
    }
    ensure_remote_matches(&intent.destination, &intent.repository_url)?;
    ensure_clean(&intent.destination, "prepared worktree")?;
    let revision = git_text(&intent.destination, ["rev-parse", "HEAD^{commit}"])?;
    if revision != intent.base_revision {
        bail!(
            "saved destination is at revision {revision}, expected {}; no Git state was changed",
            intent.base_revision
        );
    }
    let source_common = canonical_common_git_dir(&intent.source)?;
    let destination_common = canonical_common_git_dir(&intent.destination)?;
    if source_common != destination_common {
        bail!("saved destination belongs to a different Git repository; no Git state was changed");
    }
    let identity = canonical_git_dir(&intent.destination)?;
    if intent
        .git_dir_identity
        .as_ref()
        .is_some_and(|saved| saved != &identity)
    {
        bail!("saved destination Git identity changed; refusing to replace it");
    }
    intent.git_dir_identity = Some(identity);
    Ok(())
}

fn reconcile_snapshot(intent: &PreparationIntent) -> Result<(String, String)> {
    let actual_root = canonical_git_root(&intent.destination)?;
    if actual_root != intent.destination {
        bail!("prepared checkout resolves to a different Git root");
    }
    let identity = canonical_git_dir(&intent.destination)?;
    if intent.git_dir_identity.as_ref() != Some(&identity) {
        bail!("prepared checkout Git identity no longer matches its saved identity");
    }
    if canonical_common_git_dir(&intent.destination)? != canonical_common_git_dir(&intent.source)? {
        bail!("prepared checkout belongs to a different Git repository");
    }
    let branch = git_text(&intent.destination, ["symbolic-ref", "--short", "HEAD"])?;
    if branch != intent.branch {
        bail!(
            "prepared checkout is on branch {branch}, expected {}",
            intent.branch
        );
    }
    ensure_remote_matches(&intent.destination, &intent.repository_url)?;
    ensure_clean(&intent.destination, "prepared worktree")?;
    let revision = git_text(&intent.destination, ["rev-parse", "HEAD^{commit}"])?;
    let tree = git_text(&intent.destination, ["rev-parse", "HEAD^{tree}"])?;
    Ok((revision, tree))
}

fn canonical_git_root(path: &Path) -> Result<PathBuf> {
    let root = git_text(path, ["rev-parse", "--show-toplevel"])?;
    fs::canonicalize(&root).with_context(|| format!("resolve Git root {root}"))
}

fn canonical_git_dir(path: &Path) -> Result<PathBuf> {
    let git_dir = git_text(path, ["rev-parse", "--path-format=absolute", "--git-dir"])?;
    fs::canonicalize(&git_dir).with_context(|| format!("resolve Git directory {git_dir}"))
}

fn canonical_common_git_dir(path: &Path) -> Result<PathBuf> {
    let git_dir = git_text(
        path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    fs::canonicalize(&git_dir).with_context(|| format!("resolve common Git directory {git_dir}"))
}

fn absolute_destination(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("resolve worktree destination {}", path.display()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("read current directory")?
            .join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("worktree destination has no parent"))?;
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("resolve worktree destination parent {}", parent.display()))?;
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("worktree destination must name a directory"))?;
    Ok(parent.join(name))
}

fn ensure_clean(repository: &Path, label: &str) -> Result<()> {
    let output = git(
        repository,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !output.stdout.is_empty() {
        bail!("{label} is dirty; existing changes were preserved without reset or stash");
    }
    Ok(())
}

fn ensure_remote_matches(repository: &Path, expected: &str) -> Result<()> {
    let remote_names = git_text(repository, ["remote"])?;
    let mut urls = Vec::new();
    for name in remote_names.lines().filter(|value| !value.is_empty()) {
        let output = git(repository, ["remote", "get-url", "--all", name])?;
        if output.status.success() {
            let text = String::from_utf8(output.stdout).context("Git remote URL is not UTF-8")?;
            urls.extend(text.lines().map(str::to_owned));
        }
    }
    if urls
        .iter()
        .any(|actual| remote_urls_equal(actual, expected))
    {
        return Ok(());
    }
    bail!("the source repository has no remote matching the project's configured repository URL")
}

fn remote_urls_equal(actual: &str, expected: &str) -> bool {
    if normalized_remote(actual) == normalized_remote(expected) {
        return true;
    }
    let actual_path = Path::new(actual);
    let expected_path = Path::new(expected);
    if actual_path.exists() && expected_path.exists() {
        return fs::canonicalize(actual_path).ok() == fs::canonicalize(expected_path).ok();
    }
    false
}

fn normalized_remote(value: &str) -> &str {
    value.trim().trim_end_matches('/').trim_end_matches(".git")
}

fn local_branch_exists(repository: &Path, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let output = git(repository, ["show-ref", "--verify", "--quiet", &reference])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("Git could not inspect branch {branch}"),
    }
}

fn git_text<I, S>(repository: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(repository, args)?;
    if !output.status.success() {
        bail!("Git command failed: {}", safe_stderr(&output));
    }
    String::from_utf8(output.stdout)
        .context("Git output is not UTF-8")
        .map(|value| value.trim().to_owned())
}

fn git_ok<I, S>(repository: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(repository, args)?;
    if !output.status.success() {
        bail!("Git command failed: {}", safe_stderr(&output));
    }
    Ok(())
}

fn git_ok_os<I>(repository: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    git_ok(repository, args)
}

fn git<I, S>(repository: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .with_context(|| format!("run Git in {}", repository.display()))
}

fn safe_stderr(output: &Output) -> String {
    let value = String::from_utf8_lossy(&output.stderr);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        format!("exit status {}", output.status)
    } else {
        trimmed.chars().take(1000).collect()
    }
}

fn load(path: &Path) -> Result<Option<PreparationIntent>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse worktree intent {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read worktree intent {}", path.display()))
        }
    }
}

fn save(path: &Path, intent: &PreparationIntent) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("worktree intent path has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create worktree state directory {}", parent.display()))?;
    protect_directory(parent)?;
    let bytes = serde_json::to_vec_pretty(intent).context("serialize worktree intent")?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary worktree state in {}", parent.display()))?;
    protect_file(temporary.path())?;
    temporary
        .write_all(&bytes)
        .context("write worktree intent")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync worktree intent")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace worktree intent {}", path.display()))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open worktree state directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync worktree state directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect worktree state directory {}", path.display()))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect worktree state file {}", path.display()))
}

#[cfg(not(unix))]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn prepare_for_test(
        request: PrepareRequest<'_>,
        state_home: &Path,
    ) -> Result<PreparationIntent> {
        prepare_at_path(request, &state_home.join("worktree-intent.json"))
    }

    struct Repository {
        _directory: TempDir,
        remote: PathBuf,
        source: PathBuf,
        revision: String,
    }

    fn repository() -> Repository {
        let directory = tempfile::Builder::new()
            .prefix("coordinator git paths with spaces ")
            .tempdir()
            .unwrap();
        let remote = directory.path().join("remote repository.git");
        git_ok(
            directory.path(),
            ["init", "--bare", remote.to_str().unwrap()],
        )
        .unwrap();
        let source = directory.path().join("source checkout");
        git_ok(
            directory.path(),
            ["clone", remote.to_str().unwrap(), source.to_str().unwrap()],
        )
        .unwrap();
        git_ok(&source, ["config", "user.email", "test@example.invalid"]).unwrap();
        git_ok(&source, ["config", "user.name", "Coordinator Test"]).unwrap();
        fs::write(source.join("file.txt"), "one\n").unwrap();
        git_ok(&source, ["add", "file.txt"]).unwrap();
        git_ok(&source, ["commit", "-m", "initial"]).unwrap();
        git_ok(&source, ["push", "origin", "HEAD:main"]).unwrap();
        let revision = git_text(&source, ["rev-parse", "HEAD"]).unwrap();
        Repository {
            _directory: directory,
            remote,
            source,
            revision,
        }
    }

    fn request<'a>(repository: &'a Repository, destination: &'a Path) -> PrepareRequest<'a> {
        PrepareRequest {
            service_origin: "https://coordinator.example",
            project_id: "project-1",
            attempt_id: "attempt-1",
            generation: 1,
            repository_url: repository.remote.to_str().unwrap(),
            source: &repository.source,
            destination,
            branch: "agent/task-1",
            base: &repository.revision,
        }
    }

    #[test]
    fn prepares_and_reconciles_the_same_path_with_spaces() {
        let repository = repository();
        let state_home = tempfile::tempdir().unwrap();
        let destination = repository
            ._directory
            .path()
            .join("new worktree with spaces");
        let first =
            prepare_for_test(request(&repository, &destination), state_home.path()).unwrap();
        let second =
            prepare_for_test(request(&repository, &destination), state_home.path()).unwrap();
        assert_eq!(first.destination, second.destination);
        assert_eq!(first.git_dir_identity, second.git_dir_identity);
        assert_eq!(
            git_text(&destination, ["rev-parse", "HEAD"]).unwrap(),
            repository.revision
        );
    }

    #[test]
    fn refuses_wrong_remote_before_creating_a_worktree() {
        let repository = repository();
        let state_home = tempfile::tempdir().unwrap();
        let destination = repository._directory.path().join("never created");
        let mut request = request(&repository, &destination);
        request.repository_url = "https://wrong.example/repository.git";
        assert!(prepare_for_test(request, state_home.path()).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn refuses_dirty_source_and_preserves_its_changes() {
        let repository = repository();
        let state_home = tempfile::tempdir().unwrap();
        fs::write(repository.source.join("file.txt"), "changed\n").unwrap();
        let destination = repository._directory.path().join("never created");
        assert!(prepare_for_test(request(&repository, &destination), state_home.path()).is_err());
        assert_eq!(
            fs::read_to_string(repository.source.join("file.txt")).unwrap(),
            "changed\n"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn retry_uses_saved_commit_when_base_selector_moves() {
        let repository = repository();
        let state_home = tempfile::tempdir().unwrap();
        git_ok(
            &repository.source,
            ["update-ref", "refs/heads/moving-base", &repository.revision],
        )
        .unwrap();
        let destination = repository._directory.path().join("stable retry");
        let mut first_request = request(&repository, &destination);
        first_request.base = "moving-base";
        let first = prepare_for_test(first_request, state_home.path()).unwrap();

        fs::write(repository.source.join("second.txt"), "two\n").unwrap();
        git_ok(&repository.source, ["add", "second.txt"]).unwrap();
        git_ok(&repository.source, ["commit", "-m", "second"]).unwrap();
        let advanced = git_text(&repository.source, ["rev-parse", "HEAD"]).unwrap();
        git_ok(
            &repository.source,
            ["update-ref", "refs/heads/moving-base", &advanced],
        )
        .unwrap();

        let mut retry_request = request(&repository, &destination);
        retry_request.base = "moving-base";
        let retried = prepare_for_test(retry_request, state_home.path()).unwrap();
        assert_eq!(retried.base_revision, first.base_revision);
        assert_eq!(
            git_text(&destination, ["rev-parse", "HEAD"]).unwrap(),
            repository.revision
        );
    }

    #[test]
    fn current_snapshot_rejects_a_replaced_checkout() {
        let repository = repository();
        let state_home = tempfile::tempdir().unwrap();
        let destination = repository._directory.path().join("replace target");
        let prepared =
            prepare_for_test(request(&repository, &destination), state_home.path()).unwrap();
        let moved = repository._directory.path().join("original moved aside");
        fs::rename(&destination, moved).unwrap();
        git_ok(
            repository._directory.path(),
            [
                "clone",
                repository.remote.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        )
        .unwrap();
        assert!(current_snapshot(&prepared).is_err());
    }
}
