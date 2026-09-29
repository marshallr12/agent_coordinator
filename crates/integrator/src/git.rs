//! The integrator's own Git plumbing around `coordinator_local::git_workflow`:
//! one bare mirror per repository (hooks disabled), a linked worktree per
//! result, fetches, arbitrary-ref observation and the create-only push of R
//! to its result branch (and of a revert candidate to its candidate ref).
//! Computing R and the lease-guarded publish stay in `git_workflow`; the
//! mechanical revert is computed in `revert.rs`. Remotes are passed as URLs;
//! credentials come from the askpass helper, so no diagnostics here can carry
//! a token.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Runs `git -C dir <args>` with hooks disabled and no prompts.
pub(crate) fn run<I, S>(dir: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_env(dir, args, &[])
}

/// [`run`] with extra environment variables (a pinned commit identity).
pub(crate) fn run_env<I, S>(dir: &Path, args: I, env: &[(&str, &str)]) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .context("spawn git")
}

/// Runs git and returns trimmed stdout; failure carries git's last stderr line.
pub(crate) fn text<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    text_env(dir, args, &[])
}

/// [`text`] with extra environment variables (a pinned commit identity).
pub(crate) fn text_env<I, S>(dir: &Path, args: I, env: &[(&str, &str)]) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_env(dir, args, env)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git failed: {}", stderr.lines().last().unwrap_or("").trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Refuses remote arguments Git would parse as options.
fn remote_url(url: &str) -> Result<&str> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        "invalid remote URL"
    );
    Ok(url)
}

/// The mirror directory for `url` under `state_dir/mirrors`.
pub fn mirror_dir(state_dir: &Path, url: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(url.as_bytes()));
    state_dir
        .join("mirrors")
        .join(format!("{}.git", &digest[..16]))
}

/// Creates the bare mirror of `url` if it does not exist yet.
pub fn ensure_mirror(mirror: &Path, url: &str) -> Result<()> {
    let url = remote_url(url)?;
    if mirror.join("HEAD").exists() {
        return Ok(());
    }
    let parent = mirror.parent().context("mirror has no parent")?;
    std::fs::create_dir_all(parent)?;
    let target = mirror.as_os_str();
    text(
        parent,
        [
            OsStr::new("clone"),
            OsStr::new("--bare"),
            OsStr::new("--no-tags"),
            OsStr::new(url),
            target,
        ],
    )?;
    Ok(())
}

/// Fetches exact refspecs from the mirror's origin.
pub fn fetch(mirror: &Path, refspecs: &[String]) -> Result<()> {
    let mut args = vec!["fetch", "--no-tags", "--quiet", "origin"];
    args.extend(refspecs.iter().map(String::as_str));
    text(mirror, args).map(drop)
}

/// The remote's current value of `refname`, or `None` when it is absent.
pub fn ls_remote(mirror: &Path, url: &str, refname: &str) -> Result<Option<String>> {
    let url = remote_url(url)?;
    let output = run(mirror, ["ls-remote", "--exit-code", "--refs", url, refname])?;
    match output.status.code() {
        Some(2) => return Ok(None),
        Some(0) => {}
        _ => bail!("ls-remote of {refname} failed"),
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let exact = stdout.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == refname).then(|| sha.to_owned())
    });
    Ok(exact)
}

/// True when `ancestor` is reachable from `descendant` (both local).
pub fn is_ancestor(mirror: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    exit_flag(
        mirror,
        ["merge-base", "--is-ancestor", ancestor, descendant],
    )
}

/// True when `x` and `c` merge without conflicts (`merge-tree` exit 0).
pub fn merges_cleanly(mirror: &Path, x: &str, c: &str) -> Result<bool> {
    exit_flag(mirror, ["merge-tree", "--write-tree", x, c])
}

/// Maps a 0/1 exit status to true/false; anything else is an error.
fn exit_flag<const N: usize>(dir: &Path, args: [&str; N]) -> Result<bool> {
    match run(dir, args)?.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git {} failed", args[0]),
    }
}

/// Commits R lands on top of X: `X..C`, oldest first.
pub fn landing_range(mirror: &Path, x: &str, c: &str) -> Result<Vec<String>> {
    let out = text(mirror, ["rev-list", "--reverse", &format!("{x}..{c}")])?;
    Ok(out.lines().map(str::to_owned).collect())
}

/// One commit's identity, subject and trailer lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
    /// The commit's trailers, one `Key: value` per entry, continuation
    /// lines unfolded.
    pub trailers: Vec<String>,
}

/// The commits in `from..to`, in topological order (every commit before its
/// ancestors), with their subjects and trailers.
pub fn commits_between(mirror: &Path, from: &str, to: &str) -> Result<Vec<CommitInfo>> {
    let format = "--format=%H%x1f%s%x1f%(trailers:only,unfold)%x1e";
    let out = text(
        mirror,
        ["log", "--topo-order", format, &format!("{from}..{to}")],
    )?;
    Ok(out.split('\x1e').filter_map(parse_commit).collect())
}

/// Parses one `commits_between` record; blank separators yield `None`.
fn parse_commit(record: &str) -> Option<CommitInfo> {
    let mut fields = record.trim_start_matches('\n').splitn(3, '\x1f');
    let sha = fields.next().filter(|sha| !sha.is_empty())?.to_owned();
    let subject = fields.next()?.to_owned();
    let trailers = fields.next().unwrap_or_default().lines();
    let trailers = trailers.filter(|line| !line.trim().is_empty());
    Some(CommitInfo {
        sha,
        subject,
        trailers: trailers.map(str::to_owned).collect(),
    })
}

/// The blob id of `path` in `rev`, or `None` when the file is absent.
pub fn blob_at(mirror: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    let out = run(
        mirror,
        ["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")],
    )?;
    let blob = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((out.status.success() && !blob.is_empty()).then_some(blob))
}

/// The contents of `path` in `rev`, or `None` when the file is absent.
pub fn file_at(mirror: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    if blob_at(mirror, rev, path)?.is_none() {
        return Ok(None);
    }
    text(mirror, ["show", &format!("{rev}:{path}")]).map(Some)
}

/// Runs git and splits its stdout on NUL, so paths arrive raw rather than
/// C-quoted; failure carries git's last stderr line.
pub(crate) fn nul_list<const N: usize>(dir: &Path, args: [&str; N]) -> Result<Vec<String>> {
    nul_list_env(dir, args, &[])
}

/// [`nul_list`] with extra environment variables.
pub(crate) fn nul_list_env<const N: usize>(
    dir: &Path,
    args: [&str; N],
    env: &[(&str, &str)],
) -> Result<Vec<String>> {
    let output = run_env(dir, args, env)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git failed: {}", stderr.lines().last().unwrap_or("").trim());
    }
    let entries = output.stdout.split(|byte| *byte == 0);
    let paths = entries.filter(|entry| !entry.is_empty());
    Ok(paths
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Every path whose entry differs between `from` and `to` (added, modified,
/// deleted, type or mode changed; a rename is its old and new path).
pub fn changed_paths(mirror: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    nul_list(
        mirror,
        ["diff", "-z", "--no-renames", "--name-only", from, to, "--"],
    )
}

/// Git's mode for a symbolic link tree entry.
pub const SYMLINK_MODE: &str = "120000";

/// Every (mode, path) tree entry in `rev` at or under `path` (the whole
/// tree when empty), recursing into subtrees.
pub fn tree_entries(mirror: &Path, rev: &str, path: &str) -> Result<Vec<(String, String)>> {
    let path = if path.is_empty() { "." } else { path };
    let raw = nul_list(
        mirror,
        ["ls-tree", "-r", "-z", "--full-tree", rev, "--", path],
    )?;
    let parse = |entry: &String| {
        let (meta, name) = entry.split_once('\t')?;
        Some((meta.split(' ').next()?.to_owned(), name.to_owned()))
    };
    Ok(raw.iter().filter_map(parse).collect())
}

/// Creates (or reuses) a linked worktree on `branch`, started at `at`.
pub fn ensure_worktree(mirror: &Path, dir: &Path, branch: &str, at: &str) -> Result<()> {
    if dir.join(".git").exists() {
        return Ok(());
    }
    remove_worktree(mirror, dir, branch)?;
    let path = dir.to_str().context("worktree path is not UTF-8")?;
    text(
        mirror,
        ["worktree", "add", "--quiet", "-B", branch, path, at],
    )
    .map(drop)
}

/// Removes a result worktree and its branch; missing ones are fine.
pub fn remove_worktree(mirror: &Path, dir: &Path, branch: &str) -> Result<()> {
    if dir.exists() {
        let path = dir.to_str().context("worktree path is not UTF-8")?;
        if text(mirror, ["worktree", "remove", "--force", path]).is_err() {
            std::fs::remove_dir_all(dir).context("remove a partial worktree")?;
        }
    }
    let _ = run(mirror, ["worktree", "prune"]);
    let _ = run(mirror, ["branch", "-D", branch]);
    Ok(())
}

/// Pushes `sha` to a new remote ref (a result branch or a revert candidate
/// ref) with a create-only lease and confirms it by observation, never by the
/// push's exit status. An existing ref already at `sha` is success; one at
/// another commit is an error.
pub fn push_create_only(mirror: &Path, url: &str, sha: &str, refname: &str) -> Result<()> {
    let url = remote_url(url)?;
    if ls_remote(mirror, url, refname)?.as_deref() == Some(sha) {
        return Ok(());
    }
    let lease = format!("--force-with-lease={refname}:");
    let _ = run(
        mirror,
        ["push", "--quiet", &lease, url, &format!("{sha}:{refname}")],
    )?;
    let observed = ls_remote(mirror, url, refname)?;
    ensure!(
        observed.as_deref() == Some(sha),
        "remote ref {refname} is not at {sha}"
    );
    Ok(())
}

#[cfg(test)]
pub mod testing {
    //! A local bare remote with one commit on `main`, for crate tests.
    use super::*;

    pub struct Remote {
        pub _dir: tempfile::TempDir,
        pub url: String,
        pub source: PathBuf,
    }

    /// Runs git in `dir` with a fixed identity, panicking on failure.
    pub fn git(dir: &Path, args: &[&str]) -> String {
        let mut all = vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ];
        all.extend(args);
        text(dir, all).unwrap()
    }

    /// Creates `remote.git` and a `source` clone whose `main` has one commit.
    pub fn remote() -> Remote {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("remote.git");
        git(
            dir.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );
        let source = dir.path().join("source");
        git(
            dir.path(),
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        git(&source, &["checkout", "--quiet", "-b", "main"]);
        commit(&source, "base.txt", "base\n");
        git(&source, &["push", "--quiet", "origin", "main"]);
        let url = bare.to_str().unwrap().to_owned();
        Remote {
            _dir: dir,
            url,
            source,
        }
    }

    /// Writes one file, commits it and returns the new HEAD.
    pub fn commit(repo: &Path, file: &str, content: &str) -> String {
        commit_message(repo, file, content, file)
    }

    /// Writes one file, commits it with `message` and returns the new HEAD.
    pub fn commit_message(repo: &Path, file: &str, content: &str, message: &str) -> String {
        if let Some(parent) = Path::new(file).parent() {
            std::fs::create_dir_all(repo.join(parent)).unwrap();
        }
        std::fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", file]);
        git(repo, &["commit", "--quiet", "-m", message]);
        git(repo, &["rev-parse", "HEAD"])
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn mirror_fetch_observe_and_create_only_push() {
        let remote = remote();
        let state = tempfile::tempdir().unwrap();
        let mirror = mirror_dir(state.path(), &remote.url);
        ensure_mirror(&mirror, &remote.url).unwrap();
        let main = git(&remote.source, &["rev-parse", "HEAD"]);
        assert_eq!(
            ls_remote(&mirror, &remote.url, "refs/heads/main").unwrap(),
            Some(main.clone())
        );
        assert_eq!(
            ls_remote(&mirror, &remote.url, "refs/heads/nope").unwrap(),
            None
        );
        let next = commit(&remote.source, "b.txt", "b\n");
        git(
            &remote.source,
            &["push", "--quiet", "origin", "HEAD:refs/heads/topic"],
        );
        fetch(&mirror, &["+refs/heads/topic:refs/heads/topic".into()]).unwrap();
        assert!(is_ancestor(&mirror, &main, &next).unwrap());
        push_create_only(&mirror, &remote.url, &next, "refs/heads/ac/results/1").unwrap();
        push_create_only(&mirror, &remote.url, &next, "refs/heads/ac/results/1").unwrap();
        assert!(push_create_only(&mirror, &remote.url, &main, "refs/heads/ac/results/1").is_err());
    }

    #[test]
    fn option_like_urls_and_partial_worktrees_are_handled() {
        let remote = remote();
        assert!(ls_remote(&remote.source, "--upload-pack=x", "refs/heads/main").is_err());
        let dir = remote._dir.path().join("wt");
        std::fs::create_dir_all(dir.join("junk")).unwrap();
        let head = git(&remote.source, &["rev-parse", "HEAD"]);
        ensure_worktree(&remote.source, &dir, "integration/x", &head).unwrap();
        assert!(dir.join(".git").exists());
    }

    #[test]
    fn conflicts_blobs_and_landing_range() {
        let remote = remote();
        let base = git(&remote.source, &["rev-parse", "HEAD"]);
        let left = commit(&remote.source, "base.txt", "left\n");
        git(
            &remote.source,
            &["checkout", "--quiet", "-b", "other", &base],
        );
        let right = commit(&remote.source, "base.txt", "right\n");
        assert!(!merges_cleanly(&remote.source, &left, &right).unwrap());
        assert_eq!(
            landing_range(&remote.source, &left, &right).unwrap(),
            std::slice::from_ref(&right)
        );
        assert!(
            blob_at(&remote.source, &right, "base.txt")
                .unwrap()
                .is_some()
        );
        assert_eq!(file_at(&remote.source, &right, "missing").unwrap(), None);
        let changed = changed_paths(&remote.source, &base, &right).unwrap();
        assert_eq!(changed, ["base.txt"]);
        let tree = tree_entries(&remote.source, &right, "").unwrap();
        assert_eq!(tree, [("100644".to_owned(), "base.txt".to_owned())]);
        assert!(
            tree_entries(&remote.source, &right, "docs")
                .unwrap()
                .is_empty()
        );
        let entries = tree_entries(&remote.source, &right, "base.txt").unwrap();
        assert_eq!(entries, [("100644".to_owned(), "base.txt".to_owned())]);
    }
}
