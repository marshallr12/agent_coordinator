//! Step 8: commit exactly the files adopt wrote. Local only; never pushes.
//!
//! The default is to commit, because the startup guide forbids `git pull` in
//! a dirty checkout and other clones and worktrees only receive the binding
//! through a commit. A commit problem is reported and never fails the run:
//! the files are correct either way.

use std::path::Path;

use serde_json::json;

use super::git;
use super::report::{Reporter, Status};

/// The message of the commit adopt makes. No attribution trailer is added.
pub const MESSAGE: &str = "Connect repository to Agent Coordinator";

/// Commits adopt's files when asked to and safe; returns the commit ID.
///
/// `managed` is every path the commit may include (see `commit_paths`);
/// `written` is what this run changed. A run that wrote nothing commits nothing.
pub fn run(
    root: &Path,
    managed: &[String],
    written: &[String],
    wanted: bool,
    reporter: &mut Reporter,
) -> Option<String> {
    let paths = match paths_to_commit(root, managed, written, wanted) {
        Ok(paths) => paths,
        Err(reason) => {
            reporter.event("commit", Status::Skipped, &reason, json!({}));
            return None;
        }
    };
    match git::commit(root, &paths, MESSAGE) {
        Ok(id) => Some(report_commit(id, reporter)),
        Err(error) => report_failure(&error, reporter),
    }
}

/// Reports the new commit and hands its ID back.
fn report_commit(id: String, reporter: &mut Reporter) -> String {
    let summary = format!("{} (local only; nothing was pushed)", short(&id));
    reporter.event("commit", Status::Done, &summary, json!({"commit": id}));
    id
}

/// Reports a failed commit as a warning; the written files stay as they are.
fn report_failure(error: &anyhow::Error, reporter: &mut Reporter) -> Option<String> {
    let message = format!("The commit failed and the files were left in place: {error:#}");
    reporter.warn("commit", message);
    reporter.event("commit", Status::Failed, "commit failed", json!({}));
    None
}

/// Returns the paths to commit, or why no commit is made.
fn paths_to_commit(
    root: &Path,
    managed: &[String],
    written: &[String],
    wanted: bool,
) -> Result<Vec<String>, String> {
    if !wanted {
        return Err("not requested (--no-commit)".to_owned());
    }
    if written.is_empty() {
        return Err("nothing was written".to_owned());
    }
    let text = |error: anyhow::Error| format!("{error:#}");
    let paths = git::dirty_paths(root, managed).map_err(text)?;
    if paths.is_empty() {
        return Err("the files are already committed".to_owned());
    }
    match git::staged_outside(root, &paths).map_err(text)?.as_slice() {
        [] => Ok(paths),
        staged => Err(format!(
            "other files are already staged ({}); commit adopt's files yourself",
            staged.join(", ")
        )),
    }
}

/// The first seven characters of a commit ID.
fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::testing::{Capture, TestRepo};

    /// Runs the commit step for AGENTS.md and returns the commit ID and the output.
    fn commit_step(repo: &TestRepo, wanted: bool) -> (Option<String>, String) {
        let capture = Capture::default();
        let mut reporter = Reporter::new(false, Box::new(capture.clone()));
        let paths = vec!["AGENTS.md".to_owned()];
        let id = run(repo.root(), &paths, &paths, wanted, &mut reporter);
        (id, capture.text())
    }

    #[test]
    fn only_adopts_paths_are_committed_and_nothing_is_pushed() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "block\n");
        repo.write("notes.txt", "mine\n");
        let (id, output) = commit_step(&repo, true);
        assert!(id.is_some(), "{output}");
        assert_eq!(repo.git(&["log", "-1", "--format=%s"]).trim(), MESSAGE);
        assert_eq!(
            repo.git(&["show", "--name-only", "--format=", "HEAD"])
                .trim(),
            "AGENTS.md"
        );
        assert_eq!(repo.git(&["status", "--porcelain"]).trim(), "?? notes.txt");
    }

    #[test]
    fn someone_elses_staged_work_is_never_swept_up() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "block\n");
        repo.write("staged.txt", "theirs\n");
        repo.git(&["add", "staged.txt"]);
        let (id, output) = commit_step(&repo, true);
        assert_eq!(id, None);
        assert!(output.contains("already staged (staged.txt)"), "{output}");
        assert!(
            repo.git(&["status", "--porcelain"])
                .contains("A  staged.txt")
        );
    }

    #[test]
    fn no_commit_is_made_when_declined_or_already_committed() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "block\n");
        let (declined, output) = commit_step(&repo, false);
        assert_eq!(declined, None);
        assert!(output.contains("--no-commit"), "{output}");
        repo.commit_file("AGENTS.md", "block\n");
        let (again, output) = commit_step(&repo, true);
        assert_eq!(again, None);
        assert!(output.contains("already committed"), "{output}");
    }

    #[test]
    fn a_failing_hook_is_reported_and_leaves_the_files() {
        let repo = TestRepo::new();
        repo.write("AGENTS.md", "block\n");
        repo.write_hook(
            "pre-commit",
            "#!/bin/sh\necho refused by hook >&2\nexit 1\n",
        );
        let (id, output) = commit_step(&repo, true);
        assert_eq!(id, None);
        assert!(output.contains("The commit failed"), "{output}");
        assert_eq!(repo.read("AGENTS.md"), "block\n");
    }
}
