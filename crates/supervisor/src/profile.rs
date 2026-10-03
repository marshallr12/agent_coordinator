//! Exact, deterministic launch profiles for supervised harness runs
//! (autonomy plan §2.3). A profile is data: program, arguments, a fully
//! replaced environment and a working directory. Nothing inherits from the
//! invoking shell, so production credentials can never leak into a launch.
//!
//! Candidate-controlled instructions never load: Claude runs with
//! `--safe-mode` (no CLAUDE.md, hooks, skills, plugins or MCP; verified by
//! probe under subscription auth) and Codex with `project_doc_max_bytes=0`
//! (no AGENTS.md; repo `.codex/` is ignored for untrusted directories).
use crate::config::Config;
use crate::confine::StatePaths;
use crate::role_settings;
use clap::ValueEnum;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Which side of a review a launch works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Role {
    Implementer,
    Reviewer,
}

/// Which vendor harness runs the launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Claude,
    Codex,
}

/// What varies per launch.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub role: Role,
    pub harness: Harness,
    /// Per-launch clone owned by the role's uid; the harness working directory.
    pub clone: PathBuf,
    /// Per-launch directory outside the clone: prompt, settings, outputs, caches.
    pub run: PathBuf,
    pub model: String,
    pub effort: String,
    pub session_id: Uuid,
    /// Coordinator project the launch works on; selects `verification_env`.
    pub project: Option<String>,
    /// Coordinator task the launch works on; with `session_id`, it names the
    /// candidate ref an implementer's push helper publishes to.
    pub task: Option<String>,
    /// The implementer's candidate-push helper socket, exported to the
    /// harness as [`PUSH_SOCKET_ENV`] (see `push_helper`).
    pub push_socket: Option<PathBuf>,
}

/// The variable through which `agent-coordinator` finds the candidate-push
/// helper socket.
pub const PUSH_SOCKET_ENV: &str = "AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET";

/// The variable through which Claude Code receives the role's long-lived
/// `claude setup-token` token. `launch::spawn` adds it to the child process
/// only, so it is never part of a [`LaunchCommand`].
pub const CLAUDE_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The role's Claude token file, `<state_dir>/<role slug>/claude-token`: in
/// the role directory, outside the read-only `claude-config`.
pub fn claude_token(role: Role, config: &Config) -> PathBuf {
    config.state_dir.join(role.slug()).join("claude-token")
}

/// A fully resolved process to spawn; `stdin` is the prompt file.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, OsString)>,
    pub cwd: PathBuf,
    pub stdin: PathBuf,
}

impl Role {
    /// Directory name used under the state directory.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Implementer => "impl",
            Self::Reviewer => "rev",
        }
    }

    /// The Unix account a launch of this role must run as.
    pub fn user(self, config: &Config) -> &str {
        match self {
            Self::Implementer => &config.implementer_user,
            Self::Reviewer => &config.reviewer_user,
        }
    }
}

/// Files the supervisor writes into `$RUN` before a launch.
pub mod run_files {
    pub const PROMPT: &str = "prompt.md";
    pub const SETTINGS: &str = "role-settings.json";
    pub const SCHEMA: &str = "result.schema.json";
    pub const LAST_MESSAGE: &str = "last.md";
    pub const VERIFICATION: &str = "verification.json";
    pub const VERIFICATION_SESSION: &str = "verification-session.json";
    pub const CANDIDATE_SHELL: &str = "candidate-shell";
}

/// Structured verdict every reviewer launch must return (plan §2.3, M5): the
/// supervisor, not the model, posts it with a separate reviewer credential.
pub fn review_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["decision", "findings", "criteria_evidence"],
        "properties": {
            "decision": {"enum": ["approve", "request_changes"]},
            "findings": {"type": "array", "items": {"type": "string"}},
            "criteria_evidence": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["criterion", "evidence"],
                    "properties": {"criterion": {"type": "string"}, "evidence": {"type": "string"}}
                }
            }
        }
    })
}

/// Builds the launch command for `spec` on this host.
pub fn command(spec: &LaunchSpec, config: &Config) -> LaunchCommand {
    let (program, args) = match spec.harness {
        Harness::Claude => ("claude", claude_args(spec)),
        Harness::Codex => ("codex", codex_args(spec)),
    };
    LaunchCommand {
        program: config.bin_dir.join(program),
        args,
        env: environment(spec, config),
        cwd: spec.clone.clone(),
        stdin: spec.run.join(run_files::PROMPT),
    }
}

/// Claude Code flags: print mode, safe mode, generated role settings only,
/// nothing can prompt, structured stream output.
fn claude_args(spec: &LaunchSpec) -> Vec<OsString> {
    let mut args = strings(&[
        "-p",
        "--safe-mode",
        "--setting-sources",
        "user",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--strict-mcp-config",
        "--output-format",
        "stream-json",
        "--verbose",
    ]);
    push_pair(&mut args, "--settings", spec.run.join(run_files::SETTINGS));
    push_pair(&mut args, "--session-id", spec.session_id.to_string());
    push_pair(&mut args, "--model", &spec.model);
    push_pair(&mut args, "--effort", &spec.effort);
    if spec.role == Role::Reviewer {
        push_pair(&mut args, "--json-schema", review_schema().to_string());
    }
    args.push("--tools".into());
    args.extend(
        role_settings::settings(spec.role)["permissions"]["allow"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.as_str().map(OsString::from)),
    );
    args
}

/// Codex flags: no user config or rules, no AGENTS.md, workspace-write
/// sandbox with network (the host firewall and proxy enforce egress), never
/// ask for approval, JSON events and a last-message file.
fn codex_args(spec: &LaunchSpec) -> Vec<OsString> {
    let mut args = strings(&["exec", "--ignore-user-config", "--ignore-rules", "--json"]);
    push_pair(&mut args, "-m", &spec.model);
    for setting in [
        format!("model_reasoning_effort={}", spec.effort),
        "project_doc_max_bytes=0".into(),
        "sandbox_workspace_write.network_access=true".into(),
        "approval_policy=never".into(),
        "shell_environment_policy.inherit=all".into(),
        "check_for_update_on_startup=false".into(),
    ] {
        push_pair(&mut args, "-c", setting);
    }
    push_pair(&mut args, "-s", "workspace-write");
    push_pair(&mut args, "-C", &spec.clone);
    push_pair(&mut args, "--add-dir", &spec.run);
    push_pair(&mut args, "-o", spec.run.join(run_files::LAST_MESSAGE));
    if spec.role == Role::Reviewer {
        push_pair(
            &mut args,
            "--output-schema",
            spec.run.join(run_files::SCHEMA),
        );
    }
    args.push("-".into());
    args
}

/// The complete environment of a launch; nothing else is inherited. Mutable
/// home, Cargo (including caches), and coordinator state belong to this run.
/// Claude's token is not here: `launch::spawn` reads it from [`claude_token`]
/// and passes it to the child only. These paths alone do not prevent another
/// process with the same uid from writing them.
fn environment(spec: &LaunchSpec, config: &Config) -> Vec<(String, OsString)> {
    let role_dir = config.state_dir.join(spec.role.slug());
    let state = StatePaths::new(&spec.run);
    let toolchain = &config.toolchain_dir;
    let path = format!(
        "{}:{}:/usr/local/bin:/usr/bin:/bin",
        config.bin_dir.display(),
        toolchain.join("cargo/bin").display()
    );
    let mut env = vec![
        ("PATH".into(), path.into()),
        ("HOME".into(), state.home.into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("TMPDIR".into(), spec.run.join("tmp").into()),
        ("CARGO_TARGET_DIR".into(), spec.run.join("target").into()),
        ("RUSTUP_HOME".into(), toolchain.join("rustup").into()),
        ("CARGO_HOME".into(), state.cargo.into()),
        // Pinned harnesses must never replace themselves (plan §2.3).
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("AGENT_COORDINATOR_HOME".into(), state.coordinator.into()),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
        ("no_proxy".into(), "127.0.0.1,localhost".into()),
    ];
    for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
        env.push((name.into(), config.egress_proxy_url().into()));
    }
    env.push(match spec.harness {
        Harness::Claude => (
            "CLAUDE_CONFIG_DIR".into(),
            role_dir.join("claude-config").into(),
        ),
        Harness::Codex => ("CODEX_HOME".into(), role_dir.join("codex-home").into()),
    });
    env.extend(crate::verification::environment(spec, config));
    if let Some(socket) = &spec.push_socket {
        env.push((PUSH_SOCKET_ENV.into(), socket.into()));
    }
    if crate::candidate::applies(spec) {
        // Every reviewer Bash command runs as candidate code (R-P3b.3).
        env.push((
            "CLAUDE_CODE_SHELL_PREFIX".into(),
            crate::candidate::shell_path(spec).into(),
        ));
    }
    env
}

/// Converts static strings to owned OS strings.
fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// Appends a flag and its value.
fn push_pair(args: &mut Vec<OsString>, flag: &str, value: impl AsRef<std::ffi::OsStr>) {
    args.push(flag.into());
    args.push(value.as_ref().to_owned());
}

/// True when `inner` is `outer` or lies inside it (lexically).
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    inner.starts_with(outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(role: Role, harness: Harness) -> LaunchSpec {
        LaunchSpec {
            role,
            harness,
            clone: "/var/lib/agentc/impl/clones/a1".into(),
            run: "/var/lib/agentc/impl/runs/a1".into(),
            model: "m".into(),
            effort: "high".into(),
            session_id: Uuid::nil(),
            project: None,
            task: None,
            push_socket: None,
        }
    }

    fn has(command: &LaunchCommand, flag: &str) -> bool {
        command.args.iter().any(|arg| arg == flag)
    }

    #[test]
    fn claude_profile_blocks_candidate_instructions_and_prompts() {
        let command = command(
            &spec(Role::Implementer, Harness::Claude),
            &Config::default(),
        );
        for flag in ["--safe-mode", "--strict-mcp-config", "dontAsk", "none"] {
            assert!(has(&command, flag), "{flag}");
        }
        assert_eq!(command.program, PathBuf::from("/opt/agentc/bin/claude"));
    }

    #[test]
    fn codex_profile_disables_agents_md_and_approvals() {
        let command = command(&spec(Role::Reviewer, Harness::Codex), &Config::default());
        for flag in [
            "project_doc_max_bytes=0",
            "approval_policy=never",
            "--ignore-user-config",
            "--output-schema",
        ] {
            assert!(has(&command, flag), "{flag}");
        }
    }

    #[test]
    fn only_a_launch_given_a_push_socket_exports_it() {
        let variable = |command: &LaunchCommand| {
            let found = command.env.iter().find(|(name, _)| name == PUSH_SOCKET_ENV);
            found.map(|(_, value)| value.clone())
        };
        for harness in [Harness::Claude, Harness::Codex] {
            let mut spec = spec(Role::Implementer, harness);
            assert_eq!(variable(&command(&spec, &Config::default())), None);
            spec.push_socket = Some("/var/lib/agentc/push/l/sock/push.sock".into());
            let exported = variable(&command(&spec, &Config::default()));
            assert_eq!(
                exported,
                Some("/var/lib/agentc/push/l/sock/push.sock".into())
            );
        }
    }

    #[test]
    fn environment_is_replaced_and_never_carries_coordinator_tokens() {
        for harness in [Harness::Claude, Harness::Codex] {
            let command = command(&spec(Role::Implementer, harness), &Config::default());
            assert!(command.env.iter().all(|(name, _)| !name.contains("TOKEN")));
            assert!(
                command
                    .env
                    .iter()
                    .any(|(name, value)| name == "HTTPS_PROXY" && value == "http://127.0.0.1:3128")
            );
        }
    }

    #[test]
    fn the_claude_token_sits_in_the_role_directory_outside_claude_config() {
        let config = Config::default();
        let token = claude_token(Role::Reviewer, &config);
        assert_eq!(token, config.state_dir.join("rev/claude-token"));
        assert!(!token.starts_with(config.state_dir.join("rev/claude-config")));
    }
}
