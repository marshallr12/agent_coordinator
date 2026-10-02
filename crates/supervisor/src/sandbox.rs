//! Claude's OS write boundary. The whole host and run start read-only;
//! individual writable mount roots cannot be renamed around seed overlays.
//! Networking remains in the host namespace under the existing firewall.
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

fn mount(args: &mut Vec<OsString>, option: &str, source: &Path, destination: &Path) {
    args.extend([OsString::from(option), source.into(), destination.into()]);
}

/// Build the exact executable command, also exposed in `launch --dry-run`.
/// Codex's existing native profile and authentication layout are unchanged.
pub fn wrap(mut command: LaunchCommand, spec: &LaunchSpec, config: &Config) -> LaunchCommand {
    if spec.harness != Harness::Claude {
        return command;
    }
    let mut args = base_args(spec.role);
    for path in writable_directories(spec) {
        mount(&mut args, "--bind", &path, &path);
    }
    let persistent = claude_directory(spec, config);
    let credential = persistent.join(".credentials.json");
    mount(&mut args, "--bind", &credential, &credential);
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
    args.extend([
        OsString::from("--chdir"),
        spec.clone.clone().into(),
        OsString::from("--"),
        command.program.into(),
    ]);
    args.append(&mut command.args);
    command.program = config.bubblewrap.clone();
    command.args = args;
    command
}

/// Fail before launch for unsafe paths/inodes or an unavailable OS boundary.
pub fn check(spec: &LaunchSpec, config: &Config) -> Result<()> {
    if spec.harness != Harness::Claude {
        return Ok(());
    }
    trusted_binary(&config.bubblewrap)?;
    check_mounts(spec, config)?;
    probe(config, spec.role)
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
    // Validate source and destination mountpoints; never read auth bytes.
    for path in [
        config.cargo_config_seed.clone(),
        StatePaths::new(&spec.run).cargo.join("config.toml"),
        persistent.join("settings.json"),
        persistent.join("CLAUDE.md"),
        persistent.join(".credentials.json"),
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
    command
        .arg("/bin/true")
        .env_clear()
        .current_dir("/")
        .stdin(sealed_prompt(fs::File::open("/dev/null")?)?)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    fence_descriptors(&mut command)?;
    let mut child = command
        .spawn()
        .context("start required Bubblewrap kernel probe")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "Bubblewrap/kernel confinement probe failed; owner setup required"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill().context("stop timed-out Bubblewrap probe")?;
            child.wait().context("reap Bubblewrap probe")?;
            anyhow::bail!("Bubblewrap/kernel confinement probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
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

    impl Fixture {
        fn new(role: Role) -> Self {
            let root = tempfile::tempdir().unwrap();
            let config = Config {
                state_dir: root.path().join("roles"),
                cargo_config_seed: root.path().join("cargo-seed.toml"),
                ..Config::default()
            };
            fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
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
            fs::write(persistent.join(".credentials.json"), "dummy fixture auth").unwrap();
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
                ("CLONE", self.spec.clone.clone()),
                ("OUTSIDE", self.outside.clone()),
                ("ROLE_HOME", self.role_home.clone()),
                ("OTHER_RUN", self.other_run.clone()),
                ("OTHER_CLONE", self.other_clone.clone()),
                ("SEED", self.config.cargo_config_seed.clone()),
            ] {
                inner.env.push((key.into(), value.into()));
            }
            wrap(inner, &self.spec, &self.config)
        }

        fn spawn(&self, script: &str) -> std::process::Child {
            trusted_binary(&self.config.bubblewrap).unwrap();
            check_mounts(&self.spec, &self.config).unwrap();
            launch::spawn(&self.command(script), &self.spec).unwrap()
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

    /// Run by the mock reviewer harness: the harness itself still sees its
    /// login, but each command through `candidate-shell` sees no secrets,
    /// cannot write the clone or the harness's temp, cannot nest another user
    /// namespace, and leaves the harness's tracked directory in the clone.
    const REVIEWER_CANDIDATE_CHECKS: &str = r#"
deny overwrite "$CLONE/source"
test "$(cat "$CLAUDE_CONFIG_DIR/.credentials.json")" = refreshed
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
test ! -s "$CLAUDE_CONFIG_DIR/.credentials.json"
test -e "$CLAUDE_CONFIG_DIR/settings.json"
deny sh -c "printf x >> \"\$CLAUDE_CONFIG_DIR/settings.json\""
deny sh -c "printf x > \"\$CLAUDE_CONFIG_DIR/planted\""
deny sh -c "printf x > \"\$RUN/verification.json\""
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
printf refreshed > "$CLAUDE_CONFIG_DIR/.credentials.json"
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
        assert_eq!(wrap(native.clone(), &codex, &config), native);
        assert!(check(&codex, &config).is_ok());
    }
}
