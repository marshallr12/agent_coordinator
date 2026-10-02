//! Reviewer candidate-code isolation (R-P3b.3, decision U18).
//!
//! A reviewer builds and tests the implementer's candidate, so every command
//! its Bash tool runs may execute hostile code (`build.rs`, tests, UI
//! scripts). Claude Code passes each Bash command, as one shell string, to
//! the executable named by `CLAUDE_CODE_SHELL_PREFIX`. The supervisor points
//! that at a generated, read-only `$RUN/candidate-shell`, which runs the
//! command in an inner Bubblewrap: the whole state directory is replaced by an
//! empty tmpfs, only this launch's clone (read-only) and build/home/Cargo
//! directories (writable) return, the Claude login is masked by `/dev/null`,
//! coordinator state stays hidden, credential variables are unset, and no
//! further user namespace can be created. Codex has no equivalent hook, so
//! Codex reviewer launches are refused until it does.
use crate::config::Config;
use crate::confine::{self, StatePaths};
use crate::profile::{Harness, LaunchSpec, Role, run_files};
use anyhow::{Context, Result, ensure};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variables a candidate never inherits, even if a harness or
/// host exported them: provider, GitHub and harness credentials.
pub const UNSET: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_SHELL_PREFIX",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "TYPESAFE_API_KEY",
];

/// True for launches whose Bash commands run as candidate code.
pub fn applies(spec: &LaunchSpec) -> bool {
    spec.role == Role::Reviewer && spec.harness == Harness::Claude
}

/// The generated prefix executable for `spec`.
pub fn shell_path(spec: &LaunchSpec) -> PathBuf {
    spec.run.join(run_files::CANDIDATE_SHELL)
}

/// The candidate's own home. Inside the inner sandbox it is mounted over the
/// harness's `$HOME`, so planted dotfiles (`.gitconfig`, `.profile`) never
/// reach the credentialed harness, which runs git and login shells outside
/// the prefix.
pub fn home(spec: &LaunchSpec) -> PathBuf {
    spec.run.join("candidate-home")
}

/// Why `spec` cannot isolate candidate code, if it cannot.
pub fn unsupported(spec: &LaunchSpec) -> Option<String> {
    (spec.role == Role::Reviewer && spec.harness == Harness::Codex).then(|| {
        "Codex reviewer launches cannot isolate candidate code from reviewer secrets \
         (R-P3b.3); use a Claude reviewer"
            .into()
    })
}

/// The Claude login file the candidate must never read.
fn credential_file(spec: &LaunchSpec, config: &Config) -> PathBuf {
    claude_config(spec, config).join(".credentials.json")
}

/// The persistent Claude configuration directory of the launch's role.
fn claude_config(spec: &LaunchSpec, config: &Config) -> PathBuf {
    config
        .state_dir
        .join(spec.role.slug())
        .join("claude-config")
}

/// Namespace and privilege flags; the inner sandbox forbids further nesting.
fn isolation_args() -> Vec<OsString> {
    [
        "--unshare-user",
        "--disable-userns",
        "--assert-userns-disabled",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--remount-ro",
        "/proc",
        "--dev",
        "/dev",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Appends `option source destination` with the same path on both sides.
fn same(args: &mut Vec<OsString>, option: &str, path: &Path) {
    args.extend([option.into(), path.into(), path.into()]);
}

/// The inner sandbox's mount and environment arguments, before `--`.
/// Bind sources resolve against the outer view, so paths covered by the tmpfs
/// can be re-attached individually.
pub fn inner_args(spec: &LaunchSpec, config: &Config) -> Vec<OsString> {
    let state = StatePaths::new(&spec.run);
    let mut args = isolation_args();
    args.extend(["--tmpfs".into(), config.state_dir.clone().into_os_string()]);
    same(&mut args, "--ro-bind", &spec.clone);
    for writable in [spec.run.join("tmp"), spec.run.join("target"), state.cargo] {
        same(&mut args, "--bind", &writable);
    }
    args.extend(["--bind".into(), home(spec).into(), state.home.into()]);
    same(
        &mut args,
        "--ro-bind-try",
        &spec.run.join(run_files::VERIFICATION),
    );
    // Claude Code's shell snapshots live here; only the login is withheld.
    same(&mut args, "--ro-bind", &claude_config(spec, config));
    args.extend([
        "--ro-bind".into(),
        "/dev/null".into(),
        credential_file(spec, config).into_os_string(),
    ]);
    for name in UNSET {
        args.extend(["--unsetenv".into(), (*name).into()]);
    }
    args
}

/// Quotes one argument for POSIX `sh`; non-UTF-8 paths are refused.
fn quote(argument: &OsString) -> Result<String> {
    let text = argument
        .to_str()
        .context("candidate sandbox paths must be UTF-8")?;
    ensure!(
        !text.contains('\0'),
        "candidate sandbox arguments cannot contain NUL"
    );
    Ok(format!("'{}'", text.replace('\'', r"'\''")))
}

/// The complete prefix script: one command string in `$1`, run by Bash in
/// the inner sandbox from the caller's working directory.
pub fn script(spec: &LaunchSpec, config: &Config) -> Result<String> {
    let mut words = vec![quote(&config.bubblewrap.clone().into_os_string())?];
    for argument in inner_args(spec, config) {
        words.push(quote(&argument)?);
    }
    Ok(format!(
        "#!/bin/sh\n# Generated by agentc-supervisor (R-P3b.3); read-only to the reviewer.\n\
         exec {} --chdir \"$PWD\" -- /bin/bash -c \"$1\"\n",
        words.join(" ")
    ))
}

/// Writes `$RUN/candidate-shell` (owner read/execute only) and creates the
/// candidate's private home for a reviewer.
pub fn write(spec: &LaunchSpec, config: &Config) -> Result<()> {
    confine::private_dir(&home(spec)).context("create candidate home")?;
    let path = shell_path(spec);
    confine::generated_file(&path, script(spec, config)?.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
            .context("make candidate shell executable")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reviewer spec under a fixed state directory.
    fn spec(harness: Harness) -> LaunchSpec {
        LaunchSpec {
            role: Role::Reviewer,
            harness,
            clone: "/var/lib/agentc/rev/clones/c1".into(),
            run: "/var/lib/agentc/rev/runs/r1".into(),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: Some("p1".into()),
        }
    }

    #[test]
    fn only_claude_reviewers_use_the_prefix_and_codex_reviewers_are_refused() {
        assert!(applies(&spec(Harness::Claude)));
        assert!(!applies(&spec(Harness::Codex)));
        assert!(unsupported(&spec(Harness::Codex)).is_some());
        assert!(unsupported(&spec(Harness::Claude)).is_none());
        let implementer = LaunchSpec {
            role: Role::Implementer,
            ..spec(Harness::Codex)
        };
        assert!(!applies(&implementer) && unsupported(&implementer).is_none());
    }

    #[test]
    fn state_is_hidden_before_the_launch_paths_return_and_the_login_is_masked() {
        let args: Vec<String> = inner_args(&spec(Harness::Claude), &Config::default())
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let position = |needle: &str| args.iter().position(|a| a == needle).unwrap();
        let tmpfs = position("--tmpfs");
        assert_eq!(args[tmpfs + 1], "/var/lib/agentc");
        assert!(position("/var/lib/agentc/rev/clones/c1") > tmpfs);
        assert_eq!(
            args[position("/var/lib/agentc/rev/clones/c1") - 1],
            "--ro-bind"
        );
        let mask = position("/dev/null");
        assert_eq!(
            args[mask + 1],
            "/var/lib/agentc/rev/claude-config/.credentials.json"
        );
        assert!(mask > position("/var/lib/agentc/rev/claude-config"));
        assert!(!args.iter().any(|a| a.ends_with("/state/coordinator")));
        assert!(args.contains(&"--disable-userns".to_string()));
        assert!(args.contains(&"TYPESAFE_API_KEY".to_string()));
    }

    #[test]
    fn every_known_credential_variable_stays_unset() {
        // Removing a name must be a deliberate, reviewed change to this list;
        // the real-Bubblewrap test proves each listed name is unset.
        assert_eq!(
            UNSET,
            [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_SHELL_PREFIX",
                "OPENAI_API_KEY",
                "CODEX_API_KEY",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "TYPESAFE_API_KEY",
            ]
        );
    }

    #[test]
    fn script_quotes_paths_and_passes_the_command_as_one_argument() {
        let config = Config {
            state_dir: "/srv/it's state".into(),
            ..Config::default()
        };
        let text = script(&spec(Harness::Claude), &config).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains(r"'/srv/it'\''s state'"));
        assert!(text.ends_with("-- /bin/bash -c \"$1\"\n"));
    }
}
