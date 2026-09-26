//! Claude Code settings for supervised launches, generated in code.
//!
//! `claude -p` silently ignores settings it cannot parse, so the files are
//! never hand-edited: the preflight requires the installed file to equal this
//! output. Only keys verified by the P2 probes are used (`permissions` with
//! `defaultMode`, and `sandbox.enabled`).
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
