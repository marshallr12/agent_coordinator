//! Step 4: show exactly what will happen before anything happens.
//!
//! `--dry-run` adds a unified diff of every file and then stops.

use serde_json::{Value, json};

use super::files::{FileChange, InstructionPlan};
use super::interview::{Settings, Target};
use super::preflight::Preflight;
use super::report::{Reporter, Status};

/// Lines of unchanged text shown around a change in a diff.
const CONTEXT: usize = 3;

/// Prints the plan (and, for a dry run, the diffs) and emits it as one event.
pub fn present(
    pre: &Preflight,
    settings: &Settings,
    binding: &Option<FileChange>,
    instructions: &InstructionPlan,
    dry_run: bool,
    reporter: &mut Reporter,
) {
    let changes: Vec<&FileChange> = binding.iter().chain(&instructions.changes).collect();
    let mut lines = plan_lines(pre, settings, &changes, instructions);
    if dry_run {
        lines.extend(changes.iter().flat_map(|change| unified_diff(change)));
    }
    reporter.text(&lines);
    let fields = plan_fields(pre, settings, &changes, instructions, dry_run);
    reporter.event("plan", Status::Done, "plan ready", fields);
}

/// The plan as a person reads it.
fn plan_lines(
    pre: &Preflight,
    settings: &Settings,
    changes: &[&FileChange],
    instructions: &InstructionPlan,
) -> Vec<String> {
    let mut lines = vec![
        format!("Service        {}", pre.service_url),
        format!("Project        {}", project_line(&settings.target)),
        "Credential     reuse the existing credential for this origin".to_owned(),
    ];
    lines.extend(file_lines(changes));
    for (file, reason) in &instructions.skipped {
        lines.push(format!("Left alone     {file}: {reason}"));
    }
    lines.push("Will not touch credentials, remotes, hooks, or any other file.".to_owned());
    let commit = if settings.commit {
        "yes, only the files listed above"
    } else {
        "no"
    };
    lines.push(format!("Commit         {commit}"));
    lines
}

/// Describes what happens to the project.
fn project_line(target: &Target) -> String {
    match target {
        Target::Link(project) => format!("link existing \"{}\" (id {})", project.name, project.id),
        Target::Create { name, .. } => format!("create \"{name}\" in the dashboard (one click)"),
    }
}

/// The "Will write" lines, or one line saying every file is current.
fn file_lines(changes: &[&FileChange]) -> Vec<String> {
    let pending: Vec<&&FileChange> = changes.iter().filter(|change| change.is_change()).collect();
    if pending.is_empty() {
        return vec!["Will write     nothing; every file is current".to_owned()];
    }
    let label = |index: usize| if index == 0 { "Will write" } else { "" };
    pending
        .iter()
        .enumerate()
        .map(|(index, change)| {
            format!(
                "{:<14} {} ({})",
                label(index),
                change.display,
                change.summary
            )
        })
        .collect()
}

/// The plan as event fields for a calling program.
fn plan_fields(
    pre: &Preflight,
    settings: &Settings,
    changes: &[&FileChange],
    instructions: &InstructionPlan,
    dry_run: bool,
) -> Value {
    let files: Vec<Value> = changes
        .iter()
        .map(|change| file_field(change, dry_run))
        .collect();
    let skip = |(file, reason): &(String, String)| json!({"path": file, "reason": reason});
    let skipped: Vec<Value> = instructions.skipped.iter().map(skip).collect();
    json!({
        "service_url": pre.service_url,
        "project": project_field(&settings.target),
        "files": files,
        "skipped": skipped,
        "commit": settings.commit,
        "dry_run": dry_run,
    })
}

/// One file of the plan; a dry run carries its diff.
fn file_field(change: &FileChange, dry_run: bool) -> Value {
    let diff = if dry_run {
        json!(unified_diff(change))
    } else {
        Value::Null
    };
    json!({"path": change.display, "action": change.summary, "changes": change.is_change(), "diff": diff})
}

/// The project part of the plan event.
fn project_field(target: &Target) -> Value {
    match target {
        Target::Link(project) => json!({"action": "link", "id": project.id, "name": project.name}),
        Target::Create {
            name,
            repository_url,
            branch,
        } => {
            json!({"action": "create", "name": name, "repository_url": repository_url, "target_branch": branch})
        }
    }
}

/// Renders one file's change as a unified diff with a single hunk.
///
/// Every edit adopt makes is one contiguous region, so trimming the common
/// first and last lines finds it without a general diff algorithm.
pub fn unified_diff(change: &FileChange) -> Vec<String> {
    if !change.is_change() {
        return Vec::new();
    }
    let old_text = String::from_utf8_lossy(change.original.as_deref().unwrap_or_default());
    let new_text = String::from_utf8_lossy(&change.updated);
    let old: Vec<&str> = old_text.lines().collect();
    let new: Vec<&str> = new_text.lines().collect();
    let (prefix, suffix) = common_ends(&old, &new);
    let from = match change.original {
        Some(_) => format!("a/{}", change.display),
        None => "/dev/null".to_owned(),
    };
    let mut lines = vec![format!("--- {from}"), format!("+++ b/{}", change.display)];
    lines.extend(hunk(&old, &new, prefix, suffix));
    lines
}

/// Counts the lines two texts share at the start and, without overlapping those, at the end.
fn common_ends(old: &[&str], new: &[&str]) -> (usize, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let tails = old.iter().rev().zip(new.iter().rev()).take(room);
    (prefix, tails.take_while(|(a, b)| a == b).count())
}

/// Builds the hunk: context, removed lines, added lines, context.
fn hunk(old: &[&str], new: &[&str], prefix: usize, suffix: usize) -> Vec<String> {
    let start = prefix.saturating_sub(CONTEXT);
    let old_end = (old.len() - suffix + CONTEXT).min(old.len());
    let new_end = (new.len() - suffix + CONTEXT).min(new.len());
    let mut lines = vec![format!(
        "@@ -{} +{} @@",
        range(start, old_end - start),
        range(start, new_end - start)
    )];
    let tagged = |tag: char, part: &[&str]| {
        part.iter()
            .map(|line| format!("{tag}{line}"))
            .collect::<Vec<_>>()
    };
    lines.extend(tagged(' ', &old[start..prefix]));
    lines.extend(tagged('-', &old[prefix..old.len() - suffix]));
    lines.extend(tagged('+', &new[prefix..new.len() - suffix]));
    lines.extend(tagged(' ', &old[old.len() - suffix..old_end]));
    lines
}

/// Formats one side of a hunk header: `start,count`, with the unified-diff rule for empty sides.
fn range(start: usize, count: usize) -> String {
    let line = if count == 0 { start } else { start + 1 };
    format!("{line},{count}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change to `AGENTS.md` from `original` (absent when `None`) to `updated`.
    fn change(original: Option<&str>, updated: &str) -> FileChange {
        FileChange {
            display: "AGENTS.md".into(),
            target: "AGENTS.md".into(),
            git_path: "AGENTS.md".into(),
            original: original.map(|text| text.as_bytes().to_vec()),
            updated: updated.as_bytes().to_vec(),
            summary: "update".into(),
        }
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let diff = unified_diff(&change(None, "one\ntwo\n"));
        assert_eq!(
            diff,
            [
                "--- /dev/null",
                "+++ b/AGENTS.md",
                "@@ -0,0 +1,2 @@",
                "+one",
                "+two"
            ]
        );
    }

    #[test]
    fn an_insertion_shows_context_on_both_sides() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nNEW\ne\nf\ng\nh\n";
        let diff = unified_diff(&change(Some(old), new));
        assert_eq!(
            diff,
            [
                "--- a/AGENTS.md",
                "+++ b/AGENTS.md",
                "@@ -2,6 +2,7 @@",
                " b",
                " c",
                " d",
                "+NEW",
                " e",
                " f",
                " g"
            ]
        );
    }

    #[test]
    fn a_replacement_lists_removed_then_added_lines() {
        let diff = unified_diff(&change(Some("keep\nold\nkeep\n"), "keep\nnew\nkeep\n"));
        assert_eq!(
            diff[2..],
            ["@@ -1,3 +1,3 @@", " keep", "-old", "+new", " keep"]
        );
        assert!(unified_diff(&change(Some("same\n"), "same\n")).is_empty());
    }

    #[test]
    fn only_pending_changes_are_listed_to_write() {
        let current = change(Some("same\n"), "same\n");
        let pending = change(None, "new\n");
        assert_eq!(
            file_lines(&[&current]),
            ["Will write     nothing; every file is current"]
        );
        assert_eq!(
            file_lines(&[&current, &pending]),
            ["Will write     AGENTS.md (update)"]
        );
    }
}
