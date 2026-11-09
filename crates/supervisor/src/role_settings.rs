//! Claude Code settings for supervised launches, generated in code.
//!
//! `claude -p` silently ignores settings it cannot parse, so the files are
//! never hand-edited: the preflight requires the installed file to equal this
//! output. Only keys verified by live probes are used (`permissions` with
//! `defaultMode` and `sandbox.enabled` in P2; `sandbox.network.allowedDomains`
//! and `sandbox.filesystem.allowWrite` on Claude Code 2.1.303, 2026-11-06;
//! `sandbox.allowNestedUserNamespaces` and `sandbox.network.allowAllUnixSockets`
//! on 2.1.303 and 2.1.319, 2026-11-06).
//!
//! The persistent `CLAUDE.md` is generated the same way, so a role's standing
//! instructions are reviewed code and the preflight can require them exactly.
use crate::profile::Role;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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

/// The directory holding every launch's `$RUN` for a role.
///
/// Claude's sandbox, where active, makes everything outside the working
/// directory and its own temp read-only, including the run's Cargo home,
/// build directory and candidate home and temp. Its `allowWrite` takes no
/// globs, and the persistent settings are shared by every run, so the grant
/// is the whole role `runs/` directory. That widens nothing: the outer launch
/// sandbox hides other runs under a tmpfs and keeps this run read-only except
/// its own writable directories, and `candidate-shell` narrows it further.
fn runs_dir(role: Role, state_dir: &Path) -> PathBuf {
    state_dir.join(role.slug()).join("runs")
}

/// The reviewer's `sandbox.network` settings: [`REVIEWER_SANDBOX_DOMAINS`],
/// plus Unix sockets.
///
/// Where Claude's sandbox is active it blocks creating any Unix socket, so
/// workspace tests that serve on one (coordinator-cli `candidate_checkpoint`,
/// agentc-push serve/socket, agentc-supervisor relay/push-helper) fail with
/// `Operation not permitted`. Probed live on mxmini, 2026-11-06, Claude Code
/// 2.1.303 and 2.1.319: the path list `allowUnixSockets` has no effect on
/// Linux (bind still denied), so `allowAllUnixSockets` is the narrowest
/// setting that works. It matches hosts where the sandbox is inactive
/// (oracle-1, no `socat`); the outer sandbox's network namespace still hides
/// abstract sockets, and the agent uid limits which socket files connect.
fn reviewer_network() -> Value {
    json!({"allowedDomains": REVIEWER_SANDBOX_DOMAINS, "allowAllUnixSockets": true})
}

/// The `sandbox` settings for a role: off for implementers; on for
/// reviewers, with [`reviewer_network`], writes allowed under the role's
/// [`runs_dir`], and nested user namespaces.
///
/// `candidate-shell` is itself a Bubblewrap sandbox, so it needs a user
/// namespace inside Claude's. Claude Code 2.1.319 denies that by default
/// ("bwrap: No permissions to create new namespace") unless
/// `allowNestedUserNamespaces` is set. Probed live on mxmini, 2026-11-06:
/// with it, `unshare --user` and `bwrap --unshare-user` succeed in 2.1.319's
/// sandbox and fail without it; 2.1.303 nests either way and ignores the key.
/// Claude warns that nested namespaces get around its file deny rules, but a
/// reviewer's file isolation comes from the outer launch sandbox and
/// `candidate-shell`, not from Claude's sandbox.
fn sandbox(role: Role, state_dir: &Path) -> Value {
    if !claude_sandbox(role) {
        return json!({"enabled": false});
    }
    json!({
        "enabled": true,
        "allowNestedUserNamespaces": true,
        "network": reviewer_network(),
        "filesystem": {"allowWrite": [runs_dir(role, state_dir)]}
    })
}

/// Tools a role may use without prompting, as passed to `--tools`.
pub fn tools(role: Role) -> Vec<&'static str> {
    allow(role)
}

/// The settings document for a role on a host whose state root is `state_dir`.
pub fn settings(role: Role, state_dir: &Path) -> Value {
    json!({
        "permissions": {
            "allow": allow(role),
            "deny": deny(role),
            "defaultMode": "dontAsk"
        },
        "sandbox": sandbox(role, state_dir)
    })
}

/// The exact file contents the launcher and preflight expect.
pub fn render(role: Role, state_dir: &Path) -> String {
    let mut text = serde_json::to_string_pretty(&settings(role, state_dir)).expect("static JSON");
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
pub fn matches(role: Role, state_dir: &Path, installed: &str) -> bool {
    serde_json::from_str::<Value>(installed).is_ok_and(|value| value == settings(role, state_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default state root, as on both supervised hosts.
    const STATE: &str = "/var/lib/agentc";

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
        assert_eq!(
            settings(Role::Implementer, Path::new(STATE))["sandbox"]["enabled"],
            false
        );
        assert_eq!(
            settings(Role::Reviewer, Path::new(STATE))["sandbox"]["enabled"],
            true
        );
        assert!(render(Role::Implementer, Path::new(STATE)).contains("\"enabled\": false"));
    }

    #[test]
    fn only_reviewers_reach_exactly_the_crates_registry_through_claudes_sandbox() {
        let domains =
            &settings(Role::Reviewer, Path::new(STATE))["sandbox"]["network"]["allowedDomains"];
        assert_eq!(
            domains,
            &json!(["crates.io", "index.crates.io", "static.crates.io"])
        );
        assert!(
            settings(Role::Implementer, Path::new(STATE))["sandbox"]
                .get("network")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_reviewers_may_write_under_their_roles_runs_directory() {
        let writable =
            &settings(Role::Reviewer, Path::new(STATE))["sandbox"]["filesystem"]["allowWrite"];
        assert_eq!(writable, &json!(["/var/lib/agentc/rev/runs"]));
        let implementer = settings(Role::Implementer, Path::new(STATE));
        assert!(implementer["sandbox"].get("filesystem").is_none());
        let elsewhere = render(Role::Reviewer, Path::new("/srv/agentc"));
        assert!(!matches(Role::Reviewer, Path::new(STATE), &elsewhere));
    }

    /// The whole reviewer sandbox document is pinned, so a probed key cannot
    /// drift or vanish without a reviewed test change.
    #[cfg(unix)]
    #[test]
    fn reviewer_sandbox_document_is_pinned() {
        assert_eq!(
            settings(Role::Reviewer, Path::new(STATE))["sandbox"],
            json!({
                "enabled": true,
                "allowNestedUserNamespaces": true,
                "network": {
                    "allowedDomains": ["crates.io", "index.crates.io", "static.crates.io"],
                    "allowAllUnixSockets": true
                },
                "filesystem": {"allowWrite": ["/var/lib/agentc/rev/runs"]}
            })
        );
    }

    /// Implementers keep Claude's sandbox off and gain none of the reviewer
    /// keys (their outer sandbox forbids nesting; see [`claude_sandbox`]).
    #[test]
    fn implementer_sandbox_document_is_pinned() {
        assert_eq!(
            settings(Role::Implementer, Path::new(STATE))["sandbox"],
            json!({"enabled": false})
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
        let text = render(Role::Implementer, Path::new(STATE));
        assert!(matches(Role::Implementer, Path::new(STATE), &text));
        assert!(!matches(Role::Reviewer, Path::new(STATE), &text));
        assert!(!matches(
            Role::Implementer,
            Path::new(STATE),
            &text.replace("dontAsk", "acceptEdits")
        ));
        assert!(!matches(Role::Implementer, Path::new(STATE), "{not json"));
    }
}
