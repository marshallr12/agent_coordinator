//! Git queries and the one Git write (`commit`) that `adopt` performs.
//!
//! Every call names the repository root explicitly, so nothing depends on the
//! process's current directory, and every call captures Git's output so the
//! `--json` event stream on stdout stays clean.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

/// Runs `git -C root args...` and returns its captured output.
fn run(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.first().copied().unwrap_or_default()))
}

/// Runs a Git query and returns trimmed stdout, or `None` when Git reports failure.
fn query(root: &Path, args: &[&str]) -> Option<String> {
    let output = run(root, args).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Returns the work tree root containing `path`; bare repositories have none.
pub fn toplevel(path: &Path) -> Result<PathBuf> {
    if query(path, &["rev-parse", "--is-bare-repository"]).as_deref() == Some("true") {
        bail!(
            "{} is a bare repository; adopt needs a work tree",
            path.display()
        );
    }
    let Some(root) = query(path, &["rev-parse", "--show-toplevel"]) else {
        bail!("{} is not inside a Git work tree", path.display());
    };
    Ok(PathBuf::from(root))
}

/// Says whether `root` is a linked worktree (its Git directory is not the common one).
pub fn is_linked_worktree(root: &Path) -> bool {
    let absolute = |flag: &str| query(root, &["rev-parse", "--path-format=absolute", flag]);
    match (absolute("--git-dir"), absolute("--git-common-dir")) {
        (Some(own), Some(common)) => own != common,
        _ => false,
    }
}

/// Names the merge or rebase that is in progress, if any; adopt refuses to run during one.
pub fn operation_in_progress(root: &Path) -> Option<&'static str> {
    let exists = |name: &str| {
        query(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )
        .is_some_and(|path| Path::new(&path).exists())
    };
    if exists("MERGE_HEAD") {
        Some("merge")
    } else if exists("rebase-merge") || exists("rebase-apply") {
        Some("rebase")
    } else {
        None
    }
}

/// Returns the URL of `origin`, or of the only remote when there is exactly one.
pub fn remote_url(root: &Path) -> Option<String> {
    if let Some(url) = query(root, &["remote", "get-url", "origin"]) {
        return Some(url);
    }
    let remotes = query(root, &["remote"])?;
    let mut names = remotes.lines().filter(|name| !name.is_empty());
    let (only, none) = (names.next()?, names.next());
    none.is_none()
        .then(|| query(root, &["remote", "get-url", only]))
        .flatten()
}

/// Returns the remote's default branch, else the current branch.
pub fn default_branch(root: &Path) -> Option<String> {
    let remote_head = query(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .and_then(|name| name.strip_prefix("origin/").map(str::to_owned));
    remote_head
        .or_else(|| query(root, &["branch", "--show-current"]))
        .filter(|name| !name.is_empty())
}

/// Lists which of `paths` have uncommitted changes (modified, staged or untracked).
pub fn dirty_paths(root: &Path, paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["status", "--porcelain", "--untracked-files=all", "--"];
    args.extend(paths.iter().map(String::as_str));
    let output = run(root, &args)?;
    if !output.status.success() {
        bail!("git status failed: {}", stderr_of(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(porcelain_path).collect())
}

/// Extracts the path from one `git status --porcelain` line.
fn porcelain_path(line: &str) -> Option<String> {
    let path = line.get(3..)?;
    let path = path.rsplit(" -> ").next().unwrap_or(path);
    Some(path.trim_matches('"').to_owned())
}

/// Lists files staged in the index that are not among `paths`.
///
/// Adopt skips its commit when this is non-empty, so it never sweeps up
/// someone else's staged work.
pub fn staged_outside(root: &Path, paths: &[String]) -> Result<Vec<String>> {
    let output = run(root, &["diff", "--cached", "--name-only"])?;
    if !output.status.success() {
        bail!("git diff --cached failed: {}", stderr_of(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|line| !line.is_empty() && !paths.iter().any(|path| path == line))
        .map(str::to_owned)
        .collect())
}

/// Commits exactly `paths` and returns the new commit ID.
///
/// Hooks and signing configuration are honoured (no `--no-verify`), and
/// nothing is pushed.
pub fn commit(root: &Path, paths: &[String], message: &str) -> Result<String> {
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    require(run(root, &add)?, "git add")?;
    let mut args = vec!["commit", "-m", message, "--"];
    args.extend(paths.iter().map(String::as_str));
    require(run(root, &args)?, "git commit")?;
    query(root, &["rev-parse", "HEAD"]).context("read the new commit ID")
}

/// Turns a failed Git command into an error carrying its stderr.
fn require(output: Output, what: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!("{what} failed: {}", stderr_of(&output))
}

/// Returns a command's stderr (or stdout when stderr is empty) as one trimmed string.
fn stderr_of(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::testing::TestRepo;

    #[test]
    fn toplevel_finds_the_root_from_a_nested_directory() {
        let repo = TestRepo::new();
        let nested = repo.root().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let found = std::fs::canonicalize(toplevel(&nested).unwrap()).unwrap();
        assert_eq!(found, std::fs::canonicalize(repo.root()).unwrap());
        let outside = tempfile::tempdir().unwrap();
        assert!(toplevel(outside.path()).is_err());
    }

    #[test]
    fn bare_repositories_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet", "--bare"])
            .arg(directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        let error = toplevel(directory.path()).unwrap_err().to_string();
        assert!(error.contains("bare"), "{error}");
    }

    #[test]
    fn remote_url_prefers_origin_then_the_sole_remote() {
        let repo = TestRepo::new();
        assert_eq!(remote_url(repo.root()), None);
        repo.git(&["remote", "add", "upstream", "https://example.com/a/one.git"]);
        assert_eq!(
            remote_url(repo.root()).as_deref(),
            Some("https://example.com/a/one.git")
        );
        repo.git(&["remote", "add", "other", "https://example.com/a/two.git"]);
        assert_eq!(remote_url(repo.root()), None);
        repo.git(&["remote", "add", "origin", "https://example.com/a/three.git"]);
        assert_eq!(
            remote_url(repo.root()).as_deref(),
            Some("https://example.com/a/three.git")
        );
    }

    #[test]
    fn dirty_and_staged_paths_are_reported_separately() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "changed\n");
        repo.write("other.txt", "staged\n");
        repo.git(&["add", "other.txt"]);
        let paths = vec!["AGENTS.md".to_owned(), "CLAUDE.md".to_owned()];
        assert_eq!(dirty_paths(repo.root(), &paths).unwrap(), ["AGENTS.md"]);
        assert_eq!(staged_outside(repo.root(), &paths).unwrap(), ["other.txt"]);
    }

    #[test]
    fn commit_is_limited_to_the_named_paths() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "block\n");
        repo.write("unrelated.txt", "left alone\n");
        let id = commit(repo.root(), &["AGENTS.md".to_owned()], "Connect").unwrap();
        assert_eq!(id.len(), 40);
        let files = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.trim(), "AGENTS.md");
        let status = repo.git(&["status", "--porcelain"]);
        assert_eq!(status.trim(), "?? unrelated.txt");
    }

    #[test]
    fn a_merge_in_progress_is_detected() {
        let repo = TestRepo::new();
        assert_eq!(operation_in_progress(repo.root()), None);
        std::fs::write(repo.root().join(".git/MERGE_HEAD"), "0\n").unwrap();
        assert_eq!(operation_in_progress(repo.root()), Some("merge"));
    }
}
