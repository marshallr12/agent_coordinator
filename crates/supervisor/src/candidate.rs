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
//!
//! Every call gets its own inner sandbox and PID namespace, so a command
//! started in the background dies when its call returns. The call says so on
//! stderr when anything outlives the command, instead of leaving the reviewer
//! to wait on a log nobody writes. A call that runs past [`CALL_SECONDS`] is
//! stopped with a clear message, and after [`STUCK_CALLS`] such calls in a
//! row the shell refuses further commands and tells the reviewer to return
//! its verdict, so a review that cannot make progress ends in bounded time.
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

/// The longest one candidate command may run before it is stopped.
pub const CALL_SECONDS: u64 = 45 * 60;
/// Consecutive commands stopped at [`CALL_SECONDS`] after which the shell
/// refuses every further command.
pub const STUCK_CALLS: u32 = 2;
/// The exit status of a command stopped at the time limit.
const LIMIT_STATUS: u8 = 252;
/// The status of a command the shell refuses to run.
const REFUSED_STATUS: u8 = 126;
/// The harness temp file holding the count of consecutive stopped commands.
const STUCK_FILE: &str = "candidate-stuck";

/// The knobs of the generated shell; tests shrink them.
struct Tuning {
    call_seconds: u64,
    stuck_calls: u32,
    /// Where the inner shell looks for processes that outlive a command.
    proc_root: String,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            call_seconds: CALL_SECONDS,
            stuck_calls: STUCK_CALLS,
            proc_root: "/proc".into(),
        }
    }
}

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

/// What the inner sandbox's first process runs, with the command string in
/// `$1`: the command under a time limit, then a look at what is still alive.
/// The sandbox has its own PID namespace, so apart from this shell and the
/// Bubblewrap init (pid 1) any live process was started by the command and
/// is about to be killed with the call.
const INNER: &str = r#"timeout -k 10 @LIMIT@ /bin/bash -c "$1"
STATUS=$?
if [ "$STATUS" -ge 124 ] && [ "$SECONDS" -ge @LIMIT@ ]; then
 echo "candidate-shell: the command ran past the time limit of @LIMIT@ seconds and was stopped" >&2
 exit @LIMIT_STATUS@
fi
LEFT=
for DIR in @PROC@/[0-9]*; do
 PID=${DIR##*/}
 case $PID in 1|$$) continue;; esac
 read -r STAT 2>/dev/null < "$DIR/stat" || continue
 case ${STAT##*) } in Z*) continue;; esac
 LEFT="$LEFT $PID"
done
if [ -n "$LEFT" ]; then
 echo "candidate-shell: warning: process(es)$LEFT were still running when the command ended." >&2
 echo "Background jobs do not survive a candidate-shell call: they are killed now and nothing they write will appear. Run long commands in the foreground, with their output redirected into \$TMPDIR." >&2
fi
exit "$STATUS"
"#;

/// The complete prefix script: one command string in `$1`, run by Bash in
/// the inner sandbox from the caller's working directory. Afterwards, when no
/// candidate process remains, the harness's tracked directory is reset to the
/// clone, so a later out-of-prefix `git` never runs in a candidate-writable
/// directory (temp, build, Cargo or candidate home).
pub fn script(spec: &LaunchSpec, config: &Config) -> Result<String> {
    script_with(spec, config, &Tuning::default())
}

fn script_with(spec: &LaunchSpec, config: &Config, tuning: &Tuning) -> Result<String> {
    let mut words = vec![quote(&config.bubblewrap.clone().into_os_string())?];
    for argument in inner_args(spec, config) {
        words.push(quote(&argument)?);
    }
    let inner = INNER
        .replace("@LIMIT_STATUS@", &LIMIT_STATUS.to_string())
        .replace("@LIMIT@", &tuning.call_seconds.to_string())
        .replace("@PROC@", &tuning.proc_root);
    Ok(format!(
        "#!/bin/sh\n# Generated by agentc-supervisor (R-P3b.3); read-only to the reviewer.\n\
         {}{}{} --chdir \"$PWD\" -- /bin/bash -c {} candidate-shell \"$1\"\nSTATUS=$?\n\
         {}printf '%s\\n' \"$CLONE\" > \"$CWD_FILE\" || exit 126\nexit \"$STATUS\"\n",
        cwd_guard(spec)?,
        stuck_guard(spec, tuning)?,
        words.join(" "),
        quote(&inner.into())?,
        stuck_count(),
    ))
}

/// Script lines that refuse a command once [`STUCK_CALLS`] commands in a row
/// were stopped at the time limit, the review having made no progress.
fn stuck_guard(spec: &LaunchSpec, tuning: &Tuning) -> Result<String> {
    const GUARD: &str = r#"STUCK=@STUCK@
COUNT=0
[ -r "$STUCK" ] && read -r COUNT < "$STUCK"
case "$COUNT" in ''|*[!0-9]*) COUNT=0;; esac
if [ "$COUNT" -ge @CALLS@ ]; then
 echo 'candidate-shell: refused: the last @CALLS@ commands each ran past the time limit of @SECONDS@ seconds and were stopped. This review cannot make progress; run no more commands, and return your verdict now from what you have (request_changes, naming the commands that could not finish).' >&2
 printf '%s\n' "$CLONE" > "$CWD_FILE"
 exit @REFUSED@
fi
"#;
    let file = spec.run.join("tmp").join(STUCK_FILE);
    Ok(GUARD
        .replace("@STUCK@", &quote(&file.into_os_string())?)
        .replace("@CALLS@", &tuning.stuck_calls.to_string())
        .replace("@SECONDS@", &tuning.call_seconds.to_string())
        .replace("@REFUSED@", &REFUSED_STATUS.to_string()))
}

/// Script lines that count a command stopped at the limit, and reset the
/// count on any other.
fn stuck_count() -> String {
    format!(
        "if [ \"$STATUS\" = {LIMIT_STATUS} ]; then COUNT=$((COUNT + 1)); else COUNT=0; fi\n\
         printf '%s\\n' \"$COUNT\" > \"$STUCK\" 2>/dev/null\n"
    )
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
        assert!(text.contains("candidate-shell \"$1\"\nSTATUS=$?\n"));
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
            Self::tuned(|_| Tuning::default())
        }

        /// As [`Shell::new`], with the generated shell tuned by `tuning`,
        /// which is given the launch's temporary root.
        fn tuned(tuning: impl FnOnce(&Path) -> Tuning) -> Self {
            let root = tempfile::tempdir().unwrap();
            let fake = root.path().join("fake-bwrap");
            write_executable(&fake, FAKE_BWRAP);
            // The fake is exec'd by the generated shell, which cannot retry.
            let probe = status_retrying(|| {
                let mut probe = Command::new(&fake);
                probe.arg("--");
                probe
            });
            assert!(probe.success());
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
            let tuning = tuning(root.path());
            let text = script_with(&spec, &config, &tuning).unwrap();
            write_executable(&shell_path(&spec), &text);
            Self { root, spec }
        }

        /// Runs one command string from the clone; returns its exit code.
        fn run(&self, command: &str) -> i32 {
            status_retrying(|| {
                let mut shell = Command::new(shell_path(&self.spec));
                shell.arg(command).current_dir(&self.spec.clone);
                shell
            })
            .code()
            .unwrap()
        }

        /// Runs one harness-shaped command; returns its exit code and stderr.
        fn run_stderr(&self, body: &str) -> (i32, String) {
            let command = self.harness(body, false);
            let mut shell = Command::new(shell_path(&self.spec));
            shell.arg(command).current_dir(&self.spec.clone);
            let output = retry_output(shell);
            let text = String::from_utf8_lossy(&output.stderr).into_owned();
            (output.status.code().unwrap(), text)
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

    /// Runs a command to completion, retrying while exec reports `ETXTBSY`.
    ///
    /// A script written moments ago can be briefly unexecutable: another test
    /// thread may fork while the write descriptor is open, and that child
    /// holds it until its own exec. Renaming into place does not help, since
    /// the busy inode is the same. The window is a few milliseconds, so retry
    /// with a short bounded backoff instead of serializing the tests.
    fn status_retrying(mut command: impl FnMut() -> Command) -> std::process::ExitStatus {
        let mut delay = std::time::Duration::from_millis(1);
        for _ in 0..12 {
            match command().status() {
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                other => return other.unwrap(),
            }
        }
        command().status().unwrap()
    }

    /// As [`status_retrying`], capturing the output.
    fn retry_output(mut command: Command) -> std::process::Output {
        let mut delay = std::time::Duration::from_millis(1);
        for _ in 0..12 {
            match command.output() {
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                other => return other.unwrap(),
            }
        }
        command.output().unwrap()
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

    /// A shell whose process scan reads a stand-in `/proc` holding one entry
    /// per `(pid, state)`; the fake Bubblewrap has no PID namespace, so the
    /// real `/proc` cannot be scanned.
    fn scanning(entries: &[(u32, &str)]) -> Shell {
        let shell = Shell::tuned(|root| Tuning {
            proc_root: root.join("proc").display().to_string(),
            ..Tuning::default()
        });
        for (pid, state) in entries {
            let dir = shell.root.path().join("proc").join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            // The command name may hold spaces and parentheses.
            fs::write(dir.join("stat"), format!("{pid} (a b) c) {state} 1 1\n")).unwrap();
        }
        shell
    }

    #[test]
    fn a_command_that_leaves_processes_running_is_told_they_do_not_survive() {
        let shell = scanning(&[(1, "S"), (7, "S"), (8, "Z"), (9, "R")]);
        let (code, stderr) = shell.run_stderr("true");
        assert_eq!(code, 0);
        assert!(stderr.contains("process(es) 7 9 were"), "{stderr}");
        assert!(
            stderr.contains("Background jobs do not survive a candidate-shell call"),
            "{stderr}"
        );
        assert!(stderr.contains("in the foreground"), "{stderr}");
        assert_eq!(shell.tracked(), shell.spec.clone.to_str().unwrap());
    }

    #[test]
    fn a_command_that_leaves_nothing_running_gets_no_warning_and_keeps_its_status() {
        let shell = scanning(&[(1, "S"), (8, "Z")]);
        assert_eq!(shell.run_stderr("exit 3"), (3, String::new()));
    }

    #[test]
    fn a_command_past_the_time_limit_is_stopped_with_a_clear_message() {
        let shell = Shell::tuned(|_| Tuning {
            call_seconds: 1,
            ..Tuning::default()
        });
        let started = std::time::Instant::now();
        let (code, stderr) = shell.run_stderr("sleep 30");
        assert_eq!(code, i32::from(LIMIT_STATUS));
        assert!(
            stderr.contains("ran past the time limit of 1 seconds"),
            "{stderr}"
        );
        assert!(started.elapsed().as_secs() < 20);
        assert_eq!(shell.tracked(), shell.spec.clone.to_str().unwrap());
    }

    #[test]
    fn a_command_that_exits_124_by_itself_is_not_counted_as_stopped() {
        let shell = Shell::tuned(|_| Tuning {
            // This tests an explicit exit status, not a deadline. A one-second
            // budget can trip Bash's integer-second accounting even for a
            // fast command; the dedicated timeout tests keep their short limit.
            call_seconds: 30,
            stuck_calls: 1,
            ..Tuning::default()
        });
        for _ in 0..2 {
            assert_eq!(shell.run_stderr("exit 124").0, 124);
        }
    }

    #[test]
    fn repeated_stopped_commands_make_the_shell_refuse_the_rest_of_the_review() {
        let shell = Shell::tuned(|_| Tuning {
            call_seconds: 1,
            stuck_calls: 2,
            ..Tuning::default()
        });
        let ran = shell.root.path().join("ran");
        let touch = format!("touch '{}'", ran.display());
        let stopped = i32::from(LIMIT_STATUS);
        assert_eq!(shell.run_stderr("sleep 30").0, stopped);
        // A command that finishes in time resets the count.
        assert_eq!(shell.run_stderr("true").0, 0);
        assert_eq!(shell.run_stderr("sleep 30").0, stopped);
        assert_eq!(shell.run_stderr("sleep 30").0, stopped);
        let (code, stderr) = shell.run_stderr(&touch);
        assert_eq!(code, i32::from(REFUSED_STATUS));
        assert!(!ran.exists());
        assert!(stderr.contains("refused"), "{stderr}");
        assert!(stderr.contains("return your verdict now"), "{stderr}");
        assert_eq!(shell.tracked(), shell.spec.clone.to_str().unwrap());
    }
}
