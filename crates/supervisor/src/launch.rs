//! Prepares a run directory and spawns one supervised launch.
use crate::candidate;
use crate::config::Config;
use crate::confine::{self, RunState};
use crate::preflight;
use crate::profile::{self, LaunchCommand, LaunchSpec, Role, run_files};
use crate::role_settings;
use crate::sandbox;
use crate::staging_login;
use crate::verification;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::process::{Command, Stdio};

/// Writes the generated files a launch needs into `$RUN` (the prompt is the
/// caller's) and creates its private temp and build directories.
pub fn prepare_run(spec: &LaunchSpec, config: &Config) -> Result<()> {
    prepare(spec, config).map(|_| ())
}

/// Retain the lifecycle lock while writing generated files or running a harness.
fn prepare(spec: &LaunchSpec, config: &Config) -> Result<RunState> {
    let state = RunState::prepare(spec, config)?;
    for dir in ["tmp", "target"] {
        confine::private_dir(&spec.run.join(dir)).context("create run directories")?;
    }
    confine::generated_file(
        &spec.run.join(run_files::SETTINGS),
        role_settings::render(spec.role).as_bytes(),
    )?;
    if spec.role == Role::Reviewer {
        let schema = serde_json::to_string_pretty(&profile::review_schema())?;
        confine::generated_file(&spec.run.join(run_files::SCHEMA), schema.as_bytes())?;
    }
    if candidate::applies(spec) {
        candidate::write(spec, config).context("write candidate shell")?;
    }
    if let Some(described) = verification::describe(spec, config) {
        let text = serde_json::to_string_pretty(&described)?;
        confine::generated_file(&spec.run.join(run_files::VERIFICATION), text.as_bytes())?;
    }
    Ok(state)
}

/// Prepares, preflights and runs a launch; returns the harness exit code.
/// Events go to `$RUN/events.jsonl`, diagnostics to `$RUN/stderr.log`.
/// `after_harness` runs once the harness has exited (or failed to start) and
/// before the run is marked terminal, so leftovers die while it is locked.
pub fn run(spec: &LaunchSpec, config: &Config, after_harness: impl FnOnce()) -> Result<i32> {
    let state = prepare(spec, config)?;
    let problems = preflight::check(spec, config);
    if !problems.is_empty() {
        bail!("preflight refused the launch:\n- {}", problems.join("\n- "));
    }
    let command = sandbox::wrap(profile::command(spec, config), spec, config)?;
    if spec.harness == profile::Harness::Claude {
        crate::relay::start_host(spec, config).context("start loopback relays")?;
    }
    crate::setup::run(spec, config).context("project setup")?;
    run_harness(&state, &command, spec, config, after_harness)
}

/// Marks the run started, runs the harness, runs `after_harness` once it has
/// exited (or failed to start) and only then marks the run terminal.
fn run_harness(
    state: &RunState,
    command: &LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
    after_harness: impl FnOnce(),
) -> Result<i32> {
    state.started(spec)?;
    let result = run_signed_in(command, spec, config);
    after_harness();
    state.terminal(spec)?;
    Ok(result?.code().unwrap_or(-1))
}

/// Runs the harness inside a verifying reviewer's staging session (none for
/// other launches), signing it out whether or not the harness ran.
fn run_signed_in(
    command: &LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
) -> Result<std::process::ExitStatus> {
    let session = staging_login::open(spec, config)?;
    let status =
        spawn(command, spec, config).and_then(|mut child| child.wait().context("wait for harness"));
    close_session(session);
    status
}

/// Signs a launch's staging session out; a failure only warns, since the
/// coordinator's session lifetime still bounds the handed-over cookie.
fn close_session(session: Option<staging_login::StagingSession>) {
    if let Some(Err(error)) = session.map(staging_login::StagingSession::close) {
        eprintln!("agentc-supervisor: warning: {error:#}");
    }
}

/// Spawns the harness with exactly the profile's environment and
/// `no_new_privs`. A Claude harness also gets the role's token as
/// [`profile::CLAUDE_TOKEN_ENV`], read here so it never enters `command`.
pub fn spawn(
    command: &LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
) -> Result<std::process::Child> {
    let token = harness_token(spec, config)?;
    let prompt = prompt_input(command, spec)?;
    let stdout = new_log(&spec.run.join("events.jsonl"))?;
    let stderr = new_log(&spec.run.join("stderr.log"))?;
    let mut process = Command::new(&command.program);
    if spec.harness == profile::Harness::Claude {
        sandbox::fence_descriptors(&mut process)?;
    }
    crate::reaper::forbid_new_privileges(&mut process);
    process
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(k, v)| (k, v)));
    if let Some(token) = token {
        process.env(profile::CLAUDE_TOKEN_ENV, token);
    }
    process
        .current_dir(&command.cwd)
        .stdin(prompt)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("spawn {}", command.program.display()))
}

/// The prompt file as the harness's stdin; Claude gets a sealed snapshot.
fn prompt_input(command: &LaunchCommand, spec: &LaunchSpec) -> Result<File> {
    confine::regular_file(&command.stdin).context("inspect prompt")?;
    let prompt = File::open(&command.stdin).context("open prompt")?;
    if spec.harness == profile::Harness::Claude {
        sandbox::sealed_prompt(prompt)
    } else {
        Ok(prompt)
    }
}

/// The longest Claude token accepted, in bytes.
const MAX_TOKEN_BYTES: usize = 4096;

/// The role's Claude token for a Claude launch, `None` for other harnesses.
/// Errors name the file, never its contents.
fn harness_token(spec: &LaunchSpec, config: &Config) -> Result<Option<String>> {
    if spec.harness != profile::Harness::Claude {
        return Ok(None);
    }
    role_token(spec.role, config).map(Some)
}

/// The role's Claude token, read and checked as a launch reads it (the live
/// loop's sign-in check uses it too). Errors name the file, never its contents.
pub fn role_token(role: Role, config: &Config) -> Result<String> {
    let path = profile::claude_token(role, config);
    let bytes = read_token(&path).with_context(|| format!("read {}", path.display()))?;
    valid_token(&bytes).with_context(|| format!("Claude token {}", path.display()))
}

/// The most bytes a token file may hold: the token and up to two trailing
/// bytes (`\r\n`) for `valid_token` to trim.
const MAX_TOKEN_FILE_BYTES: u64 = MAX_TOKEN_BYTES as u64 + 2;

/// The token file's bytes: its path holds no symlink component (see
/// `confine::path_without_symlinks`), `open_token` vets the opened file, and
/// `read_bounded` refuses more than [`MAX_TOKEN_FILE_BYTES`].
fn read_token(path: &std::path::Path) -> Result<Vec<u8>> {
    confine::path_without_symlinks(path, false)?;
    read_bounded(open_token(path)?, MAX_TOKEN_FILE_BYTES)
}

/// Opens `path` without following a final symlink (`O_NOFOLLOW`) or waiting
/// on a FIFO (`O_NONBLOCK`), then requires the opened file itself to be a
/// regular, single-link file of at most [`MAX_TOKEN_FILE_BYTES`].
#[cfg(unix)]
fn open_token(path: &std::path::Path) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "{} must be a regular, single-link file",
        path.display()
    );
    ensure!(
        metadata.len() <= MAX_TOKEN_FILE_BYTES,
        "{} is longer than {MAX_TOKEN_FILE_BYTES} bytes",
        path.display()
    );
    Ok(file)
}

#[cfg(not(unix))]
fn open_token(_path: &std::path::Path) -> Result<File> {
    bail!("the Claude token file requires Unix file checks")
}

/// Reads `source` to its end, consuming at most one byte past `limit`, and
/// refuses it if it holds more than `limit` bytes.
fn read_bounded(source: impl std::io::Read, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    source.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "the token file is longer than {limit} bytes"
    );
    Ok(bytes)
}

/// `bytes` without trailing whitespace, if what remains is non-empty, at most
/// [`MAX_TOKEN_BYTES`] long and only printable, non-space ASCII. The error
/// messages are fixed text, so they never carry token bytes.
fn valid_token(bytes: &[u8]) -> Result<String> {
    let token = bytes.trim_ascii_end();
    ensure!(!token.is_empty(), "the token is empty");
    ensure!(
        token.len() <= MAX_TOKEN_BYTES,
        "the token is longer than {MAX_TOKEN_BYTES} bytes"
    );
    ensure!(
        token.iter().all(u8::is_ascii_graphic),
        "the token holds whitespace, control or non-ASCII bytes"
    );
    Ok(token.iter().map(|&byte| char::from(byte)).collect())
}

/// A pre-existing output, including a planted link, is never truncated.
pub fn new_log(path: &std::path::Path) -> Result<File> {
    confine::path_without_symlinks(path, true)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// The launch as JSON, for `--dry-run` and audit logs.
pub fn describe(command: &LaunchCommand) -> Value {
    let lossy = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
    json!({
        "program": command.program,
        "args": command.args.iter().map(|a| lossy(a)).collect::<Vec<_>>(),
        "env": command.env.iter().map(|(k, v)| format!("{k}={}", lossy(v))).collect::<Vec<_>>(),
        "cwd": command.cwd,
        "stdin": command.stdin,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn launch_logs_are_private_and_never_truncate_existing_files_or_links() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("events.jsonl");
        drop(new_log(&output).unwrap());
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(new_log(&output).is_err());
        let sentinel = root.path().join("sentinel");
        fs::write(&sentinel, "untouched").unwrap();
        let symlinked = root.path().join("stderr.log");
        symlink(&sentinel, &symlinked).unwrap();
        assert!(new_log(&symlinked).is_err());
        let hardlinked = root.path().join("hardlink");
        fs::hard_link(&sentinel, &hardlinked).unwrap();
        assert!(new_log(&hardlinked).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
    }

    /// A Codex reviewer run in a fresh state directory, with its prompt.
    fn codex_run(root: &std::path::Path) -> (Config, LaunchSpec, RunState) {
        prepared_run(root, Role::Reviewer, profile::Harness::Codex)
    }

    /// A prepared run of `role` and `harness` in a fresh state directory,
    /// with its prompt.
    fn prepared_run(
        root: &std::path::Path,
        role: Role,
        harness: profile::Harness,
    ) -> (Config, LaunchSpec, RunState) {
        let config = Config {
            state_dir: root.join("roles"),
            cargo_config_seed: root.join("cargo-seed.toml"),
            ..Config::default()
        };
        fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
        let base = config.state_dir.join(role.slug());
        let spec = LaunchSpec {
            role,
            harness,
            clone: base.join("clones/current"),
            run: base.join("runs/current"),
            model: "mock".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::new_v4(),
            project: None,
            task: None,
            push_socket: None,
        };
        fs::create_dir_all(&spec.clone).unwrap();
        let state = prepare(&spec, &config).unwrap();
        fs::write(spec.run.join(run_files::PROMPT), "prompt").unwrap();
        (config, spec, state)
    }

    /// A `/bin/sh -c script` launch in `spec`'s clone with the profile's
    /// environment.
    fn shell(spec: &LaunchSpec, config: &Config, script: &str) -> LaunchCommand {
        LaunchCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            ..profile::command(spec, config)
        }
    }

    /// Waits for `child` and returns its exit code and `events.jsonl`.
    fn finished(mut child: std::process::Child, spec: &LaunchSpec) -> (Option<i32>, String) {
        let code = child.wait().unwrap().code();
        let events = fs::read_to_string(spec.run.join("events.jsonl")).unwrap();
        (code, events)
    }

    #[test]
    fn a_claude_harness_receives_the_token_that_describe_never_shows() {
        let root = tempfile::tempdir().unwrap();
        let (config, spec, _state) =
            prepared_run(root.path(), Role::Implementer, profile::Harness::Claude);
        let token = "sk-ant-oat01-Fixture_Token-0123";
        fs::write(
            profile::claude_token(spec.role, &config),
            format!("{token}\n"),
        )
        .unwrap();
        let script = format!(
            "test \"$CLAUDE_CODE_OAUTH_TOKEN\" = '{token}' && printf %s ${{#CLAUDE_CODE_OAUTH_TOKEN}}"
        );
        let command = shell(&spec, &config, &script);
        let shown = describe(&profile::command(&spec, &config)).to_string();
        let env = describe(&command)["env"].to_string();
        for text in [&shown, &env] {
            assert!(!text.contains(profile::CLAUDE_TOKEN_ENV), "{text}");
            assert!(!text.contains(token), "describe shows the token");
        }
        let (code, events) = finished(spawn(&command, &spec, &config).unwrap(), &spec);
        assert_eq!((code, events), (Some(0), token.len().to_string()));
    }

    #[test]
    fn a_codex_harness_gets_no_claude_token() {
        let root = tempfile::tempdir().unwrap();
        let (config, spec, _state) = codex_run(root.path());
        fs::write(profile::claude_token(spec.role, &config), "unused-token").unwrap();
        let script = "if printenv CLAUDE_CODE_OAUTH_TOKEN; then exit 3; fi";
        let command = shell(&spec, &config, script);
        let (code, events) = finished(spawn(&command, &spec, &config).unwrap(), &spec);
        assert_eq!((code, events.as_str()), (Some(0), ""));
    }

    /// Plants a token file at the given path.
    type Plant = Box<dyn Fn(&std::path::Path)>;

    /// Ways to plant an unusable token file at a path, each with the error
    /// text it must cause.
    fn bad_tokens() -> Vec<(&'static str, Plant)> {
        let write = |text: &'static [u8]| -> Plant {
            Box::new(move |path: &std::path::Path| fs::write(path, text).unwrap())
        };
        let linked = |hard: bool| -> Plant {
            Box::new(move |path: &std::path::Path| {
                let real = path.with_file_name("real-token");
                fs::write(&real, "Secret-Sentinel-ok").unwrap();
                match hard {
                    true => fs::hard_link(&real, path).unwrap(),
                    false => symlink(&real, path).unwrap(),
                }
            })
        };
        let oversized = vec![b'S'; MAX_TOKEN_BYTES + 1].leak();
        vec![
            ("No such file", Box::new(|_: &std::path::Path| {})),
            ("empty", write(b"")),
            ("empty", write(b"\n \t\n")),
            ("whitespace", write(b"Secret-Sentinel more\n")),
            ("whitespace", write(b"Secret-Sentinel\tmore")),
            ("control", write(b"Secret-Sentinel\x07")),
            ("non-ASCII", write("Secret-Sentinel\u{e9}".as_bytes())),
            ("longer than 4096", write(oversized)),
            ("symlink", linked(false)),
            ("single-link", linked(true)),
            (
                "regular",
                Box::new(|path: &std::path::Path| fs::create_dir(path).unwrap()),
            ),
        ]
    }

    #[test]
    fn an_unusable_token_file_refuses_the_spawn_without_revealing_it() {
        for (expected, plant) in bad_tokens() {
            let root = tempfile::tempdir().unwrap();
            let (config, spec, _state) =
                prepared_run(root.path(), Role::Reviewer, profile::Harness::Claude);
            plant(&profile::claude_token(spec.role, &config));
            let command = shell(&spec, &config, "exit 0");
            let error = format!("{:#}", spawn(&command, &spec, &config).unwrap_err());
            assert!(error.contains(expected), "{expected}: {error}");
            assert!(
                !error.contains("Secret") && !error.contains("SSSS"),
                "{error}"
            );
            assert!(!spec.run.join("events.jsonl").exists(), "{expected}");
        }
    }

    #[test]
    fn content_past_the_read_limit_refuses_the_token() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("claude-token");
        let mut text = "T".repeat(MAX_TOKEN_BYTES);
        text.push_str("\n\nSECOND-LINE-GARBAGE\n");
        fs::write(&path, text).unwrap();
        let Err(error) = read_token(&path) else {
            panic!("content past the read limit was accepted");
        };
        let error = format!("{error:#}");
        assert!(error.contains("longer than"), "{error}");
        assert!(
            !error.contains("GARBAGE") && !error.contains("TTTT"),
            "{error}"
        );
    }

    #[test]
    fn the_token_read_consumes_at_most_one_byte_past_its_limit() {
        use std::io::Read;
        let limit = MAX_TOKEN_FILE_BYTES;
        let exact = read_bounded(std::io::repeat(b'T').take(limit), limit);
        assert_eq!(exact.unwrap().len() as u64, limit);
        let mut source = std::io::repeat(b'T').take(4 * limit);
        assert!(read_bounded(&mut source, limit).is_err());
        assert_eq!(
            source.limit(),
            3 * limit - 1,
            "the read went past its bound"
        );
    }

    #[test]
    fn opening_the_token_refuses_oversized_files_final_symlinks_fifos_and_directories() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real-token");
        fs::write(&real, "token").unwrap();
        assert!(open_token(&real).is_ok());
        let oversized = root.path().join("oversized");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_TOKEN_FILE_BYTES + 1)
            .unwrap();
        let error = open_token(&oversized).unwrap_err().to_string();
        assert!(error.contains("longer than 4098 bytes"), "{error}");
        let link = root.path().join("link");
        symlink(&real, &link).unwrap();
        let fifo = root.path().join("fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let results =
                [link, fifo, root.path().to_owned()].map(|path| open_token(&path).is_err());
            sender.send(results).unwrap();
        });
        let refused = receiver.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(refused.expect("opening the token blocked"), [true; 3]);
    }

    #[test]
    fn a_token_keeps_up_to_the_limit_and_loses_only_trailing_whitespace() {
        let longest = "T".repeat(MAX_TOKEN_BYTES);
        assert_eq!(valid_token(longest.as_bytes()).unwrap(), longest);
        assert_eq!(valid_token(b"abc~!-_.\r\n").unwrap(), "abc~!-_.");
        assert!(valid_token(b" abc").is_err());
    }

    #[test]
    fn harness_runs_without_new_privileges_and_cleanup_precedes_terminal() {
        let root = tempfile::tempdir().unwrap();
        let (config, spec, state) = codex_run(root.path());
        let command = LaunchCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "grep NoNewPrivs: /proc/self/status".into()],
            env: Vec::new(),
            cwd: spec.clone.clone(),
            stdin: spec.run.join(run_files::PROMPT),
        };
        let terminal = spec.run.join(".state-terminal.json");
        let mut terminal_at_cleanup = None;
        let cleanup = || terminal_at_cleanup = Some(terminal.exists());
        let code = run_harness(&state, &command, &spec, &config, cleanup).unwrap();
        assert_eq!(code, 0);
        assert_eq!(terminal_at_cleanup, Some(false), "cleanup missing or late");
        assert!(terminal.exists());
        let events = fs::read_to_string(spec.run.join("events.jsonl")).unwrap();
        assert_eq!(events.split_whitespace().nth(1), Some("1"), "{events}");
    }
}
