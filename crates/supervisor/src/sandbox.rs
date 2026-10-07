//! Claude's OS write boundary. The whole host and run start read-only;
//! individual writable mount roots cannot be renamed around seed overlays.
//! Each launch has its own network namespace; see `relay` for the bridge.
use crate::config::Config;
use crate::confine::{self, StatePaths};
use crate::profile::{Harness, LaunchCommand, LaunchSpec, Role, run_files};
use anyhow::{Context, Result, ensure};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Required flags: unsupported kernels or Bubblewrap versions fail closed.
/// No user/network namespace fallback or direct-harness retry is permitted.
/// Reviewers keep user namespaces and a writable `/proc` (the nested uid map
/// is written there) so `candidate-shell` can start its own sandbox, which
/// forbids further nesting and remounts its `/proc` read-only (R-P3b.3).
fn base_args(role: Role) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--unshare-user".into()];
    if role == Role::Implementer {
        args.extend(["--disable-userns".into(), "--assert-userns-disabled".into()]);
    }
    args.extend(shared_args());
    if role == Role::Implementer {
        args.extend(["--remount-ro".into(), "/proc".into()]);
    }
    args
}

/// Flags common to every role's outer sandbox.
fn shared_args() -> Vec<OsString> {
    [
        "--unshare-net",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn writable_directories(spec: &LaunchSpec) -> Vec<PathBuf> {
    let state = StatePaths::new(&spec.run);
    let mut directories = vec![
        state.home,
        state.cargo,
        state.coordinator,
        spec.run.join("tmp"),
        spec.run.join("target"),
    ];
    if spec.role == Role::Implementer {
        directories.push(spec.clone.clone());
    }
    if crate::candidate::applies(spec) {
        // Writable sources for the inner sandbox's private home and temp.
        directories.push(crate::candidate::home(spec));
        directories.push(crate::candidate::temp(spec));
    }
    directories
}

fn claude_directory(spec: &LaunchSpec, config: &Config) -> PathBuf {
    config
        .state_dir
        .join(spec.role.slug())
        .join("claude-config")
}

/// The supervisor signs in with the reviewer's staging logins itself
/// (decision U22), so the harness never needs them: an empty tmpfs hides them.
fn hide_verification_logins(args: &mut Vec<OsString>, spec: &LaunchSpec, config: &Config) {
    let logins = config
        .state_dir
        .join(Role::Reviewer.slug())
        .join("verification");
    if spec.role == Role::Reviewer && logins.is_dir() {
        args.extend([OsString::from("--tmpfs"), logins.into()]);
    }
}

/// An implementer's candidate-push helper socket stays reachable, read-only,
/// and no other launch's is: an empty tmpfs replaces `<state_dir>/push`, then
/// only this launch's socket directory is bound back at the same path (Bubblewrap
/// creates its mount point in the tmpfs), so the harness can connect to its own
/// socket but never replace it.
fn bind_push_socket(args: &mut Vec<OsString>, spec: &LaunchSpec, config: &Config) {
    if let Some(directory) = spec.push_socket.as_deref().and_then(Path::parent) {
        let root = crate::push_helper_root(config);
        args.extend([OsString::from("--tmpfs"), root.into()]);
        mount(args, "--ro-bind", directory, directory);
    }
}

/// Other launches of the role stay invisible: an empty tmpfs replaces the
/// role's `runs/`, then this launch's `$RUN` is bound back read-only before
/// its writable directories are bound over it. Their `.state-started` files
/// name their session ids, which also name their push helpers' directories.
fn hide_other_runs(args: &mut Vec<OsString>, spec: &LaunchSpec, config: &Config) {
    let runs = config.state_dir.join(spec.role.slug()).join("runs");
    args.extend([OsString::from("--tmpfs"), runs.into()]);
    mount(args, "--ro-bind", &spec.run, &spec.run);
}

fn mount(args: &mut Vec<OsString>, option: &str, source: &Path, destination: &Path) {
    args.extend([OsString::from(option), source.into(), destination.into()]);
}

/// Build the exact executable command, also exposed in `launch --dry-run`.
/// Codex's existing native profile and authentication layout are unchanged.
pub fn wrap(
    mut command: LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
) -> Result<LaunchCommand> {
    if spec.harness != Harness::Claude {
        return Ok(command);
    }
    let mut args = base_args(spec.role);
    hide_other_runs(&mut args, spec, config);
    for path in writable_directories(spec) {
        mount(&mut args, "--bind", &path, &path);
    }
    for path in crate::setup::caches(spec, config)? {
        mount(&mut args, "--bind", &path, &path);
    }
    // `claude-config` and the role's token stay under the read-only root.
    let persistent = claude_directory(spec, config);
    let cargo = StatePaths::new(&spec.run).cargo.join("config.toml");
    mount(&mut args, "--ro-bind", &config.cargo_config_seed, &cargo);
    for destination in [
        persistent.join("settings.json"),
        spec.run.join(run_files::SETTINGS),
    ] {
        mount(
            &mut args,
            "--ro-bind",
            &persistent.join("settings.json"),
            &destination,
        );
    }
    let instructions = persistent.join("CLAUDE.md");
    mount(&mut args, "--ro-bind", &instructions, &instructions);
    hide_verification_logins(&mut args, spec, config);
    bind_push_socket(&mut args, spec, config);
    args.extend([
        OsString::from("--chdir"),
        spec.clone.clone().into(),
        OsString::from("--"),
        crate::relay::program(config).into(),
    ]);
    args.extend(crate::relay::namespace_args(spec, config)?);
    args.push(command.program.into());
    args.append(&mut command.args);
    command.program = config.bubblewrap.clone();
    command.args = args;
    Ok(command)
}

/// Fail before launch for unsafe paths/inodes or an unavailable OS boundary.
pub fn check(spec: &LaunchSpec, config: &Config) -> Result<()> {
    if spec.harness != Harness::Claude {
        return Ok(());
    }
    crate::relay::endpoints(spec, config)?;
    trusted_binary(&config.bubblewrap)?;
    confine::protected_executable(&crate::relay::program(config))
        .context("supervisor binary for the namespace relay")?;
    check_mounts(spec, config)?;
    probe(config, spec.role)?;
    probe_relay(spec, config)
}

/// A writable hard link can modify an inode also visible through the read-only
/// root. Do not follow symlinks during this walk: their targets remain subject
/// to the mounted namespace. Special files and cross-device descendants are refused.
#[cfg(unix)]
fn check_tree(root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    confine::path_without_symlinks(root, false)?;
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir(),
        "writable mount {} must be a directory",
        root.display()
    );
    let device = metadata.dev();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            ensure!(
                metadata.dev() == device,
                "nested filesystem in writable mount {}",
                root.display()
            );
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                ensure!(
                    metadata.nlink() == 1,
                    "hard-linked file in writable mount {}",
                    root.display()
                );
            } else {
                ensure!(
                    metadata.is_symlink(),
                    "special file in writable mount {}",
                    root.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_tree(_root: &Path) -> Result<()> {
    anyhow::bail!("Claude write confinement requires Linux")
}

fn check_mounts(spec: &LaunchSpec, config: &Config) -> Result<()> {
    confine::managed_paths(spec, config, false)?;
    ensure!(
        fs::symlink_metadata(&spec.clone)?.is_dir(),
        "clone must be a directory"
    );
    for path in writable_directories(spec) {
        check_tree(&path)?;
    }
    let persistent = claude_directory(spec, config);
    // Validate source and destination mountpoints.
    for path in [
        config.cargo_config_seed.clone(),
        StatePaths::new(&spec.run).cargo.join("config.toml"),
        persistent.join("settings.json"),
        persistent.join("CLAUDE.md"),
        spec.run.join(run_files::SETTINGS),
    ] {
        confine::regular_file(&path)?;
    }
    if crate::candidate::applies(spec) {
        let expected = crate::candidate::script(spec, config)?;
        let actual = confine::read_regular(&crate::candidate::shell_path(spec))?;
        ensure!(
            actual == expected.as_bytes(),
            "candidate-shell differs from the generated script"
        );
    }
    Ok(())
}

fn trusted_binary(path: &Path) -> Result<()> {
    confine::protected_executable(path).context("required Bubblewrap binary")
}

/// Bound the immutable stdin snapshot without ever reporting prompt contents.
#[cfg(target_os = "linux")]
pub const MAX_PROMPT_BYTES: u64 = 16 * 1024 * 1024;

/// A host file on fd 0 would retain its original writable mount reference:
/// /proc/self/fd/0 can reopen it even though its pathname is read-only inside
/// Bubblewrap. Pass only a sealed memory snapshot; the host descriptor is
/// dropped before spawning. No credential files are involved in this copy.
#[cfg(target_os = "linux")]
pub fn sealed_prompt(prompt: fs::File) -> Result<fs::File> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: the static name is NUL-terminated and the flags are documented
    // Linux memfd flags. On success this function exclusively owns the fd.
    let descriptor = unsafe {
        libc::memfd_create(
            c"agentc-prompt".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).context("create sealed prompt snapshot");
    }
    // SAFETY: memfd_create returned a new owned descriptor above.
    let mut snapshot = unsafe { fs::File::from_raw_fd(descriptor) };
    let copied = std::io::copy(&mut prompt.take(MAX_PROMPT_BYTES + 1), &mut snapshot)
        .context("snapshot prompt")?;
    ensure!(
        copied <= MAX_PROMPT_BYTES,
        "Claude prompt exceeds the 16 MiB snapshot limit"
    );
    snapshot
        .seek(SeekFrom::Start(0))
        .context("rewind prompt snapshot")?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: snapshot owns a live memfd, and F_ADD_SEALS takes this integer
    // bitmask. There are no writable mappings that could prevent sealing.
    if unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error()).context("seal prompt snapshot");
    }
    Ok(snapshot)
}

#[cfg(not(target_os = "linux"))]
pub fn sealed_prompt(_prompt: fs::File) -> Result<fs::File> {
    anyhow::bail!("Claude sealed prompt snapshots require Linux")
}

/// Bubblewrap itself preserves arbitrary inherited descriptors. Mark every
/// descriptor above stderr close-on-exec, including inherited directory fds
/// and the supervisor's lifecycle lock. The spawn error pipe remains usable
/// before exec. Only an async-signal-safe syscall runs in the child hook.
#[cfg(target_os = "linux")]
pub fn fence_descriptors(command: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // SAFETY: this closure allocates nothing, acquires no locks, and only calls
    // close_range and reads errno. CLOEXEC leaves Rust's spawn-error fd open
    // until exec, so errors still reach the parent without a broken handshake.
    unsafe {
        command.pre_exec(|| {
            if libc::close_range(3, u32::MAX, libc::CLOSE_RANGE_CLOEXEC as libc::c_int) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn fence_descriptors(_command: &mut Command) -> Result<()> {
    anyhow::bail!("Claude write confinement requires Linux")
}

/// Starts the role's outer sandbox once; for reviewers it must also be able
/// to start the nested, nesting-free candidate sandbox.
fn probe(config: &Config, role: Role) -> Result<()> {
    let mut command = Command::new(&config.bubblewrap);
    command.args(base_args(role)).arg("--");
    if role == Role::Reviewer {
        command.arg(&config.bubblewrap).args([
            "--unshare-user",
            "--disable-userns",
            "--assert-userns-disabled",
            "--ro-bind",
            "/",
            "/",
            "--",
        ]);
    }
    command.arg("/bin/true");
    await_probe(command, "Bubblewrap/kernel confinement")
}

/// Starts this launch's namespace relay once inside the role's sandbox: the
/// installed supervisor must support it and every relayed port must bind.
fn probe_relay(spec: &LaunchSpec, config: &Config) -> Result<()> {
    let mut command = Command::new(&config.bubblewrap);
    command
        .args(base_args(spec.role))
        .arg("--")
        .arg(crate::relay::program(config))
        .args(crate::relay::namespace_args(spec, config)?)
        .arg("/bin/true");
    await_probe(command, "namespace relay")
}

/// Runs a probe with no environment, a sealed empty stdin and fenced
/// descriptors; it must succeed within five seconds.
fn await_probe(mut command: Command, what: &str) -> Result<()> {
    command
        .env_clear()
        .current_dir("/")
        .stdin(sealed_prompt(fs::File::open("/dev/null")?)?)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    fence_descriptors(&mut command)?;
    // Probe exactly as the launch will run: without new privileges.
    crate::reaper::forbid_new_privileges(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("start required {what} probe"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "{what} probe failed; owner setup required"
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().context("stop timed-out probe")?;
    child.wait().context("reap probe")?;
    anyhow::bail!("{what} probe timed out")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::test_support::skip_when_nested;
    use crate::{launch, profile, role_settings};
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use uuid::Uuid;

    struct Fixture {
        _root: tempfile::TempDir,
        config: Config,
        spec: LaunchSpec,
        outside: PathBuf,
        role_home: PathBuf,
        other_run: PathBuf,
        other_clone: PathBuf,
    }

    /// Stands in for `agentc-supervisor netns-relay`: skips the relay
    /// arguments and runs the harness (the real relay has its own tests).
    fn stub_relay(config: &Config) {
        let program = crate::relay::program(config);
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(
            &program,
            "#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nexec \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    impl Fixture {
        fn new(role: Role) -> Self {
            let root = tempfile::tempdir().unwrap();
            let config = Config {
                state_dir: root.path().join("roles"),
                cargo_config_seed: root.path().join("cargo-seed.toml"),
                bin_dir: root.path().join("bin"),
                bubblewrap: crate::test_support::bubblewrap(),
                ..Config::default()
            };
            fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
            stub_relay(&config);
            let base = config.state_dir.join(role.slug());
            let spec = LaunchSpec {
                role,
                harness: Harness::Claude,
                clone: base.join("clones/current"),
                run: base.join("runs/current"),
                model: "mock".into(),
                effort: "low".into(),
                session_id: Uuid::new_v4(),
                project: None,
                task: None,
                push_socket: None,
            };
            fs::create_dir_all(&spec.clone).unwrap();
            fs::write(spec.clone.join("source"), "original").unwrap();
            launch::prepare_run(&spec, &config).unwrap();
            fs::write(spec.run.join(run_files::PROMPT), "mock prompt").unwrap();
            for marker in [".state-lock", ".state-started", ".state-terminal.json"] {
                fs::write(spec.run.join(marker), "host lifecycle").unwrap();
            }
            let persistent = claude_directory(&spec, &config);
            fs::create_dir(&persistent).unwrap();
            fs::write(
                persistent.join("settings.json"),
                role_settings::render(role),
            )
            .unwrap();
            fs::write(persistent.join("CLAUDE.md"), "").unwrap();
            fs::write(
                profile::claude_token(role, &config),
                "dummy-fixture-token\n",
            )
            .unwrap();
            let login = config.state_dir.join("rev/verification/p1.json");
            fs::create_dir_all(login.parent().unwrap()).unwrap();
            fs::write(&login, "operator password").unwrap();
            // These fixtures deliberately remain writable to the test uid.
            // Refusal must come from the actual mount boundary, not DAC.
            let outside = root.path().join("outside");
            let role_home = base.join("home/.profile");
            let other_run = base.join("runs/other/state/home/.profile");
            let other_clone = base.join("clones/other/source");
            for path in [&outside, &role_home, &other_run, &other_clone] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "untouched").unwrap();
            }
            symlink(&outside, spec.clone.join("escape-link")).unwrap();
            Self {
                _root: root,
                config,
                spec,
                outside,
                role_home,
                other_run,
                other_clone,
            }
        }

        fn command(&self, script: &str) -> LaunchCommand {
            let mut inner = profile::command(&self.spec, &self.config);
            inner.program = "/bin/bash".into();
            inner.args = ["-euc", script].into_iter().map(OsString::from).collect();
            for (key, value) in [
                ("RUN", self.spec.run.clone()),
                (
                    "VERIFICATION_LOGIN",
                    self.config.state_dir.join("rev/verification/p1.json"),
                ),
                ("CLONE", self.spec.clone.clone()),
                ("OUTSIDE", self.outside.clone()),
                ("ROLE_HOME", self.role_home.clone()),
                ("OTHER_RUN", self.other_run.clone()),
                ("OTHER_CLONE", self.other_clone.clone()),
                ("SEED", self.config.cargo_config_seed.clone()),
                (
                    "TOKEN_FILE",
                    profile::claude_token(self.spec.role, &self.config),
                ),
            ] {
                inner.env.push((key.into(), value.into()));
            }
            wrap(inner, &self.spec, &self.config).unwrap()
        }

        fn spawn(&self, script: &str) -> std::process::Child {
            trusted_binary(&self.config.bubblewrap).unwrap();
            check_mounts(&self.spec, &self.config).unwrap();
            launch::spawn(&self.command(script), &self.spec, &self.config).unwrap()
        }

        fn successful(&self, script: &str) {
            let mut child = self.spawn(script);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "mock harness failed: {}",
                        fs::read_to_string(self.spec.run.join("stderr.log")).unwrap()
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("mock harness timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Run by the mock reviewer harness: each command through
    /// `candidate-shell` sees no secrets (neither the token file nor a
    /// credential variable), cannot write the clone or the harness's temp,
    /// cannot nest another user namespace, and leaves the harness's tracked
    /// directory in the clone.
    const REVIEWER_CANDIDATE_CHECKS: &str = r#"
deny overwrite "$CLONE/source"
deny cat "$VERIFICATION_LOGIN"
export HARNESS_IPC="$(readlink /proc/self/ns/ipc)" HARNESS_UTS="$(readlink /proc/self/ns/uts)"
export MARKER="agentc-harness-marker-$$"
(exec -a "$MARKER" sleep 30) &
MARKER_PID=$!
printf harness > "$TMPDIR/harness-only"
CHECKS='
set -eu
test "$TMPDIR" = "$RUN/candidate-tmp"
test ! -e "$RUN/tmp/harness-only"
deny() { if "$@" 2>/dev/null; then echo "candidate unexpected success: $*" >&2; exit 41; fi; }
for cmdline in /proc/[0-9]*/cmdline; do
 if tr "\0" " " < "$cmdline" 2>/dev/null | grep -q -- "$MARKER"; then
  echo "candidate unexpected success: sees harness process $cmdline" >&2; exit 43
 fi
done
test "$(awk "\$2 == \"/proc\" { options = \$4 } END { print options }" /proc/self/mounts | cut -d, -f1)" = ro
test ! -e "$TOKEN_FILE"
deny cat "$TOKEN_FILE"
test -e "$CLAUDE_CONFIG_DIR/settings.json"
deny sh -c "printf x >> \"\$CLAUDE_CONFIG_DIR/settings.json\""
deny sh -c "printf x > \"\$CLAUDE_CONFIG_DIR/planted\""
deny sh -c "printf x > \"\$RUN/verification.json\""
test "$(cat "$RUN/verification-session.json")" = session
deny sh -c "printf x > \"\$RUN/verification-session.json\""
test "$(awk "/^CapEff:/ { print \$2 }" /proc/self/status)" = 0000000000000000
test "$(awk "/^CapBnd:/ { print \$2 }" /proc/self/status)" = 0000000000000000
test "$(readlink /proc/self/ns/ipc)" != "$HARNESS_IPC"
test "$(readlink /proc/self/ns/uts)" != "$HARNESS_UTS"
printf "[filter]" > "$HOME/.gitconfig"
printf planted > "$HOME/.profile"
for name in $UNSET_NAMES; do
 if printenv "$name" > /dev/null; then echo "candidate unexpected success: inherits $name" >&2; exit 44; fi
done
deny test -e "$ROLE_HOME"
deny test -e "$OTHER_RUN"
deny test -e "$OTHER_CLONE"
deny test -e "$AGENT_COORDINATOR_HOME"
deny test -e "$RUN/prompt.md"
test "$(cat "$CLONE/source")" = original
deny sh -c "printf x > \"\$CLONE/source\""
deny unshare -Ur true
deny unshare -U true
for path in "$HOME" "$CARGO_HOME/registry" "$TMPDIR" "$CARGO_TARGET_DIR"; do
 printf candidate > "$path/candidate-write"
done
test "$(pwd)" = "$CLONE"
'
# Shaped like Claude Code's command: the candidate moves the tracked cwd.
"$PREFIX" "$CHECKS
cd \"\$CARGO_TARGET_DIR\" && pwd -P >| '$TMPDIR/claude-mock-cwd'" \
 || { echo "candidate checks failed: $?" >&2; kill "$MARKER_PID"; exit 42; }
kill "$MARKER_PID"
test "$(cat "$TMPDIR/claude-mock-cwd")" = "$CLONE"
test "$(cat "$TMPDIR/harness-only")" = harness
test ! -e "$TMPDIR/candidate-write"
test "$(cat "$RUN/candidate-tmp/candidate-write")" = candidate
test "$(cat "$CARGO_TARGET_DIR/candidate-write")" = candidate
test ! -e "$HOME/.gitconfig"
test ! -e "$HOME/.profile"
test ! -e "$HOME/candidate-write"
"#;

    #[test]
    fn real_bubblewrap_refuses_cross_run_writes_and_seed_replacement_for_both_roles() {
        if skip_when_nested(
            "real_bubblewrap_refuses_cross_run_writes_and_seed_replacement_for_both_roles",
        ) {
            return;
        }
        for role in [Role::Implementer, Role::Reviewer] {
            let fixture = Fixture::new(role);
            let mut script = String::from(
                r#"
deny() { if "$@" 2>/dev/null; then echo "unexpected success: $*" >&2; exit 31; fi; }
overwrite() { printf corrupted > "$1"; }
for file in "$OUTSIDE" "$ROLE_HOME" "$OTHER_RUN" "$OTHER_CLONE" \
 "$SEED" "$CARGO_HOME/config.toml" "$CLAUDE_CONFIG_DIR/settings.json" \
 "$CLAUDE_CONFIG_DIR/CLAUDE.md" "$RUN/role-settings.json" "$RUN/prompt.md" \
 "$RUN/.state-lock" "$RUN/.state-started" "$RUN/.state-terminal.json" \
 "$RUN/events.jsonl" "$RUN/stderr.log" "$CLONE/escape-link"; do
 deny overwrite "$file"
done
deny rm "$CARGO_HOME/config.toml"
deny mv "$CARGO_HOME" "$CARGO_HOME-replaced"
deny mv "$CLAUDE_CONFIG_DIR" "$CLAUDE_CONFIG_DIR-replaced"
deny mv "$RUN/state" "$RUN/state-replaced"
deny mv "$RUN/role-settings.json" "$RUN/settings-replaced"
deny touch "$RUN/new-metadata"
for path in "$HOME" "$CARGO_HOME/registry" "$CARGO_HOME/git" \
 "$AGENT_COORDINATOR_HOME" "$TMPDIR" "$CARGO_TARGET_DIR"; do
 printf allowed > "$path/own-write"
done
test "$CLAUDE_CODE_OAUTH_TOKEN" = dummy-fixture-token
for file in "$CLAUDE_CONFIG_DIR/.credentials.json" "$CLAUDE_CONFIG_DIR/planted" "$TOKEN_FILE"; do
 deny overwrite "$file"
done
deny mkdir "$CLAUDE_CONFIG_DIR/.oauth_refresh.lock"
test "$(awk '/^CapEff:/ {print $2}' /proc/self/status)" = 0000000000000000
test "$(awk '/^CapBnd:/ {print $2}' /proc/self/status)" = 0000000000000000
test "$(awk '/^NoNewPrivs:/ {print $2}' /proc/self/status)" = 1
printf output-through-stdout
printf output-through-stderr >&2
"#,
            );
            match role {
                Role::Implementer => script.push_str(
                    r#"
printf allowed > "$CLONE/source"
deny ln "$OUTSIDE" "$CLONE/new-hardlink"
deny unshare -Ur true
"#,
                ),
                Role::Reviewer => {
                    script.push_str("PREFIX=\"$CLAUDE_CODE_SHELL_PREFIX\"\n");
                    // Every name the candidate must not inherit is set first.
                    for name in crate::candidate::UNSET {
                        script.push_str(&format!("export {name}=outer-secret\n"));
                    }
                    let names = crate::candidate::UNSET.join(" ");
                    script.push_str(&format!("export UNSET_NAMES=\"{names}\"\n"));
                    script.push_str(REVIEWER_CANDIDATE_CHECKS);
                }
            }
            if role == Role::Reviewer {
                // A verifying reviewer's description, read-only to candidates.
                fs::write(fixture.spec.run.join(run_files::VERIFICATION), "{}").unwrap();
                let session = fixture.spec.run.join(run_files::VERIFICATION_SESSION);
                fs::write(session, "session").unwrap();
            }
            fixture.successful(&script);
            for path in [
                &fixture.outside,
                &fixture.role_home,
                &fixture.other_run,
                &fixture.other_clone,
            ] {
                assert_eq!(fs::read_to_string(path).unwrap(), "untouched");
            }
            assert_eq!(
                fs::read(&fixture.config.cargo_config_seed).unwrap(),
                confine::CARGO_CONFIG_SEED
            );
            assert_eq!(
                fs::read(StatePaths::new(&fixture.spec.run).cargo.join("config.toml")).unwrap(),
                confine::CARGO_CONFIG_SEED
            );
            assert_eq!(
                fs::read_to_string(fixture.spec.run.join("events.jsonl")).unwrap(),
                "output-through-stdout"
            );
        }
    }

    #[test]
    fn a_push_socket_adds_only_its_masked_push_root_directory_and_variable() {
        let mut fixture = Fixture::new(Role::Implementer);
        let plain = fixture.command("true");
        let root = crate::push_helper_root(&fixture.config);
        let socket = root.join("l1/sock/push.sock");
        fixture.spec.push_socket = Some(socket.clone());
        let pushed = fixture.command("true");
        let directory = socket.parent().unwrap().as_os_str();
        let mounts = [
            OsString::from("--tmpfs"),
            root.into(),
            OsString::from("--ro-bind"),
            directory.into(),
            directory.into(),
        ];
        let at = pushed.args.windows(5).position(|window| window == mounts);
        let at = at.expect("the push root is not masked right before the socket bind");
        let mut args = pushed.args.clone();
        args.drain(at..at + 5);
        assert_eq!(args, plain.args, "the push socket changed other arguments");
        let mut env = pushed.env.clone();
        let index = env
            .iter()
            .position(|(name, _)| name == profile::PUSH_SOCKET_ENV)
            .expect("the harness is not told the socket");
        assert_eq!(env.remove(index).1, socket.as_os_str());
        assert_eq!(env, plain.env, "the push socket changed other variables");
    }

    #[test]
    fn other_runs_are_masked_before_this_runs_mounts() {
        for role in [Role::Implementer, Role::Reviewer] {
            let fixture = Fixture::new(role);
            let args = fixture.command("true").args;
            let runs = fixture.config.state_dir.join(role.slug()).join("runs");
            let run = fixture.spec.run.as_os_str();
            let mask = [
                OsString::from("--tmpfs"),
                runs.into(),
                OsString::from("--ro-bind"),
                run.into(),
                run.into(),
            ];
            let at = args.windows(5).position(|window| window == mask);
            let at = at.expect("other runs are not masked");
            let first_run_mount = args
                .iter()
                .position(|arg| Path::new(arg).starts_with(&fixture.spec.run) && arg != run);
            assert!(
                first_run_mount.unwrap() > at + 4,
                "a run mount precedes the mask"
            );
        }
    }

    #[test]
    fn an_implementer_reaches_its_push_socket_read_only_inside_the_sandbox() {
        if skip_when_nested("an_implementer_reaches_its_push_socket_read_only_inside_the_sandbox") {
            return;
        }
        let mut fixture = Fixture::new(Role::Implementer);
        let directory = fixture.config.state_dir.join("push/l1/sock");
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("push.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let other = fixture.config.state_dir.join("push/l2/sock");
        fs::create_dir_all(&other).unwrap();
        let _other = UnixListener::bind(other.join("push.sock")).unwrap();
        std::thread::spawn(move || {
            use std::io::Write;
            for mut stream in listener.incoming().flatten() {
                let _ = stream.write_all(b"pushed\n");
            }
        });
        fixture.spec.push_socket = Some(socket.clone());
        let script = format!(
            r#"
deny() {{ if "$@" 2>/dev/null; then echo "unexpected success: $*" >&2; exit 31; fi; }}
test "$AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET" = '{}'
reply=$(/usr/bin/python3 -c 'import os, socket
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET"])
print(s.recv(64).decode().strip())')
test "$reply" = pushed
deny touch '{}/planted'
deny rm "$AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET"
deny test -e '{}'
deny test -e "$OTHER_RUN"
test -e "$RUN/prompt.md"
"#,
            socket.display(),
            directory.display(),
            other.display()
        );
        fixture.successful(&script);
        assert!(socket.exists() && !directory.join("planted").exists());
    }

    #[test]
    fn preflight_refuses_an_unprotected_relay_binary_and_privileged_relay_ports() {
        if skip_when_nested(
            "preflight_refuses_an_unprotected_relay_binary_and_privileged_relay_ports",
        ) {
            return;
        }
        let mut fixture = Fixture::new(Role::Reviewer);
        let error = format!("{:#}", check(&fixture.spec, &fixture.config).unwrap_err());
        assert!(error.contains("namespace relay"), "{error}");
        fixture.config.egress_listen = "127.0.0.1:80".into();
        let error = format!("{:#}", check(&fixture.spec, &fixture.config).unwrap_err());
        assert!(error.contains("egress_listen"), "{error}");
    }

    #[test]
    fn relay_probe_runs_the_installed_binary_and_refuses_one_without_the_relay() {
        if skip_when_nested(
            "relay_probe_runs_the_installed_binary_and_refuses_one_without_the_relay",
        ) {
            return;
        }
        let fixture = Fixture::new(Role::Reviewer);
        probe_relay(&fixture.spec, &fixture.config).unwrap();
        let program = crate::relay::program(&fixture.config);
        fs::write(
            &program,
            "#!/bin/sh\necho 'unrecognized subcommand' >&2\nexit 2\n",
        )
        .unwrap();
        let error = probe_relay(&fixture.spec, &fixture.config).unwrap_err();
        assert!(error.to_string().contains("namespace relay probe failed"));
    }

    #[test]
    fn claude_launches_cannot_reach_host_loopback_listeners() {
        if skip_when_nested("claude_launches_cannot_reach_host_loopback_listeners") {
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("reachable from the host");
        for role in [Role::Implementer, Role::Reviewer] {
            Fixture::new(role).successful(&format!(
                "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then exit 9; fi"
            ));
        }
    }

    #[test]
    fn mount_audit_refuses_preexisting_hardlinks_special_files_and_symlinked_roots() {
        let fixture = Fixture::new(Role::Implementer);
        let linked = fixture.spec.clone.join("hardlink");
        fs::hard_link(&fixture.outside, &linked).unwrap();
        assert!(
            check_mounts(&fixture.spec, &fixture.config)
                .unwrap_err()
                .to_string()
                .contains("hard-linked")
        );
        fs::remove_file(linked).unwrap();
        let socket = StatePaths::new(&fixture.spec.run).home.join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(
            check_mounts(&fixture.spec, &fixture.config)
                .unwrap_err()
                .to_string()
                .contains("special file")
        );
        drop(listener);
        fs::remove_file(socket).unwrap();
        let temp = fixture.spec.run.join("tmp");
        fs::remove_dir(&temp).unwrap();
        symlink(fixture.outside.parent().unwrap(), &temp).unwrap();
        assert!(check_mounts(&fixture.spec, &fixture.config).is_err());
        assert_eq!(fs::read_to_string(&fixture.outside).unwrap(), "untouched");
    }

    #[test]
    fn inherited_host_file_and_directory_descriptors_are_closed_before_bubblewrap() {
        if skip_when_nested(
            "inherited_host_file_and_directory_descriptors_are_closed_before_bubblewrap",
        ) {
            return;
        }
        let fixture = Fixture::new(Role::Implementer);
        let file = OpenOptions::new()
            .write(true)
            .open(&fixture.outside)
            .unwrap();
        let directory = File::open(fixture.outside.parent().unwrap()).unwrap();
        for fd in [file.as_raw_fd(), directory.as_raw_fd()] {
            // SAFETY: live owned descriptors; deliberately simulate descriptors
            // inherited by the supervisor without the normal Rust CLOEXEC bit.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        }
        fixture.successful(&format!(
            "test ! -e /proc/self/fd/{}; test ! -e /proc/self/fd/{}",
            file.as_raw_fd(),
            directory.as_raw_fd()
        ));
        assert_eq!(fs::read_to_string(&fixture.outside).unwrap(), "untouched");
    }

    #[test]
    fn real_bubblewrap_reads_sealed_stdin_but_cannot_rewrite_it_or_the_host_prompt() {
        if skip_when_nested(
            "real_bubblewrap_reads_sealed_stdin_but_cannot_rewrite_it_or_the_host_prompt",
        ) {
            return;
        }
        let fixture = Fixture::new(Role::Implementer);
        fixture.successful(
            r#"
test "$(cat)" = 'mock prompt'
deny() { if "$@" 2>/dev/null; then echo 'prompt mutation unexpectedly succeeded' >&2; exit 31; fi; }
overwrite() { printf corrupted > "$1"; }
deny overwrite /proc/self/fd/0
deny overwrite /dev/stdin
deny bash -c 'printf corrupted >&0'
deny truncate -s 1 /proc/self/fd/0
deny truncate -s 32 /proc/self/fd/0
deny overwrite "$RUN/prompt.md"
test "$(cat /proc/self/fd/0)" = 'mock prompt'
"#,
        );
        assert_eq!(
            fs::read_to_string(fixture.spec.run.join(run_files::PROMPT)).unwrap(),
            "mock prompt"
        );
    }

    #[test]
    fn prompt_snapshot_has_all_seals_and_rejects_inputs_over_the_bound() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let fixture = Fixture::new(Role::Implementer);
        let path = fixture.spec.run.join(run_files::PROMPT);
        let mut snapshot = sealed_prompt(File::open(&path).unwrap()).unwrap();
        let expected =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        // SAFETY: snapshot is a live owned memfd; F_GET_SEALS takes no extra argument.
        assert_eq!(
            unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_GET_SEALS) },
            expected
        );
        let mut text = String::new();
        snapshot.read_to_string(&mut text).unwrap();
        assert_eq!(text, "mock prompt");
        snapshot.seek(SeekFrom::Start(0)).unwrap();
        assert!(snapshot.write_all(b"changed").is_err());
        assert!(snapshot.set_len(1).is_err());
        assert!(snapshot.set_len(100).is_err());
        let large = OpenOptions::new().write(true).open(&path).unwrap();
        large.set_len(MAX_PROMPT_BYTES).unwrap();
        let boundary = sealed_prompt(File::open(&path).unwrap()).unwrap();
        assert_eq!(boundary.metadata().unwrap().len(), MAX_PROMPT_BYTES);
        large.set_len(MAX_PROMPT_BYTES + 1).unwrap();
        assert!(
            sealed_prompt(File::open(&path).unwrap())
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
    }

    fn namespace_processes(namespace: &Path) -> Vec<PathBuf> {
        fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (fs::read_link(path.join("ns/pid")).ok()?.as_path() == namespace).then_some(path)
            })
            .collect()
    }

    #[test]
    fn namespace_teardown_kills_detached_descendants_on_exit_and_wrapper_kill() {
        if skip_when_nested(
            "namespace_teardown_kills_detached_descendants_on_exit_and_wrapper_kill",
        ) {
            return;
        }
        for kill_wrapper in [false, true] {
            let fixture = Fixture::new(Role::Implementer);
            let script = if kill_wrapper {
                "readlink /proc/self/ns/pid; sleep 30 </dev/null >/dev/null 2>&1 & wait"
            } else {
                "readlink /proc/self/ns/pid; sleep 30 </dev/null >/dev/null 2>&1 & exit 0"
            };
            let mut child = fixture.spawn(script);
            let output = fixture.spec.run.join("events.jsonl");
            let deadline = Instant::now() + Duration::from_secs(5);
            let namespace = loop {
                let text = fs::read_to_string(&output).unwrap();
                if text.trim().starts_with("pid:[") {
                    break PathBuf::from(text.trim());
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("mock harness did not report its PID namespace");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if kill_wrapper {
                child.kill().unwrap();
            }
            child.wait().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !namespace_processes(&namespace).is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                namespace_processes(&namespace).is_empty(),
                "descendant escaped namespace teardown"
            );
        }
    }

    #[test]
    fn bubblewrap_prerequisites_fail_closed_and_codex_remains_native() {
        if skip_when_nested("bubblewrap_prerequisites_fail_closed_and_codex_remains_native") {
            return;
        }
        let fixture = Fixture::new(Role::Implementer);
        probe(&fixture.config, Role::Implementer).unwrap();
        probe(&fixture.config, Role::Reviewer).unwrap();
        let mut config = fixture.config.clone();
        config.bubblewrap = fixture.outside.clone();
        fs::set_permissions(&config.bubblewrap, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(trusted_binary(&config.bubblewrap).is_err());
        config.bubblewrap = "/missing/bwrap".into();
        assert!(check(&fixture.spec, &config).is_err());
        let codex = LaunchSpec {
            harness: Harness::Codex,
            ..fixture.spec.clone()
        };
        let native = profile::command(&codex, &config);
        assert_eq!(wrap(native.clone(), &codex, &config).unwrap(), native);
        assert!(check(&codex, &config).is_ok());
    }

    /// The fixture with a setup for project `p1` that runs `script` in Bash
    /// with the clone, a test-owned cache directory and the outside file as
    /// `$1`, `$2` and `$3`.
    fn with_setup(script: &str) -> (Fixture, PathBuf) {
        let mut fixture = Fixture::new(Role::Implementer);
        let cache = fixture._root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        fixture.spec.project = Some("p1".into());
        let mut command: Vec<String> = ["/bin/bash", "-euc", script, "setup"]
            .map(String::from)
            .into();
        for path in [&fixture.spec.clone, &cache, &fixture.outside] {
            command.push(path.display().to_string());
        }
        let setup = crate::setup::ProjectSetup {
            command,
            cache_paths: vec![cache.clone()],
            ..Default::default()
        };
        fixture.config.setup.insert("p1".into(), setup);
        (fixture, cache)
    }

    #[test]
    fn project_setup_runs_sandboxed_and_writes_only_the_clone_and_caches() {
        if skip_when_nested("project_setup_runs_sandboxed_and_writes_only_the_clone_and_caches") {
            return;
        }
        let script = r#"
deny() { if "$@" 2>/dev/null; then echo "unexpected success: $*" >&2; exit 31; fi; }
printf fetched > "$1/fetched"
printf cached > "$2/entry"
deny sh -c "printf corrupted > '$3'"
test "$(awk '/^NoNewPrivs:/ {print $2}' /proc/self/status)" = 1
test "$(awk '/^CapEff:/ {print $2}' /proc/self/status)" = 0000000000000000
echo setup-output
"#;
        let (fixture, cache) = with_setup(script);
        let result = crate::setup::run(&fixture.spec, &fixture.config);
        let log = fs::read_to_string(fixture.spec.run.join(crate::setup::SETUP_LOG)).unwrap();
        assert!(result.is_ok(), "{result:?}: {log}");
        assert_eq!(
            fs::read_to_string(fixture.spec.clone.join("fetched")).unwrap(),
            "fetched"
        );
        assert_eq!(fs::read_to_string(cache.join("entry")).unwrap(), "cached");
        assert_eq!(fs::read_to_string(&fixture.outside).unwrap(), "untouched");
        assert!(log.contains("setup-output"), "{log}");
        let wrapped =
            crate::setup::sandboxed(&fixture.config.setup["p1"], &fixture.spec, &fixture.config);
        assert_eq!(wrapped.unwrap().program, fixture.config.bubblewrap);
    }

    #[test]
    fn a_failed_slow_or_codex_setup_refuses_the_launch() {
        if skip_when_nested("a_failed_slow_or_codex_setup_refuses_the_launch") {
            return;
        }
        let (mut fixture, _) = with_setup("exit 3");
        let error = crate::setup::run(&fixture.spec, &fixture.config).unwrap_err();
        assert!(format!("{error:#}").contains("setup failed"), "{error:#}");
        fs::remove_file(fixture.spec.run.join(crate::setup::SETUP_LOG)).unwrap();
        let setup = fixture.config.setup.get_mut("p1").unwrap();
        (setup.command, setup.timeout_seconds) = (vec!["/bin/sleep".into(), "30".into()], 1);
        let error = crate::setup::run(&fixture.spec, &fixture.config).unwrap_err();
        assert!(
            format!("{error:#}").contains("longer than 1 s"),
            "{error:#}"
        );
        fixture.spec.harness = Harness::Codex;
        let error = crate::setup::run(&fixture.spec, &fixture.config).unwrap_err();
        assert!(format!("{error:#}").contains("Bubblewrap"), "{error:#}");
        fixture.spec.project = None;
        assert!(crate::setup::run(&fixture.spec, &fixture.config).is_ok());
    }

    #[test]
    fn cache_paths_are_bound_writable_only_when_owned_real_directories() {
        let (mut fixture, cache) = with_setup("true");
        let bind = [
            OsString::from("--bind"),
            cache.clone().into(),
            cache.clone().into(),
        ];
        assert!(fixture.command("true").args.windows(3).any(|w| w == bind));
        let link = fixture._root.path().join("link");
        symlink(&cache, &link).unwrap();
        // SAFETY: geteuid has no preconditions.
        let root = unsafe { libc::geteuid() } == 0;
        let refused = [
            link,
            PathBuf::from("relative"),
            PathBuf::from("/no/such/dir"),
        ];
        let foreign = (!root).then(|| PathBuf::from("/proc"));
        for path in refused.into_iter().chain(foreign) {
            fixture.config.setup.get_mut("p1").unwrap().cache_paths = vec![path.clone()];
            let command = profile::command(&fixture.spec, &fixture.config);
            assert!(
                wrap(command, &fixture.spec, &fixture.config).is_err(),
                "{}",
                path.display()
            );
        }
    }
}
