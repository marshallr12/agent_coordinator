//! Claude Code settings for supervised launches, generated in code.
//!
//! `claude -p` silently ignores settings it cannot parse, so the files are
//! never hand-edited: the preflight requires the installed file to equal this
//! output. Only keys verified by the P2 probes are used (`permissions` with
//! `defaultMode`, and `sandbox.enabled`).
//!
//! The persistent `CLAUDE.md` is generated the same way, so a role's standing
//! instructions are reviewed code and the preflight can require them exactly.
use crate::profile::Role;
use serde_json::{Value, json};

/// Tool rules every supervised launch denies: raw publication, remote and
/// config changes, privilege escalation and web tools (egress goes through the
/// allowlisting proxy instead).
const COMMON_DENY: &[&str] = &[
    "Bash(git push:*)",
    "Bash(git remote:*)",
    "Bash(git config:*)",
    "Bash(sudo:*)",
    "WebFetch",
    "WebSearch",
];

/// Extra denials for reviewers, whose clone is evidence, not a workspace.
const REVIEWER_DENY: &[&str] = &[
    "Edit",
    "Write",
    "NotebookEdit",
    "Bash(git commit:*)",
    "Bash(git tag:*)",
];

/// Tools each role may use without prompting (nothing can prompt).
fn allow(role: Role) -> Vec<&'static str> {
    match role {
        Role::Implementer => vec!["Read", "Edit", "Write", "Glob", "Grep", "Bash"],
        Role::Reviewer => vec!["Read", "Glob", "Grep", "Bash"],
    }
}

/// Denials for a role.
fn deny(role: Role) -> Vec<&'static str> {
    let mut rules = COMMON_DENY.to_vec();
    if role == Role::Reviewer {
        rules.extend_from_slice(REVIEWER_DENY);
    }
    rules
}

/// The settings document for a role.
pub fn settings(role: Role) -> Value {
    json!({
        "permissions": {
            "allow": allow(role),
            "deny": deny(role),
            "defaultMode": "dontAsk"
        },
        "sandbox": {"enabled": true}
    })
}

/// The exact file contents the launcher and preflight expect.
pub fn render(role: Role) -> String {
    let mut text = serde_json::to_string_pretty(&settings(role)).expect("static JSON");
    text.push('\n');
    text
}

/// Standing instructions both roles carry in their persistent `CLAUDE.md`.
///
/// Claude Code writes a background command's output under its own temp
/// directory (`$RUN/tmp/claude-<uid>/.../tasks/<id>.output`). A reviewer's
/// candidate shell mounts a fresh tmpfs over that directory so candidate code
/// cannot read the harness's temp, so a shell loop that waits for the file
/// never sees it and spins until the launch ends. The harness's own tools
/// reach the file from outside the sandbox, so agents wait with those.
const WAITING: &str = "\
# Waiting for background work

Wait for a background command with the harness's own tools: read its output
with the background-task output tool, or act on its completion notification.
Never poll the harness's task output files, or anything under its temp
directory (`$RUN/tmp`, the `tasks/*.output` paths), from a shell loop such as
`until grep ...; do sleep ...; done`. Commands run through `candidate-shell`
cannot see that directory, so such a loop never ends and keeps running until
the launch does. A shell loop cannot see processes outside the candidate
sandbox either, so do not wait on `pgrep` for them.

To run a long command (a test or lint gate, for example), run it in the
foreground with its output redirected into `$TMPDIR`, and read that file when
the command returns.
";

/// The exact `CLAUDE.md` contents the launcher and preflight expect.
pub fn instructions(_role: Role) -> &'static str {
    WAITING
}

/// True when `installed` is semantically identical to the generated settings.
pub fn matches(role: Role, installed: &str) -> bool {
    serde_json::from_str::<Value>(installed).is_ok_and(|value| value == settings(role))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_denies_raw_push_and_reviewers_cannot_edit() {
        for role in [Role::Implementer, Role::Reviewer] {
            assert!(deny(role).contains(&"Bash(git push:*)"));
        }
        assert!(deny(Role::Reviewer).contains(&"Edit"));
        assert!(!allow(Role::Reviewer).contains(&"Write"));
    }

    #[test]
    fn every_role_is_told_to_wait_with_the_harness_tools_and_not_a_shell_loop() {
        for role in [Role::Implementer, Role::Reviewer] {
            let text = instructions(role);
            for expected in [
                "background-task output tool",
                "completion notification",
                "Never poll the harness's task output files",
                "$RUN/tmp",
                "tasks/*.output",
                "candidate-shell",
                "foreground",
                "$TMPDIR",
            ] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
        }
    }

    #[test]
    fn rendered_files_round_trip_and_edits_are_detected() {
        let text = render(Role::Implementer);
        assert!(matches(Role::Implementer, &text));
        assert!(!matches(Role::Reviewer, &text));
        assert!(!matches(
            Role::Implementer,
            &text.replace("dontAsk", "acceptEdits")
        ));
        assert!(!matches(Role::Implementer, "{not json"));
    }
}
