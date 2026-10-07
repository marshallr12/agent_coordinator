//! Process hygiene for a launch (R-P3b.5(b)): every harness runs with
//! `no_new_privs`, and the `launch` command adopts the processes a harness
//! leaves behind (double forks, `setsid` daemons) and kills them once the
//! harness exits. Claude launches already get both from Bubblewrap (its own
//! PID namespace, `--die-with-parent`); this covers Codex, which runs in the
//! host namespace.
//!
//! Every Bubblewrap run (each sandbox probe, the project setup and the
//! harness) usually leaves one process here: Bubblewrap exits as soon as its
//! PID-namespace init reports the command's status, without reaping that
//! init, which re-parents to the launch as an exited zombie. The kernel has
//! already killed everything inside the namespace by then, so the cleanup
//! only reaps these; its report tells them apart from live processes killed.

use anyhow::Result;
use std::process::Command;

/// How long [`kill_leftovers`] keeps killing a tree that keeps forking.
#[cfg(target_os = "linux")]
const KILL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Sets `no_new_privs` in the child before exec, so no setuid/setgid binary
/// or file capability can raise a harness's privileges.
#[cfg(target_os = "linux")]
pub fn forbid_new_privileges(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure allocates nothing, takes no locks and only calls
    // prctl and reads errno, which is async-signal-safe after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `no_new_privs` is a Linux concept; elsewhere there is nothing to set.
#[cfg(not(target_os = "linux"))]
pub fn forbid_new_privileges(_command: &mut Command) {}

/// Makes this process the reaper of its orphaned descendants, so detached
/// harness processes re-parent here rather than to init. Call it only in a
/// process whose every child belongs to the launch (the `launch` command),
/// since [`kill_leftovers`] kills all of them.
#[cfg(target_os = "linux")]
pub fn adopt_orphans() -> Result<()> {
    // SAFETY: prctl with integer arguments has no memory-safety contract.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1 as libc::c_ulong, 0, 0, 0) } != 0 {
        anyhow::bail!(
            "become the launch's child subreaper: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Elsewhere orphans cannot be adopted; leftovers are not tracked.
#[cfg(not(target_os = "linux"))]
pub fn adopt_orphans() -> Result<()> {
    Ok(())
}

/// Runs `body` as the subreaper of everything it starts. `body` gets the
/// cleanup to call once its harness has exited; the cleanup runs again when
/// `body` fails, so a launch refused before or after its harness leaks
/// nothing it already started (a preflight probe's detached grandchild).
pub fn reaped<T>(body: impl FnOnce(fn()) -> Result<T>) -> Result<T> {
    adopt_orphans()?;
    let result = body(kill_and_report);
    if result.is_err() {
        kill_and_report();
    }
    result
}

/// One process [`kill_leftovers`] found below the launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    /// The command name from `/proc/<pid>/stat` (`?` when unreadable).
    pub name: String,
    /// Whether it had already exited, an unreaped zombie, when first seen;
    /// such a process was only reaped, not killed.
    pub exited: bool,
}

/// SIGKILLs every descendant of this process and reaps them (see
/// [`kill_rounds`]); returns each distinct leftover found.
pub fn kill_leftovers() -> Result<Vec<Leftover>> {
    kill_rounds().map(|(found, _)| found)
}

/// SIGKILLs every descendant of this process at once (so neither tree depth
/// nor a ptrace-attached leftover can stall it) and reaps without blocking,
/// round after round until no descendant or child is left. Returns the
/// distinct leftover processes, unreaped zombies included, as first seen,
/// and how many rounds that took.
#[cfg(target_os = "linux")]
fn kill_rounds() -> Result<(Vec<Leftover>, usize)> {
    let deadline = std::time::Instant::now() + KILL_DEADLINE;
    let mut found = std::collections::BTreeMap::new();
    for round in 1.. {
        let tree = descendants()?;
        for &pid in &tree {
            found.entry(pid).or_insert_with(|| observe(pid));
            kill_descendant(pid, &tree);
        }
        if !reap_ready() && tree.is_empty() {
            return Ok((found.into_values().collect(), round));
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("launch processes survived {KILL_DEADLINE:?} of kill rounds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unreachable!("the round counter cannot run out")
}

/// Elsewhere leftovers are not tracked.
#[cfg(not(target_os = "linux"))]
fn kill_rounds() -> Result<(Vec<Leftover>, usize)> {
    Ok((Vec::new(), 0))
}

/// Runs [`kill_leftovers`] and reports on standard error what it killed and
/// reaped, or why it could not finish; the launch's outcome stands either way.
pub fn kill_and_report() {
    match kill_leftovers() {
        Ok(found) => report(&found)
            .into_iter()
            .for_each(|line| eprintln!("{line}")),
        Err(error) => eprintln!("agentc-supervisor: warning: {error:#}"),
    }
}

/// The informational line for `found`, or `None` when nothing was left:
/// still-running processes that were killed, then already-exited ones that
/// were only reaped (each Bubblewrap sandbox leaves its exited PID-namespace
/// init behind), each group summarised by command name.
fn report(found: &[Leftover]) -> Option<String> {
    let (exited, running): (Vec<_>, Vec<_>) = found.iter().partition(|l| l.exited);
    let mut parts = Vec::new();
    if !running.is_empty() {
        let names = summarise(&running);
        parts.push(format!(
            "killed {} leftover launch processes ({names})",
            running.len()
        ));
    }
    if !exited.is_empty() {
        let names = summarise(&exited);
        parts.push(format!(
            "reaped {} exited launch processes ({names})",
            exited.len()
        ));
    }
    (!parts.is_empty()).then(|| format!("agentc-supervisor: {}", parts.join("; ")))
}

/// `name` or `name xN` for each distinct command name, in name order.
fn summarise(leftovers: &[&Leftover]) -> String {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for leftover in leftovers {
        *counts.entry(leftover.name.as_str()).or_default() += 1;
    }
    let shown = counts.iter().map(|(name, count)| match count {
        1 => (*name).to_owned(),
        _ => format!("{name} x{count}"),
    });
    shown.collect::<Vec<_>>().join(", ")
}

/// The command name and exited state of `pid` from `/proc/<pid>/stat`; a
/// process that vanished first counts as exited with name `?`.
#[cfg(target_os = "linux")]
fn observe(pid: libc::pid_t) -> Leftover {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let name = stat
        .split_once('(')
        .and_then(|(_, rest)| rest.rsplit_once(')'));
    let state = name.and_then(|(_, rest)| rest.split_whitespace().next());
    Leftover {
        name: name.map_or("?", |(name, _)| name).to_owned(),
        exited: state.is_none_or(|state| matches!(state, "Z" | "X")),
    }
}

/// SIGKILLs `pid` through a pidfd, once its parent is confirmed to be this
/// process or a member of `tree`, so a recycled pid is never signalled.
/// A process that is already gone is skipped.
#[cfg(target_os = "linux")]
fn kill_descendant(pid: libc::pid_t, tree: &[libc::pid_t]) {
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let Ok(fd) = libc::c_int::try_from(fd) else {
        return;
    };
    if fd < 0 {
        return;
    }
    let me = std::process::id().to_string();
    let parent = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
    let parent = parent.as_deref().and_then(parent_of);
    if parent.is_some_and(|p| p == me || p.parse().is_ok_and(|p| tree.contains(&p))) {
        // SAFETY: fd is the pidfd opened above; the other arguments are null.
        unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd, libc::SIGKILL, 0, 0) };
    }
    // SAFETY: fd is owned here and closed exactly once.
    unsafe { libc::close(fd) };
}

/// Reaps every child that has already exited; returns whether this process
/// still has children (running, or exited since).
#[cfg(target_os = "linux")]
fn reap_ready() -> bool {
    loop {
        // SAFETY: a null status pointer is allowed; WNOHANG never blocks.
        match unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } {
            0 => return true,
            pid if pid > 0 => continue,
            _ => return std::io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD),
        }
    }
}

/// Every descendant of this process, parents before children, from the
/// parent field of each `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
fn descendants() -> Result<Vec<libc::pid_t>> {
    let parents = parent_links()?;
    let mut tree = vec![libc::pid_t::try_from(std::process::id())?];
    let mut next = 0;
    while next < tree.len() {
        let parent = tree[next];
        tree.extend(
            parents
                .iter()
                .filter(|(_, p)| *p == parent)
                .map(|(c, _)| *c),
        );
        next += 1;
    }
    tree.remove(0);
    Ok(tree)
}

/// `(pid, parent pid)` for every process readable in `/proc`; processes that
/// vanish while being read are skipped.
#[cfg(target_os = "linux")]
fn parent_links() -> Result<Vec<(libc::pid_t, libc::pid_t)>> {
    let mut links = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let name = entry?.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some(parent) = parent_of(&stat).and_then(|p| p.parse().ok()) {
            links.push((pid, parent));
        }
    }
    Ok(links)
}

/// The parent pid field of a `/proc/<pid>/stat` line. The command name may
/// hold spaces or parentheses, so fields are counted after its last `)`.
#[cfg(target_os = "linux")]
fn parent_of(stat: &str) -> Option<&str> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
