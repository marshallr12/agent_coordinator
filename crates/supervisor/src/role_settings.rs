//! Claude Code settings for supervised launches, generated in code.
//!
//! `claude -p` silently ignores settings it cannot parse, so the files are
//! never hand-edited: the preflight requires the installed file to equal this
//! output. Only keys verified by live probes are used (`permissions` with
//! `defaultMode` and `sandbox.enabled` in P2; `sandbox.network.allowedDomains`
//! on Claude Code 2.1.303, 2026-11-06).
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

/// Whether Claude's own Bash sandbox is on for a role.
///
/// It is off for implementers: it starts a nested Bubblewrap, and the
/// implementer's outer sandbox runs with `--disable-userns`, under which every
/// further user namespace fails with ENOSPC. Left on, it either breaks every
/// Bash command (when `socat`, one of its dependencies, is installed) or is
/// silently inactive (when it is not). The outer sandbox is the write boundary.
/// Reviewers' outer sandbox permits nesting, so theirs stays on.
fn claude_sandbox(role: Role) -> bool {
    role != Role::Implementer
}

/// Hosts a reviewer's Bash commands may reach through Claude's own sandbox.
///
/// Where that sandbox is active (a host with `socat`), it tunnels Bash network
/// traffic through its own proxy, which in `-p` mode denies every host not
/// listed here (`CONNECT 403`) before the egress proxy is consulted. Reviewers
/// run hostile candidate code, so only the registry Cargo needs is listed:
/// the sparse index and crate downloads. The egress proxy still applies.
const REVIEWER_SANDBOX_DOMAINS: &[&str] = &["crates.io", "index.crates.io", "static.crates.io"];

/// The `sandbox` settings for a role: off for implementers; on for
/// reviewers, with network access limited to [`REVIEWER_SANDBOX_DOMAINS`].
fn sandbox(role: Role) -> Value {
    if !claude_sandbox(role) {
        return json!({"enabled": false});
    }
    json!({
        "enabled": true,
        "network": {"allowedDomains": REVIEWER_SANDBOX_DOMAINS}
    })
}

/// The settings document for a role.
pub fn settings(role: Role) -> Value {
    json!({
        "permissions": {
            "allow": allow(role),
            "deny": deny(role),
            "defaultMode": "dontAsk"
        },
        "sandbox": sandbox(role)
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
    fn only_implementers_turn_claudes_nested_bash_sandbox_off() {
        assert_eq!(settings(Role::Implementer)["sandbox"]["enabled"], false);
        assert_eq!(settings(Role::Reviewer)["sandbox"]["enabled"], true);
        assert!(render(Role::Implementer).contains("\"enabled\": false"));
    }

    #[test]
    fn only_reviewers_reach_exactly_the_crates_registry_through_claudes_sandbox() {
        let domains = &settings(Role::Reviewer)["sandbox"]["network"]["allowedDomains"];
        assert_eq!(
            domains,
            &json!(["crates.io", "index.crates.io", "static.crates.io"])
        );
        assert!(
            settings(Role::Implementer)["sandbox"]
                .get("network")
                .is_none()
        );
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
