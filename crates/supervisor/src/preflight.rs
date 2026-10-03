//! Launch preflight (autonomy plan §2.3, B7a): refuse to launch unless the
//! exact containment the profile assumes is in place. Every check reports a
//! human-readable problem instead of failing fast, so one run lists them all.
use crate::clone;
use crate::config::Config;
use crate::confine::{self, StatePaths};
use crate::network_probe;
use crate::profile::{self, Harness, LaunchSpec, Role, run_files};
use crate::role_settings;
use crate::verification;
use anyhow::{Context, Result, ensure};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::Command;

/// All problems that block `spec` on this host; empty means launchable.
pub fn check(spec: &LaunchSpec, config: &Config) -> Vec<String> {
    let command = profile::command(spec, config);
    let mut problems = Vec::new();
    problems.extend(account_problem(spec.role, config));
    problems.extend(binary_problems(
        &command.program,
        pinned(spec.harness, config),
    ));
    problems.extend(layout_problems(spec));
    problems.extend(state_problems(spec, config));
    problems.extend(settings_problem(spec));
    if let Err(error) = crate::sandbox::check(spec, config) {
        problems.push(format!("Claude write confinement: {error:#}"));
    }
    problems.extend(crate::candidate::unsupported(spec));
    problems.extend(verification_problems(spec, config));
    #[cfg(target_os = "linux")]
    problems.extend(push_socket_problem(spec, config));
    problems.extend(network_problems(config));
    match clone::hardening_problems(&spec.clone) {
        Ok(found) => problems.extend(found),
        Err(error) => problems.push(format!("clone {}: {error:#}", spec.clone.display())),
    }
    problems
}

/// The firewall and egress proxy must be in force for this identity (the
/// role account, per `account_problem`), which is also the launch's.
fn network_problems(config: &Config) -> Vec<String> {
    let timeout = network_probe::PROBE_TIMEOUT;
    let direct = network_probe::direct_egress_problem(&config.egress_probe_target, timeout);
    let proxy = network_probe::proxy_problem(
        &config.egress_listen,
        &config.egress_probe_blocked_host,
        timeout,
    );
    direct.into_iter().chain(proxy).collect()
}

/// A candidate-push helper socket is for implementers only and must be an
/// existing socket at an absolute path short enough for the helper to bind
/// (see `push_helper::check_socket_path`), canonically spelled without
/// symlinks, in a launch's `sock/` directory
/// under `<state_dir>/push`, the directory the sandbox masks.
#[cfg(target_os = "linux")]
fn push_socket_problem(spec: &LaunchSpec, config: &Config) -> Option<String> {
    let socket = spec.push_socket.as_deref()?;
    let checked = (|| {
        ensure!(
            spec.role == Role::Implementer,
            "only implementer launches get a candidate-push helper"
        );
        crate::push_helper::check_socket_path(socket)?;
        confine::path_without_symlinks(socket, false)?;
        let in_sock = socket.parent().and_then(Path::file_name) == Some("sock".as_ref());
        ensure!(
            in_sock && socket.ancestors().nth(3) == Some(crate::push_helper_root(config).as_path()),
            "it must be <state_dir>/push/<launch>/sock/<name>"
        );
        let metadata = fs::symlink_metadata(socket)?;
        ensure!(metadata.file_type().is_socket(), "it is not a socket");
        Ok(())
    })();
    checked
        .err()
        .map(|error| format!("push socket {}: {error:#}", socket.display()))
}

/// The pinned version string for a harness.
fn pinned(harness: Harness, config: &Config) -> &str {
    match harness {
        Harness::Claude => &config.pinned.claude,
        Harness::Codex => &config.pinned.codex,
    }
}

/// The launch must run as the role's dedicated account, never the owner's.
fn account_problem(role: Role, config: &Config) -> Option<String> {
    let current = match Command::new("id").arg("-un").output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => return Some("cannot determine current role account".into()),
    };
    let expected = role.user(config);
    (current != expected)
        .then(|| format!("running as {current:?}; {role:?} launches run as {expected:?}"))
}

/// The harness binary must exist, be pinned, report the pinned version, and
/// not be writable by the agent account.
fn binary_problems(program: &Path, pinned: &str) -> Vec<String> {
    if !program.exists() {
        return vec![format!("{} is not installed", program.display())];
    }
    let mut problems = writable_problem(program).into_iter().collect::<Vec<_>>();
    if !problems.is_empty() {
        return problems;
    }
    if pinned.is_empty() {
        problems.push(format!(
            "no pinned version configured for {}",
            program.display()
        ));
        return problems;
    }
    let mut version = Command::new(program);
    crate::reaper::forbid_new_privileges(&mut version);
    let reported = version.arg("--version").output();
    let reported = reported.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    if !reported.as_deref().is_ok_and(|text| text.contains(pinned)) {
        problems.push(format!(
            "{} does not report pinned version {pinned:?}",
            program.display()
        ));
    }
    problems
}

/// Never execute an untrusted binary, including its version command.
fn writable_problem(program: &Path) -> Option<String> {
    confine::protected_executable(program)
        .err()
        .map(|error| format!("{error:#}"))
}

/// `$RUN` and the clone must be disjoint, and the prompt must exist.
fn layout_problems(spec: &LaunchSpec) -> Vec<String> {
    let mut problems = Vec::new();
    if profile::is_within(&spec.run, &spec.clone) || profile::is_within(&spec.clone, &spec.run) {
        problems.push("the run directory and the clone must not contain each other".into());
    }
    if let Err(error) = confine::regular_file(&spec.run.join(run_files::PROMPT)) {
        problems.push(format!(
            "prompt.md must be a regular unlinked file: {error:#}"
        ));
    }
    if spec.role == Role::Reviewer
        && let Err(error) = confine::regular_file(&spec.run.join(run_files::SCHEMA))
    {
        problems.push(format!(
            "reviewer launches need regular result.schema.json: {error:#}"
        ));
    }
    problems
}

/// Claude launches need the generated role settings, byte-for-byte in meaning.
fn settings_problem(spec: &LaunchSpec) -> Option<String> {
    if spec.harness != Harness::Claude {
        return None;
    }
    let path = spec.run.join(run_files::SETTINGS);
    let installed = confine::read_regular(&path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();
    (!role_settings::matches(spec.role, &installed)).then(|| {
        format!(
            "{} differs from the generated {:?} settings",
            path.display(),
            spec.role
        )
    })
}

/// Host seeds must survive an agent trying to rename their parent. Every
/// ancestor is root-owned and closed to group/world writes, including role
/// roots; writable work lives in individually owned child directories.
fn state_problems(spec: &LaunchSpec, config: &Config) -> Vec<String> {
    let mut problems = Vec::new();
    let mut record = |result: Result<()>| {
        if let Err(error) = result {
            problems.push(format!("{error:#}"));
        }
    };
    record(confine::managed_paths(spec, config, false));
    record(protected_directory(
        &config.state_dir.join(spec.role.slug()),
    ));
    record(protected_file(&config.cargo_config_seed));
    record(confine::cargo_seed(config).map(|_| ()));
    for directory in ["runs", "clones"] {
        let path = config.state_dir.join(spec.role.slug()).join(directory);
        record(confine::check_private_dir(&path));
        record(role_owned(&path, spec.role, config, false));
    }
    if spec.harness == Harness::Codex {
        let path = config.state_dir.join(spec.role.slug()).join("codex-home");
        record(confine::check_private_dir(&path));
        record(role_owned(&path, spec.role, config, false));
    }
    let state = StatePaths::new(&spec.run);
    for dir in state
        .directories()
        .into_iter()
        .chain([spec.run.join("tmp"), spec.run.join("target")])
    {
        record(confine::check_private_dir(&dir));
        record(role_owned(&dir, spec.role, config, false));
    }
    let cargo = state.cargo.join("config.toml");
    record(role_owned(&cargo, spec.role, config, true));
    record((|| {
        ensure!(
            confine::read_regular(&cargo)? == confine::CARGO_CONFIG_SEED,
            "per-launch Cargo config differs from the required baseline"
        );
        Ok(())
    })());
    if spec.harness == Harness::Claude {
        let persistent = config
            .state_dir
            .join(spec.role.slug())
            .join("claude-config");
        record(protected_directory(&persistent));
        for file in ["settings.json", "CLAUDE.md"] {
            record(protected_file(&persistent.join(file)));
        }
        record((|| {
            let settings = confine::read_regular(&persistent.join("settings.json"))?;
            ensure!(
                std::str::from_utf8(&settings)
                    .is_ok_and(|text| role_settings::matches(spec.role, text)),
                "persistent Claude settings differ from the generated role settings"
            );
            ensure!(
                confine::read_regular(&persistent.join("CLAUDE.md"))?.is_empty(),
                "persistent CLAUDE.md must be the empty seed"
            );
            Ok(())
        })());
        record(role_owned(
            &persistent.join(".credentials.json"),
            spec.role,
            config,
            true,
        ));
        record((|| {
            for entry in fs::read_dir(&persistent)? {
                let entry = entry?;
                ensure!(
                    ["settings.json", "CLAUDE.md", ".credentials.json"]
                        .iter()
                        .any(|name| entry.file_name() == *name),
                    "persistent Claude config contains an unexpected entry; owner cleanup required"
                );
            }
            Ok(())
        })());
    }
    problems
}

fn protected_directory(path: &Path) -> Result<()> {
    confine::path_without_symlinks(path, false)?;
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        ensure!(
            metadata.is_dir(),
            "{} must be a directory",
            ancestor.display()
        );
        protected_metadata(ancestor, &metadata, false)?;
    }
    Ok(())
}

fn protected_file(path: &Path) -> Result<()> {
    confine::path_without_symlinks(path, false)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file(),
        "{} must be a regular file",
        path.display()
    );
    protected_metadata(path, &metadata, true)?;
    protected_directory(path.parent().context("protected seed has no parent")?)
}

#[cfg(unix)]
fn protected_metadata(path: &Path, metadata: &fs::Metadata, file: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    protection_facts(
        path,
        metadata.uid(),
        metadata.mode(),
        metadata.nlink(),
        file,
    )
}

/// Kept independent of the test runner's uid so hostile ownership/mode cases
/// are exercised without privileges, rather than skipping root seed checks.
#[cfg(unix)]
fn protection_facts(path: &Path, uid: u32, mode: u32, links: u64, file: bool) -> Result<()> {
    ensure!(uid == 0, "{} must be root-owned", path.display());
    ensure!(
        mode & if file { 0o222 } else { 0o022 } == 0,
        "{} must be {}",
        path.display(),
        if file {
            "read-only"
        } else {
            "not group/world-writable"
        }
    );
    ensure!(
        !file || links == 1,
        "{} must not be hard-linked",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn protected_metadata(_path: &Path, _metadata: &fs::Metadata, _file: bool) -> Result<()> {
    anyhow::bail!("protected host seeds require Unix ownership checks")
}

/// Metadata only: never read, copy, or print an authentication file's bytes.
#[cfg(unix)]
fn role_owned(path: &Path, role: Role, config: &Config, file: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    confine::path_without_symlinks(path, false)?;
    let output = Command::new("id")
        .args(["-u", role.user(config)])
        .output()?;
    ensure!(
        output.status.success(),
        "cannot resolve role account {}",
        role.user(config)
    );
    let uid: u32 = std::str::from_utf8(&output.stdout)?.trim().parse()?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.uid() == uid,
        "{} must be owned by its role account",
        path.display()
    );
    if file {
        ensure!(
            metadata.is_file() && metadata.nlink() == 1 && metadata.mode() & 0o7777 == 0o600,
            "{} must be a regular, single-link mode 0600 file",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn role_owned(_path: &Path, _role: Role, _config: &Config, _file: bool) -> Result<()> {
    anyhow::bail!("private role state requires Unix ownership checks")
}

/// A verifying reviewer needs its project's test login (private to the
/// reviewer account) and, when offered, a browser agents cannot replace.
fn verification_problems(spec: &LaunchSpec, config: &Config) -> Vec<String> {
    let Some(entry) = verification::for_launch(spec, config) else {
        return Vec::new();
    };
    let project = spec.project.as_deref().unwrap_or_default();
    let login = verification::credential_file(config, project);
    let mut problems = private_file_problem(&login).into_iter().collect::<Vec<_>>();
    let directory = config
        .state_dir
        .join(Role::Reviewer.slug())
        .join("verification");
    if !login.starts_with(directory) {
        problems.push("verification login must remain below the managed reviewer directory".into());
    }
    if entry.browser {
        problems.extend(match config.browser.exists() {
            true => writable_problem(&config.browser),
            false => Some(format!(
                "browser {} is not installed",
                config.browser.display()
            )),
        });
    }
    problems
}

/// The file must be readable by this account and closed to group and world.
#[cfg(unix)]
fn private_file_problem(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = confine::regular_file(path);
    let readable = metadata.is_ok() && std::fs::File::open(path).is_ok();
    let private = metadata.is_ok_and(|metadata| metadata.mode() & 0o7777 == 0o600);
    (!readable || !private).then(|| {
        format!(
            "verification login {} must exist, be readable by this account and be mode 0600",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn private_file_problem(_path: &Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// A loopback address with nothing listening (bound, then released).
    fn closed_port() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().to_string()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_push_socket_must_be_an_implementers_short_absolute_socket() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let config = Config {
            state_dir: dir.path().into(),
            egress_probe_target: closed_port(),
            egress_listen: closed_port(),
            ..Config::default()
        };
        let sock = dir.path().join("push/l1/sock");
        fs::create_dir_all(&sock).unwrap();
        let socket = sock.join("push.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let elsewhere = dir.path().join("elsewhere.sock");
        let _other = std::os::unix::net::UnixListener::bind(&elsewhere).unwrap();
        let mut spec = LaunchSpec {
            role: Role::Implementer,
            harness: Harness::Claude,
            clone: dir.path().join("clone"),
            run: dir.path().join("run"),
            model: "m".into(),
            effort: "low".into(),
            session_id: Uuid::nil(),
            project: None,
            task: None,
            push_socket: None,
        };
        assert_eq!(push_socket_problem(&spec, &config), None);
        spec.push_socket = Some(socket.clone());
        assert_eq!(push_socket_problem(&spec, &config), None);
        let file = sock.join("file");
        fs::write(&file, "").unwrap();
        let long = std::path::PathBuf::from(format!("/{}", "a".repeat(93)));
        let cases = [
            (file, "not a socket"),
            ("relative.sock".into(), "absolute"),
            (long, "93 bytes"),
            (elsewhere, "<state_dir>/push/<launch>/sock/<name>"),
            (
                dir.path().join("push/l1/sock/../sock/push.sock"),
                "canonical",
            ),
            (dir.path().join("push/./l1/sock/push.sock"), "canonical"),
        ];
        for (path, expected) in cases {
            spec.push_socket = Some(path);
            let problem = push_socket_problem(&spec, &config).unwrap_or_default();
            assert!(problem.contains(expected), "{expected}: {problem}");
        }
        spec.push_socket = Some(socket);
        spec.role = Role::Reviewer;
        let problems = check(&spec, &config).join("\n");
        assert!(problems.contains("only implementer launches"), "{problems}");
    }

    #[test]
    fn missing_containment_is_reported_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let spec = LaunchSpec {
            role: Role::Reviewer,
            harness: Harness::Claude,
            clone: dir.path().join("clone"),
            run: dir.path().join("clone/run"),
            model: "m".into(),
            effort: "low".into(),
            session_id: Uuid::nil(),
            project: Some("p1".into()),
            task: None,
            push_socket: None,
        };
        let mut config: Config =
            toml::from_str("[verification.p1]\nurl = \"http://127.0.0.1:1\"").unwrap();
        config.bin_dir = dir.path().into();
        config.state_dir = dir.path().into();
        config.browser = dir.path().join("no-browser");
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.egress_probe_target = open.local_addr().unwrap().to_string();
        config.egress_listen = closed_port();
        let problems = check(&spec, &config).join("\n");
        for expected in [
            "launches run as",
            "not installed",
            "must not contain",
            "prompt.md",
            "schema",
            "differs",
            "clone",
            "verification login",
            "no-browser is not installed",
            "direct egress reachable",
            "egress proxy not running",
        ] {
            assert!(problems.contains(expected), "{expected}: {problems}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn seed_protection_requires_root_and_unreplaceable_parents_without_privileged_tests() {
        let path = Path::new("/seed");
        assert!(protection_facts(path, 0, 0o444, 1, true).is_ok());
        assert!(protection_facts(path, 1000, 0o444, 1, true).is_err());
        for mode in [0o644, 0o464, 0o446] {
            assert!(protection_facts(path, 0, mode, 1, true).is_err());
        }
        assert!(protection_facts(path, 0, 0o444, 2, true).is_err());
        assert!(protection_facts(path, 0, 0o750, 2, false).is_ok());
        assert!(protection_facts(path, 1000, 0o750, 2, false).is_err());
        for mode in [0o770, 0o752, 0o1777] {
            assert!(protection_facts(path, 0, mode, 2, false).is_err());
        }
        // Even a read-only seed is untrusted under a writable/non-root
        // ancestor. This real-filesystem check fails under either test uid.
        let root = tempfile::tempdir().unwrap();
        let seed = root.path().join("seed");
        fs::write(&seed, confine::CARGO_CONFIG_SEED).unwrap();
        use std::os::unix::fs::{PermissionsExt, symlink};
        fs::set_permissions(&seed, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(protected_file(&seed).is_err());
        let link = root.path().join("linked");
        symlink(&seed, &link).unwrap();
        assert!(protected_file(&link).is_err());
        assert!(protected_file(&root.path().join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn preflight_reports_altered_cargo_claude_seeds_modes_and_credential_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let current = Command::new("id").arg("-un").output().unwrap();
        let mut config = Config {
            state_dir: root.path().join("roles"),
            cargo_config_seed: root.path().join("seed"),
            implementer_user: String::from_utf8(current.stdout).unwrap().trim().into(),
            ..Config::default()
        };
        fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
        let spec = LaunchSpec {
            role: Role::Implementer,
            harness: Harness::Claude,
            clone: config.state_dir.join("impl/clones/one"),
            run: config.state_dir.join("impl/runs/one"),
            model: "test".into(),
            effort: "low".into(),
            session_id: Uuid::nil(),
            project: None,
            task: None,
            push_socket: None,
        };
        fs::create_dir_all(&spec.clone).unwrap();
        crate::launch::prepare_run(&spec, &config).unwrap();
        let persistent = config.state_dir.join("impl/claude-config");
        fs::create_dir(&persistent).unwrap();
        fs::write(
            persistent.join("settings.json"),
            crate::role_settings::render(Role::Reviewer),
        )
        .unwrap();
        fs::write(persistent.join("CLAUDE.md"), "unexpected instructions").unwrap();
        symlink(
            &config.cargo_config_seed,
            persistent.join(".credentials.json"),
        )
        .unwrap();
        fs::write(persistent.join("extra.json"), "unexpected").unwrap();
        let state = StatePaths::new(&spec.run);
        fs::set_permissions(&state.home, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(state.cargo.join("config.toml"), "altered").unwrap();
        let problems = state_problems(&spec, &config).join("\n");
        for expected in [
            "0700",
            "per-launch Cargo config differs",
            "persistent Claude settings differ",
            "symlink",
            "unexpected entry",
        ] {
            assert!(problems.contains(expected), "{expected}: {problems}");
        }
        fs::write(
            persistent.join("settings.json"),
            crate::role_settings::render(spec.role),
        )
        .unwrap();
        assert!(
            state_problems(&spec, &config)
                .join("\n")
                .contains("CLAUDE.md must be the empty seed")
        );
        let codex_home = config.state_dir.join("impl/codex-home");
        symlink(root.path(), &codex_home).unwrap();
        let codex = LaunchSpec {
            harness: Harness::Codex,
            ..spec.clone()
        };
        assert!(
            state_problems(&codex, &config)
                .iter()
                .any(|problem| problem.contains("codex-home") && problem.contains("symlink"))
        );
        config.cargo_config_seed = root.path().join("missing-seed");
        assert!(
            state_problems(&spec, &config)
                .join("\n")
                .contains("required Cargo config seed")
        );
    }
    #[cfg(unix)]
    #[test]
    fn unsafe_binary_is_refused_without_executing_its_version_command() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("claude");
        fs::write(&program, "#!/bin/sh\n: > \"$0.ran\"\nprintf pinned\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!binary_problems(&program, "pinned").is_empty());
        assert!(!root.path().join("claude.ran").exists());
    }
}
