//! Reviewer candidate-code isolation (R-P3b.3, decision U18).
//!
//! A reviewer builds and tests the implementer's candidate, so every command
//! its Bash tool runs may execute hostile code (`build.rs`, tests, UI
//! scripts). Claude Code passes each Bash command, as one shell string, to
//! the executable named by `CLAUDE_CODE_SHELL_PREFIX`. The supervisor points
//! that at a generated, read-only `$RUN/candidate-shell`, which runs the
//! command in an inner Bubblewrap: the whole state directory is replaced by an
//! empty tmpfs, only this launch's clone (read-only), build/home/Cargo
//! directories (writable) and the token-free `claude-config` (read-only)
//! return, so the role's Claude token file stays hidden, as does coordinator
//! state; credential variables are unset, and no further user namespace can
//! be created. After each command the harness's tracked working directory is
//! reset to the clone, so its own git calls never run in a candidate-writable
//! directory. Codex has no equivalent hook, so Codex reviewer launches are
//! refused until it does.
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

/// The candidate's own temporary directory (its `TMPDIR`). The harness's
/// `$RUN/tmp`, which holds Claude Code's tracked-directory file, is replaced
/// by a throwaway tmpfs inside the inner sandbox, so candidate code (including
/// a concurrent background command) can never rewrite that file.
pub fn temp(spec: &LaunchSpec) -> PathBuf {
    spec.run.join("candidate-tmp")
}

/// Why `spec` cannot isolate candidate code, if it cannot.
pub fn unsupported(spec: &LaunchSpec) -> Option<String> {
    (spec.role == Role::Reviewer && spec.harness == Harness::Codex).then(|| {
        "Codex reviewer launches cannot isolate candidate code from reviewer secrets \
         (R-P3b.3); use a Claude reviewer"
            .into()
    })
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
    // The harness's temp stays out of reach; see `temp`.
    args.extend(["--tmpfs".into(), spec.run.join("tmp").into_os_string()]);
    for writable in [temp(spec), spec.run.join("target"), state.cargo] {
        same(&mut args, "--bind", &writable);
    }
    args.extend(["--bind".into(), home(spec).into(), state.home.into()]);
    same(
        &mut args,
        "--ro-bind-try",
        &spec.run.join(run_files::VERIFICATION),
    );
    // The per-run staging session (decision U22), never the login itself.
    same(
        &mut args,
        "--ro-bind-try",
        &spec.run.join(run_files::VERIFICATION_SESSION),
    );
    // Holds only settings.json and CLAUDE.md (see `preflight`); the token
    // file beside it stays under the tmpfs.
    same(&mut args, "--ro-bind", &claude_config(spec, config));
    for name in UNSET {
        args.extend(["--unsetenv".into(), (*name).into()]);
    }
    args.extend(["--setenv".into(), "TMPDIR".into(), temp(spec).into()]);
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

/// Claude Code ends every Bash command string with this, followed by the
/// path of the file from which it reads the shell's new working directory.
const CWD_MARKER: &str = "pwd -P >| ";

/// The complete prefix script: one command string in `$1`, run by Bash in
/// the inner sandbox from the caller's working directory. Afterwards, when no
/// candidate process remains, the harness's tracked directory is reset to the
/// clone, so a later out-of-prefix `git` never runs in a candidate-writable
/// directory (temp, build, Cargo or candidate home).
pub fn script(spec: &LaunchSpec, config: &Config) -> Result<String> {
    let mut words = vec![quote(&config.bubblewrap.clone().into_os_string())?];
    for argument in inner_args(spec, config) {
        words.push(quote(&argument)?);
    }
    Ok(format!(
        "#!/bin/sh\n# Generated by agentc-supervisor (R-P3b.3); read-only to the reviewer.\n\
         {}{} --chdir \"$PWD\" -- /bin/bash -c \"$1\"\nSTATUS=$?\n\
         printf '%s\\n' \"$CLONE\" > \"$CWD_FILE\" || exit 126\nexit \"$STATUS\"\n",
        cwd_guard(spec)?,
        words.join(" ")
    ))
}

/// Script lines that locate the harness's cwd file from `$1` and refuse, with
/// exit status 126 and before running anything, a command whose cwd file is
/// missing or not a plain file name directly inside the harness's `$RUN/tmp`.
fn cwd_guard(spec: &LaunchSpec) -> Result<String> {
    const GUARD: &str = r#"CLONE=@CLONE@
case "$1" in *@MARKER@*) CWD_FILE=${1##*@MARKER@} ;; *) CWD_FILE= ;; esac
CWD_FILE=${CWD_FILE#\'}
CWD_FILE=${CWD_FILE%\'}
NAME=${CWD_FILE#@TMP@/}
case "$NAME" in "$CWD_FILE"|''|.|..|*[!A-Za-z0-9._-]*)
 echo 'candidate-shell: no harness cwd file in the command; refused' >&2
 exit 126;;
esac
"#;
    Ok(GUARD
        .replace("@CLONE@", &quote(&spec.clone.clone().into_os_string())?)
        .replace("@MARKER@", &quote(&CWD_MARKER.into())?)
        .replace("@TMP@", &quote(&spec.run.join("tmp").into_os_string())?))
}

/// Writes `$RUN/candidate-shell` (owner read/execute only) and creates the
/// candidate's private home and temp for a reviewer.
pub fn write(spec: &LaunchSpec, config: &Config) -> Result<()> {
    confine::private_dir(&home(spec)).context("create candidate home")?;
    confine::private_dir(&temp(spec)).context("create candidate temp")?;
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
            task: None,
            push_socket: None,
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
    fn state_is_hidden_before_the_launch_paths_return_and_the_token_stays_hidden() {
        let config = Config::default();
        let args: Vec<String> = inner_args(&spec(Harness::Claude), &config)
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
        let token = crate::profile::claude_token(Role::Reviewer, &config);
        assert!(token.starts_with(&args[tmpfs + 1]));
        let uncovering = args[tmpfs + 2..]
            .iter()
            .filter(|a| token.starts_with(a.as_str()));
        assert_eq!(uncovering.count(), 0, "a mount re-attaches the token");
        assert!(!args.iter().any(|a| a == "/dev/null"));
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
        assert!(text.contains("-- /bin/bash -c \"$1\"\nSTATUS=$?\n"));
        assert!(text.ends_with("exit \"$STATUS\"\n"));
    }

    #[test]
    fn harness_temp_is_a_tmpfs_and_the_candidate_gets_its_own_temp() {
        let args: Vec<String> = inner_args(&spec(Harness::Claude), &Config::default())
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let run = "/var/lib/agentc/rev/runs/r1";
        let harness_temp = format!("{run}/tmp");
        let at = args.iter().position(|a| *a == harness_temp).unwrap();
        assert_eq!(args[at - 1], "--tmpfs");
        assert!(at > args.iter().position(|a| a == "/var/lib/agentc").unwrap());
        assert_eq!(args.iter().filter(|a| **a == harness_temp).count(), 1);
        let own = format!("{run}/candidate-tmp");
        assert!(args.windows(3).any(|w| w == ["--bind", &own, &own]));
        assert!(args.windows(3).any(|w| w == ["--setenv", "TMPDIR", &own]));
    }
}

/// Runs the generated `candidate-shell` with a stand-in Bubblewrap so the
/// cwd guard and reset are exercised by a real POSIX shell.
#[cfg(all(test, unix))]
mod shell_tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Skips every flag up to `--`, honouring `--chdir`, then runs the rest.
    const FAKE_BWRAP: &str = r#"#!/bin/sh
while [ "$1" != -- ]; do
 if [ "$1" = --chdir ]; then cd "$2" || exit 125; fi
 shift
done
shift
exec "$@"
"#;

    /// A reviewer launch under a temporary root with a generated shell.
    struct Shell {
        root: tempfile::TempDir,
        spec: LaunchSpec,
    }

    impl Shell {
        /// Lays out clone and run directories and writes `candidate-shell`.
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let fake = root.path().join("fake-bwrap");
            write_executable(&fake, FAKE_BWRAP);
            let spec = LaunchSpec {
                clone: root.path().join("clone"),
                run: root.path().join("run"),
                ..tests_spec()
            };
            for dir in [&spec.clone, &spec.run.join("tmp"), &spec.run.join("target")] {
                fs::create_dir_all(dir).unwrap();
            }
            let config = Config {
                bubblewrap: fake,
                ..Config::default()
            };
            write_executable(&shell_path(&spec), &script(&spec, &config).unwrap());
            Self { root, spec }
        }

        /// Runs one command string from the clone; returns its exit code.
        fn run(&self, command: &str) -> i32 {
            Command::new(shell_path(&self.spec))
                .arg(command)
                .current_dir(&self.spec.clone)
                .status()
                .unwrap()
                .code()
                .unwrap()
        }

        /// The harness's tracked-directory file.
        fn cwd_file(&self) -> PathBuf {
            self.spec.run.join("tmp/claude-1a2b-cwd")
        }

        /// A harness-shaped command: `body`, then the tracked-cwd write.
        fn harness(&self, body: &str, quoted: bool) -> String {
            let file = self.cwd_file().display().to_string();
            let file = if quoted { format!("'{file}'") } else { file };
            format!("{body} && pwd -P >| {file}")
        }

        /// The tracked directory the harness would read back.
        fn tracked(&self) -> String {
            fs::read_to_string(self.cwd_file())
                .unwrap()
                .trim_end()
                .into()
        }
    }

    /// A Claude reviewer spec with placeholder paths.
    fn tests_spec() -> LaunchSpec {
        LaunchSpec {
            role: Role::Reviewer,
            harness: Harness::Claude,
            clone: PathBuf::new(),
            run: PathBuf::new(),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: None,
            task: None,
            push_socket: None,
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_command_that_moves_the_cwd_leaves_the_harness_in_the_clone() {
        let shell = Shell::new();
        let target = shell.spec.run.join("target");
        for quoted in [false, true] {
            let command = shell.harness(&format!("cd '{}'", target.display()), quoted);
            assert_eq!(shell.run(&command), 0);
            assert_eq!(shell.tracked(), shell.spec.clone.to_str().unwrap());
        }
    }

    #[test]
    fn a_failing_command_keeps_its_status_and_still_resets_the_cwd() {
        let shell = Shell::new();
        fs::write(shell.cwd_file(), "/elsewhere\n").unwrap();
        assert_eq!(shell.run(&shell.harness("exit 7", false)), 7);
        assert_eq!(shell.tracked(), shell.spec.clone.to_str().unwrap());
    }

    #[test]
    fn commands_without_a_harness_cwd_file_are_refused_before_running() {
        let shell = Shell::new();
        let ran = shell.root.path().join("ran");
        let touch = format!("touch '{}'", ran.display());
        let outside = shell.root.path().join("outside-cwd");
        let tmp = shell.spec.run.join("tmp");
        for command in [
            touch.clone(),
            format!("{touch} && pwd -P >| {}", outside.display()),
            format!("{touch} && pwd -P >| {}/sub/claude-cwd", tmp.display()),
            format!("{touch} && pwd -P >| {}/", tmp.display()),
            format!("{touch} && pwd -P >| {}/..", tmp.display()),
            format!("{touch} && pwd -P >| {} x", shell.cwd_file().display()),
        ] {
            assert_eq!(shell.run(&command), 126, "{command}");
            assert!(!ran.exists(), "{command}");
        }
        assert!(!outside.exists());
    }
}
