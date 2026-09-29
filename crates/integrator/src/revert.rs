//! The mechanical revert of a published result R on target tip X (planning
//! plan-final §2.4a "M6"), computed by Git rather than an LLM in a
//! disposable linked worktree of the mirror. When R is the integrator's merge
//! of the candidate into T0 (two or more parents, the first being T0) it is
//! `git revert -m 1 R`; otherwise R landed fast-forward and its landing range
//! `T0..R` is reverted as one step: `git revert` of a commit whose parent is
//! T0 and whose tree is R's, which undoes every commit of the range at once.
//! An R that is not in X's history, or whose revert would change nothing on
//! X, has no mechanical revert.
//!
//! The commit has parent X, a pinned identity and date, and a message built
//! only from the revert task, R, T0 and C's subject. Every Git command that
//! computes it runs without the host's global and system configuration (and
//! without hooks), so the same inputs give the same candidate commit id on
//! any host. A known limit: the target's own `.gitattributes` still apply,
//! including merge drivers such as the builtin `union`; they are reviewed
//! repository content, and Git 2.39 cannot ignore them for one command (it
//! has no `attr.tree`).
use crate::git;
use anyhow::{Context, Result, ensure};
use std::path::Path;

/// The environment of every Git command this module runs: the pinned
/// identity and date of `git_workflow`'s integration commits, and no global
/// or system configuration.
const ISOLATED: [(&str, &str); 8] = [
    ("GIT_AUTHOR_NAME", "Agent Coordinator"),
    ("GIT_AUTHOR_EMAIL", "agent-coordinator@example.invalid"),
    ("GIT_AUTHOR_DATE", "2000-01-01T00:00:00 +0000"),
    ("GIT_COMMITTER_NAME", "Agent Coordinator"),
    ("GIT_COMMITTER_EMAIL", "agent-coordinator@example.invalid"),
    ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00 +0000"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
];

/// What to revert, and where.
pub struct RevertSpec<'a> {
    /// The target tip X the revert is computed on.
    pub x: &'a str,
    /// The published result R being reverted.
    pub r: &'a str,
    /// The tip T0 that R is computed from.
    pub t0: &'a str,
    /// The candidate C that R integrated; its subject names the revert.
    pub c: &'a str,
    /// The revert task, named in the commit's trailer.
    pub task_id: &'a str,
}

/// The outcome of a mechanical revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mechanical {
    /// The revert applied cleanly: the candidate commit (parent X) and tree.
    Clean { commit: String, tree: String },
    /// The revert conflicts on X in these paths.
    Conflict(Vec<String>),
    /// R is not in X's history (or not in the mirror at all).
    NotInHistory,
    /// Reverting R changes nothing on X: X already undid it.
    AlreadyUndone,
}

/// One revert computation: the mirror, the worktree and the environment
/// every Git command gets (the caller's, then [`ISOLATED`], which wins).
struct Run<'a> {
    mirror: &'a Path,
    dir: &'a Path,
    env: Vec<(&'a str, &'a str)>,
}

/// Computes the revert of `spec` in a fresh worktree `dir` on `branch` (both
/// removed again afterwards, whatever the outcome).
pub fn compute(mirror: &Path, dir: &Path, branch: &str, spec: &RevertSpec) -> Result<Mechanical> {
    compute_with(mirror, (dir, branch), spec, &[])
}

/// [`compute`] with extra environment variables below [`ISOLATED`] (tests
/// use it to present a hostile host configuration).
fn compute_with(
    mirror: &Path,
    (dir, branch): (&Path, &str),
    spec: &RevertSpec,
    host: &[(&str, &str)],
) -> Result<Mechanical> {
    let env = host.iter().chain(&ISOLATED).copied().collect();
    let run = Run { mirror, dir, env };
    if !run.in_history(spec)? {
        return Ok(Mechanical::NotInHistory);
    }
    git::remove_worktree(mirror, dir, branch)?;
    run.add_worktree(branch, spec.x)?;
    let outcome = run.revert(spec);
    git::remove_worktree(mirror, dir, branch)?;
    outcome
}

impl Run<'_> {
    /// Runs git in `at` with this run's environment; trimmed stdout.
    fn git<const N: usize>(&self, at: &Path, args: [&str; N]) -> Result<String> {
        git::text_env(at, args, &self.env)
    }

    /// True when R is a commit of the mirror and an ancestor of X.
    fn in_history(&self, spec: &RevertSpec) -> Result<bool> {
        let object = format!("{}^{{commit}}", spec.r);
        let exists = git::run_env(self.mirror, ["cat-file", "-e", &object], &self.env)?;
        if !exists.status.success() {
            return Ok(false);
        }
        let args = ["merge-base", "--is-ancestor", spec.r, spec.x];
        match git::run_env(self.mirror, args, &self.env)?.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => anyhow::bail!("git merge-base failed"),
        }
    }

    /// Adds the worktree on `branch` at `at`.
    fn add_worktree(&self, branch: &str, at: &str) -> Result<()> {
        let path = self.dir.to_str().context("worktree path is not UTF-8")?;
        let args = ["worktree", "add", "--quiet", "-B", branch, path, at];
        self.git(self.mirror, args).map(drop)
    }

    /// Applies the revert to the worktree's index and commits it, or
    /// reports why there is no mechanical revert.
    fn revert(&self, spec: &RevertSpec) -> Result<Mechanical> {
        let mut args = vec!["revert".to_owned(), "--no-commit".to_owned()];
        args.extend(self.revert_args(spec)?);
        let output = git::run_env(self.dir, &args, &self.env)?;
        if !output.status.success() {
            return self.conflict(&output.stderr);
        }
        let index = self.git(self.dir, ["write-tree"])?;
        if index == self.git(self.mirror, ["rev-parse", &format!("{}^{{tree}}", spec.x)])? {
            return Ok(Mechanical::AlreadyUndone);
        }
        self.commit(&self.message(spec)?)
    }

    /// The conflicting paths of a failed revert; a failure without any is an
    /// error carrying Git's last stderr line.
    fn conflict(&self, stderr: &[u8]) -> Result<Mechanical> {
        let paths = git::nul_list_env(
            self.dir,
            ["diff", "-z", "--name-only", "--diff-filter=U"],
            &self.env,
        )?;
        let stderr = String::from_utf8_lossy(stderr);
        let last = stderr.lines().last().unwrap_or("").trim().to_owned();
        ensure!(!paths.is_empty(), "git revert failed: {last}");
        Ok(Mechanical::Conflict(paths))
    }

    /// The `git revert` arguments for R: `-m 1 R` for the integrator's merge
    /// into T0, otherwise the one-step stand-in for the range `T0..R`.
    fn revert_args(&self, spec: &RevertSpec) -> Result<Vec<String>> {
        let parents = self.git(self.mirror, ["rev-parse", &format!("{}^@", spec.r)])?;
        let parents: Vec<&str> = parents.lines().collect();
        if parents.len() > 1 && parents[0] == spec.t0 {
            return Ok(vec!["-m".into(), "1".into(), spec.r.into()]);
        }
        Ok(vec![self.range_commit(spec)?])
    }

    /// A commit with parent T0 and R's tree: reverting it undoes `T0..R`
    /// whole.
    fn range_commit(&self, spec: &RevertSpec) -> Result<String> {
        let tree = format!("{}^{{tree}}", spec.r);
        let message = format!("range {}..{}", spec.t0, spec.r);
        let args = ["commit-tree", &tree, "-p", spec.t0, "-m", &message];
        self.git(self.mirror, args)
    }

    /// `Revert "<subject of C>"`, a body naming R and T0, and trailers
    /// naming the revert task and R.
    fn message(&self, spec: &RevertSpec) -> Result<String> {
        let subject = self.git(self.mirror, ["log", "-1", "--format=%s", spec.c])?;
        Ok(format!(
            "Revert \"{subject}\"\n\nThis reverts integration result {r} (computed on {t0}).\n\n\
             Agent-Coordinator-Revert-Task: {task}\nAgent-Coordinator-Reverted-Result: {r}\n",
            r = spec.r,
            t0 = spec.t0,
            task = spec.task_id
        ))
    }

    /// Commits the index with the pinned identity and returns the commit
    /// and its tree.
    fn commit(&self, message: &str) -> Result<Mechanical> {
        let args = [
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "--cleanup=verbatim",
            "-m",
            message,
        ];
        self.git(self.dir, args).context("commit the revert")?;
        let commit = self.git(self.dir, ["rev-parse", "HEAD"])?;
        let tree = self.git(self.dir, ["rev-parse", "HEAD^{tree}"])?;
        Ok(Mechanical::Clean { commit, tree })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{Remote, commit as write, git, remote};

    /// A spec reverting `r` (computed on `t0`, integrating `c`) on `x`.
    fn spec<'a>(x: &'a str, r: &'a str, t0: &'a str, c: &'a str) -> RevertSpec<'a> {
        RevertSpec {
            x,
            r,
            t0,
            c,
            task_id: "rt1",
        }
    }

    /// Computes `spec` in a worktree named `name` of the remote's source.
    fn run(remote: &Remote, name: &str, spec: &RevertSpec) -> Mechanical {
        let dir = remote._dir.path().join(name);
        compute(&remote.source, &dir, &format!("revert/{name}"), spec).unwrap()
    }

    /// The candidate commit of a clean revert.
    fn clean(outcome: &Mechanical) -> &str {
        match outcome {
            Mechanical::Clean { commit, .. } => commit,
            other => panic!("{other:?}"),
        }
    }

    /// Lands `feature` as a `--no-ff` merge into `main` (first parent T0);
    /// returns (T0, C, R).
    fn merged_feature(remote: &Remote) -> (String, String, String) {
        let dir = &remote.source;
        let t0 = git(dir, &["rev-parse", "HEAD"]);
        git(dir, &["checkout", "--quiet", "-b", "feature"]);
        let c = write(dir, "feature.txt", "feature\n");
        git(dir, &["checkout", "--quiet", "main"]);
        git(
            dir,
            &["merge", "--quiet", "--no-ff", "-m", "merge", "feature"],
        );
        (t0, c, git(dir, &["rev-parse", "HEAD"]))
    }

    #[test]
    fn a_merge_result_reverts_with_mainline_one_reproducibly() {
        let remote = remote();
        let (t0, c, r) = merged_feature(&remote);
        let x = write(&remote.source, "later.txt", "later\n");
        let first = run(&remote, "one", &spec(&x, &r, &t0, &c));
        let again = run(&remote, "two", &spec(&x, &r, &t0, &c));
        assert_eq!(first, again, "same (X, R) gives the same commit");
        let commit = clean(&first);
        let dir = &remote.source;
        assert_eq!(git(dir, &["rev-parse", &format!("{commit}^")]), x);
        let files = git(dir, &["ls-tree", "--name-only", commit]);
        assert_eq!(files, "base.txt\nlater.txt");
        let message = git(dir, &["log", "-1", "--format=%B", commit]);
        assert!(message.starts_with("Revert \"feature.txt\""), "{message}");
        assert!(message.contains("Agent-Coordinator-Revert-Task: rt1"));
        assert!(message.contains(&format!("Agent-Coordinator-Reverted-Result: {r}")));
    }

    #[test]
    fn a_fast_forward_range_reverts_every_commit() {
        let remote = remote();
        let dir = &remote.source;
        let t0 = git(dir, &["rev-parse", "HEAD"]);
        write(dir, "one.txt", "one\n");
        let r = write(dir, "two.txt", "two\n");
        let x = write(dir, "later.txt", "later\n");
        let outcome = run(&remote, "ff", &spec(&x, &r, &t0, &r));
        let commit = clean(&outcome);
        let files = git(dir, &["ls-tree", "--name-only", commit]);
        assert_eq!(files, "base.txt\nlater.txt", "both range commits undone");
        assert_eq!(outcome, run(&remote, "ff2", &spec(&x, &r, &t0, &r)));
    }

    #[test]
    fn a_result_outside_the_target_history_has_no_revert() {
        let remote = remote();
        let dir = &remote.source;
        let t0 = git(dir, &["rev-parse", "HEAD"]);
        git(dir, &["checkout", "--quiet", "-b", "side"]);
        let r = write(dir, "side.txt", "side\n");
        git(dir, &["checkout", "--quiet", "main"]);
        let x = write(dir, "later.txt", "later\n");
        let outcome = run(&remote, "outside", &spec(&x, &r, &t0, &r));
        assert_eq!(outcome, Mechanical::NotInHistory);
        let missing = "ab".repeat(20);
        let outcome = run(&remote, "missing", &spec(&x, &missing, &t0, &r));
        assert_eq!(outcome, Mechanical::NotInHistory, "R not in the mirror");
    }

    #[test]
    fn a_result_already_undone_on_the_target_has_no_revert() {
        let remote = remote();
        let dir = &remote.source;
        let t0 = git(dir, &["rev-parse", "HEAD"]);
        let r = write(dir, "one.txt", "one\n");
        git(dir, &["rm", "--quiet", "one.txt"]);
        git(dir, &["commit", "--quiet", "-m", "undo by hand"]);
        let x = git(dir, &["rev-parse", "HEAD"]);
        let outcome = run(&remote, "undone", &spec(&x, &r, &t0, &r));
        assert_eq!(outcome, Mechanical::AlreadyUndone);
    }

    #[test]
    fn a_hostile_host_configuration_does_not_change_the_candidate() {
        let remote = remote();
        let (t0, c, r) = merged_feature(&remote);
        let x = write(&remote.source, "later.txt", "later\n");
        let home = remote._dir.path().join("home");
        std::fs::create_dir_all(home.join("xdg/git")).unwrap();
        let hostile = "[i18n]\n\tcommitEncoding = ISO-8859-1\n[merge]\n\tconflictStyle = diff3\n[rerere]\n\tenabled = true\n";
        std::fs::write(home.join(".gitconfig"), hostile).unwrap();
        std::fs::write(home.join("xdg/git/config"), hostile).unwrap();
        let home_path = home.to_str().unwrap();
        let xdg = home.join("xdg");
        let host = [
            ("HOME", home_path),
            ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ];
        let spec = spec(&x, &r, &t0, &c);
        let dir = remote._dir.path().join("hostile");
        let hostile = compute_with(&remote.source, (&dir, "revert/hostile"), &spec, &host);
        assert_eq!(hostile.unwrap(), run(&remote, "plain", &spec));
    }

    #[test]
    fn a_later_change_to_the_same_lines_conflicts() {
        let remote = remote();
        let (t0, c, r) = merged_feature(&remote);
        let x = write(&remote.source, "feature.txt", "changed later\n");
        let outcome = run(&remote, "conflict", &spec(&x, &r, &t0, &c));
        assert_eq!(outcome, Mechanical::Conflict(vec!["feature.txt".into()]));
        let dir = remote._dir.path().join("conflict");
        assert!(!dir.exists(), "the worktree is removed");
    }
}
