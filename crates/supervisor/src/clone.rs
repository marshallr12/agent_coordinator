//! Per-launch Git clones (autonomy plan §2.3, "Git isolation between roles").
//!
//! Each launch gets its own clone owned by its role's uid, so implementer and
//! reviewer never share a `.git`. Clones are hardened: hooks and fsmonitor are
//! disabled (a candidate cannot plant code that Git runs for us), and the
//! `origin` push URL is dead, so a raw `git push origin …` fails while the
//! coordinator CLI's secret-scanned candidate push, which targets the
//! configured repository URL explicitly, still works.
use anyhow::{Context, Result, bail, ensure};
use std::path::Path;
use std::process::{Command, Output};

/// The directory inside an implementer clone where the loop prepares and
/// registers the attempt's worktree; the clone's `info/exclude` hides it.
pub const CHECKOUT_DIR: &str = "agentc-checkout";

/// Push URL that no transport accepts.
pub const DISABLED_PUSH_URL: &str = "disabled://push-only-via-agent-coordinator";

/// Repository settings every per-launch clone must carry.
pub fn hardening() -> [(&'static str, &'static str); 3] {
    [
        ("core.hooksPath", "/dev/null"),
        ("core.fsmonitor", "false"),
        ("remote.origin.pushurl", DISABLED_PUSH_URL),
    ]
}

/// Clones `url` into `dest` (borrowing objects from `mirror` when present),
/// hardens it, and checks out `revision` detached.
pub fn create(url: &str, mirror: Option<&Path>, revision: &str, dest: &Path) -> Result<()> {
    ensure!(!dest.exists(), "{} already exists", dest.display());
    ensure!(
        !url.starts_with('-'),
        "repository URL cannot begin with a dash"
    );
    let mut args = vec!["clone", "--no-hardlinks", "--no-checkout", "--quiet"];
    let mirror_text = mirror.map(|m| m.display().to_string());
    if let Some(mirror) = &mirror_text {
        args.extend(["--reference-if-able", mirror]);
    }
    let dest_text = dest.display().to_string();
    args.extend(["--", url, &dest_text]);
    run(None, &args)?;
    for (key, value) in hardening() {
        run(Some(dest), &["config", "--local", key, value])?;
    }
    run(Some(dest), &["checkout", "--quiet", "--detach", revision])?;
    let head = text(run(Some(dest), &["rev-parse", "HEAD"])?)?;
    ensure!(head == revision, "clone HEAD {head} is not {revision}");
    exclude_checkout(dest)
}

/// Appends [`CHECKOUT_DIR`] to the clone's `info/exclude`, so the prepared
/// worktree never shows up as untracked in the clone itself.
fn exclude_checkout(dest: &Path) -> Result<()> {
    use std::io::Write;
    let info = dest.join(".git/info");
    std::fs::create_dir_all(&info)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(info.join("exclude"))?;
    Ok(writeln!(file, "/{CHECKOUT_DIR}/")?)
}

/// Sets the clone's repository-local commit identity (`user.name`,
/// `user.email`), which every worktree of the clone shares. The agent may
/// not run `git config`, so the identity comes from the host configuration.
pub fn set_identity(dest: &Path, name: &str, email: &str) -> Result<()> {
    for (key, value) in [("user.name", name), ("user.email", email)] {
        let plain = !value.is_empty() && !value.starts_with('-');
        ensure!(
            plain && !value.contains(['\n', '\r', '\0']),
            "{key} must be a non-empty single line not starting with '-'"
        );
        run(Some(dest), &["config", "--local", key, value])?;
    }
    Ok(())
}

/// Points `origin`'s fetch URL at the canonical repository after cloning from
/// a local mirror, so tools that read `remote.origin.url` (build provenance,
/// the coordinator CLI) see the real remote. The dead push URL is untouched.
pub fn set_origin(dest: &Path, url: &str) -> Result<()> {
    ensure!(!url.starts_with('-'), "origin URL cannot begin with a dash");
    run(Some(dest), &["remote", "set-url", "origin", url])?;
    Ok(())
}

/// Lists the hardening settings that are missing or wrong in `clone`.
pub fn hardening_problems(clone: &Path) -> Result<Vec<String>> {
    ensure!(clone.join(".git").is_dir(), "not a Git clone");
    let mut problems = Vec::new();
    for (key, expected) in hardening() {
        let output = git(Some(clone), &["config", "--local", "--get", key])?;
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if actual != expected {
            problems.push(format!("{key} is {actual:?}, expected {expected:?}"));
        }
    }
    Ok(problems)
}

/// Runs hardened Git in `cwd` and returns its trimmed stdout; `run` uses it
/// on the root-owned mirror.
pub fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    // Mirror data is deliberately readable by both role UIDs, but only root
    // may write it. A supervisor's private-state umask (often 0077) must not
    // turn newly fetched refs/objects into unreadable clone inputs. Keep this
    // override local to mirror Git; changing the process umask would also
    // expose credentials and per-launch state.
    //
    // Git 2.47 and later let a fetch leave `git maintenance run --auto
    // --detach` running after it returns, and its repack deletes files a
    // role's `clone --no-hardlinks` of the mirror may already have listed.
    // Run the maintenance in the foreground so it is finished (and has
    // written its files under the same sharedRepository override) before this
    // returns. Git versions that do not know a key ignore it: `gc.autoDetach`
    // is the older spelling the gc task honours, `maintenance.autoDetach` the
    // one 2.47 added.
    let mut mirror_args = vec![
        "-c",
        "core.sharedRepository=0644",
        "-c",
        "maintenance.autoDetach=false",
        "-c",
        "gc.autoDetach=false",
    ];
    mirror_args.extend_from_slice(args);
    text(run(Some(cwd), &mirror_args)?)
}

/// Runs Git with hooks and fsmonitor disabled for this invocation too.
fn git(cwd: Option<&Path>, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
    ]);
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    command.args(args).env("GIT_TERMINAL_PROMPT", "0");
    command.output().context("run git")
}

/// Runs Git and fails with its stderr on a non-zero exit.
fn run(cwd: Option<&Path>, args: &[&str]) -> Result<Output> {
    let output = git(cwd, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

/// Trimmed UTF-8 stdout.
fn text(output: Output) -> Result<String> {
    Ok(String::from_utf8(output.stdout)
        .context("git printed non-UTF-8")?
        .trim()
        .to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    /// A bare remote with one commit whose tree plants a post-checkout hook
    /// file (hooks are never cloned, but the test also plants one directly).
    fn remote(dir: &Path) -> (String, String) {
        let work = dir.join("work");
        fs::create_dir(&work).unwrap();
        let sh = |args: &[&str], cwd: &Path| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        sh(&["init", "-q", "-b", "main"], &work);
        fs::write(work.join("a.txt"), "a\n").unwrap();
        sh(&["add", "a.txt"], &work);
        sh(
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "a",
            ],
            &work,
        );
        let head = text(git(Some(&work), &["rev-parse", "HEAD"]).unwrap()).unwrap();
        (work.display().to_string(), head)
    }

    #[test]
    fn clones_are_hardened_detached_and_refuse_raw_push() {
        let dir = tempfile::tempdir().unwrap();
        let (url, head) = remote(dir.path());
        let dest = dir.path().join("clone");
        create(&url, None, &head, &dest).unwrap();
        assert!(hardening_problems(&dest).unwrap().is_empty());
        let marker = dir.path().join("HOOK_RAN");
        let hook = dest.join(".git/hooks/post-commit");
        fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        make_executable(&hook);
        let committed = Command::new("git")
            .args([
                "-C",
                dest.to_str().unwrap(),
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
            ])
            .args([
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "planted hook must not run",
            ])
            .status()
            .unwrap();
        assert!(committed.success());
        assert!(!marker.exists(), "a planted hook ran");
        let pushed = Command::new("git")
            .args([
                "-C",
                dest.to_str().unwrap(),
                "push",
                "origin",
                "HEAD:refs/heads/x",
            ])
            .output()
            .unwrap();
        assert!(!pushed.status.success(), "raw push to origin succeeded");
    }

    /// Marks a planted hook executable so only the hardening can stop it.
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Walks `root` and asserts that every path below it is readable by
    /// group and other, searchable where it is a directory, and writable by
    /// neither. A path that cannot be read is named in the panic.
    fn assert_readable_without_worker_writes(root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = fs::metadata(&path)
                .unwrap_or_else(|error| panic!("cannot read mirror path {path:?}: {error}"));
            let mode = metadata.permissions().mode();
            assert_eq!(mode & 0o022, 0, "worker-writeable mirror path {path:?}");
            assert_eq!(mode & 0o044, 0o044, "unreadable mirror path {path:?}");
            if metadata.is_dir() {
                assert_eq!(mode & 0o011, 0o011, "unsearchable mirror path {path:?}");
                pending.extend(children(&path));
            }
        }
    }

    /// The paths directly inside `dir`, which is named if it cannot be listed.
    fn children(dir: &Path) -> Vec<std::path::PathBuf> {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("cannot list mirror path {dir:?}: {error}"));
        entries.map(|entry| entry.unwrap().path()).collect()
    }

    #[test]
    fn canonical_origin_keeps_push_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let (url, head) = remote(dir.path());
        let dest = dir.path().join("clone");
        create(&url, None, &head, &dest).unwrap();
        set_origin(&dest, "https://github.com/example/repo.git").unwrap();
        let fetch = text(git(Some(&dest), &["remote", "get-url", "origin"]).unwrap()).unwrap();
        assert_eq!(fetch, "https://github.com/example/repo.git");
        assert!(hardening_problems(&dest).unwrap().is_empty());
    }

    #[test]
    fn the_identity_lets_a_worktree_commit_and_the_checkout_dir_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (url, head) = remote(dir.path());
        let dest = dir.path().join("clone");
        create(&url, None, &head, &dest).unwrap();
        assert!(set_identity(&dest, "-x", "a@b.invalid").is_err());
        assert!(set_identity(&dest, "n", "a\nb").is_err());
        set_identity(&dest, "agentc implementer", "agentc-impl@agentc.invalid").unwrap();
        let work = dest.join(CHECKOUT_DIR);
        let path = work.display().to_string();
        run(
            Some(&dest),
            &["worktree", "add", "-q", "-b", "w", &path, &head],
        )
        .unwrap();
        fs::write(work.join("new.txt"), "x").unwrap();
        run(Some(&work), &["add", "new.txt"]).unwrap();
        // Only the clone's own identity: no global, system or env identity.
        let mut commit = Command::new("git");
        commit.args(["-C", &path, "commit", "-q", "-m", "m"]);
        commit
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for name in
            ["NAME", "EMAIL"].map(|n| ["AUTHOR", "COMMITTER"].map(|r| format!("GIT_{r}_{n}")))
        {
            commit.env_remove(&name[0]).env_remove(&name[1]);
        }
        commit.env_remove("EMAIL");
        let committed = commit.output().unwrap();
        assert!(committed.status.success(), "{committed:?}");
        let author = text(run(Some(&work), &["log", "-1", "--format=%an <%ae>"]).unwrap());
        let author = author.unwrap();
        assert_eq!(author, "agentc implementer <agentc-impl@agentc.invalid>");
        let status = text(run(Some(&dest), &["status", "--porcelain"]).unwrap()).unwrap();
        assert_eq!(status, "");
    }

    #[test]
    fn tampered_clone_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (url, head) = remote(dir.path());
        let dest = dir.path().join("clone");
        create(&url, None, &head, &dest).unwrap();
        run(
            Some(&dest),
            &["config", "--local", "core.hooksPath", ".githooks"],
        )
        .unwrap();
        assert_eq!(hardening_problems(&dest).unwrap().len(), 1);
    }

    /// A mirror of the remote at `url` whose next fetch always has Git's
    /// automatic maintenance to run: the fetch keeps its pack (one pack more
    /// than `gc.autoPackLimit`), so the maintenance repacks. This is the
    /// repack that removes files a clone of the mirror may have listed.
    fn mirror_with_due_maintenance(dir: &Path, url: &str) -> std::path::PathBuf {
        let mirror = dir.join("mirror.git");
        run(
            None,
            &[
                "clone",
                "--mirror",
                "--quiet",
                url,
                mirror.to_str().unwrap(),
            ],
        )
        .unwrap();
        for (key, value) in [
            ("gc.auto", "1"),
            ("gc.autoPackLimit", "1"),
            ("fetch.unpackLimit", "1"),
        ] {
            run(Some(&mirror), &["config", key, value]).unwrap();
        }
        mirror
    }

    /// Commits to the remote's work tree.
    fn commit_to(work: &Path, message: &str) {
        run(
            Some(work),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        )
        .unwrap();
    }

    /// The pids of Git maintenance processes (`git maintenance run ...`, the
    /// detached one a fetch leaves behind on Git 2.47 and later) working on
    /// the repository at `repo`. The maintenance of other tests running in
    /// parallel is not counted: a maintenance process is this repository's
    /// when its working directory is `repo` or it holds a file under `repo`
    /// open (its `objects/maintenance.lock`).
    fn git_maintenance_processes(repo: &Path) -> Vec<String> {
        let repo = repo.canonicalize().unwrap();
        let mut found = Vec::new();
        for entry in fs::read_dir("/proc").unwrap().flatten() {
            let proc = entry.path();
            let Ok(cmdline) = fs::read(proc.join("cmdline")) else {
                continue;
            };
            let words: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
            let is_git = words[0].rsplit(|b| *b == b'/').next() == Some(b"git");
            if !is_git || !words.contains(&&b"maintenance"[..]) {
                continue;
            }
            let in_repo = fs::read_link(proc.join("cwd")).is_ok_and(|cwd| cwd.starts_with(&repo))
                || fs::read_dir(proc.join("fd")).is_ok_and(|fds| {
                    fds.flatten().any(|fd| {
                        fs::read_link(fd.path()).is_ok_and(|target| target.starts_with(&repo))
                    })
                });
            if in_repo {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        found
    }

    #[test]
    fn mirror_fetch_leaves_no_detached_maintenance_running() {
        if !Path::new("/proc/self/cmdline").exists() {
            eprintln!("note: skipping mirror fetch maintenance check: no /proc");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (url, _) = remote(dir.path());
        let mirror = mirror_with_due_maintenance(dir.path(), &url);
        // Detaching is racy, so look after several fetches. A Git that
        // detaches by default (2.47 or later) leaves its maintenance running
        // after every one of these without the foreground setting; an older
        // Git runs it in the foreground and passes either way.
        const ROUNDS: usize = 5;
        for round in 0..ROUNDS {
            commit_to(Path::new(&url), &format!("round {round}"));
            git_output(&mirror, &["fetch", "--prune", "--quiet"]).unwrap();
            assert_eq!(
                git_maintenance_processes(&mirror),
                Vec::<String>::new(),
                "maintenance outlived the fetch in round {round}"
            );
        }
        // The maintenance did run, and a clone of the mirror right after is
        // whole. Every fetch keeps its pack (`fetch.unpackLimit=1`), so a
        // mirror whose maintenance never ran has the clone's pack and one per
        // round. How far a repack merges them differs between Git versions
        // (one pack on 2.53, three on 2.56), so only require that it merged.
        let packs = fs::read_dir(mirror.join("objects/pack"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
            .count();
        assert!(
            packs < ROUNDS + 1,
            "the fetch's maintenance did not repack: {packs} packs"
        );
        let clone = dir.path().join("clone");
        run(
            None,
            &[
                "clone",
                "--no-hardlinks",
                "--quiet",
                mirror.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        )
        .unwrap();
        run(Some(&clone), &["fsck", "--connectivity-only"]).unwrap();
    }

    #[test]
    fn mirror_fetch_keeps_updated_and_new_refs_readable_without_worker_writes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (url, _) = remote(dir.path());
        // The fetch's own maintenance repacks, in the foreground (see
        // `git_output`), so the objects checked below include what maintenance
        // wrote and no maintenance is left running to change them during the walk.
        let mirror = mirror_with_due_maintenance(dir.path(), &url);
        // This enforces restrictive creation modes independent of the test
        // runner's umask, without mutating process-global state in parallel
        // tests. The production failure is the equivalent umask 0077 case.
        run(Some(&mirror), &["config", "core.sharedRepository", "0600"]).unwrap();
        let work = Path::new(&url);
        fs::write(work.join("a.txt"), "new mirror content\n").unwrap();
        run(
            Some(work),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qam",
                "update",
            ],
        )
        .unwrap();
        run(Some(work), &["branch", "ac/results/new-result"]).unwrap();
        let head = text(run(Some(work), &["rev-parse", "HEAD"]).unwrap()).unwrap();
        git_output(&mirror, &["fetch", "--prune", "--quiet"]).unwrap();

        for name in ["refs/heads/main", "refs/heads/ac/results/new-result"] {
            let path = mirror.join(name);
            assert_eq!(fs::read_to_string(&path).unwrap().trim(), head);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        for name in ["refs/heads/ac", "refs/heads/ac/results"] {
            assert_eq!(
                fs::metadata(mirror.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        assert_readable_without_worker_writes(&mirror.join("objects"));
        // Invocation-only: do not change the repository's persisted settings.
        assert_eq!(
            text(
                run(
                    Some(&mirror),
                    &["config", "--local", "--get", "core.sharedRepository"]
                )
                .unwrap()
            )
            .unwrap(),
            "0600"
        );
    }
}
