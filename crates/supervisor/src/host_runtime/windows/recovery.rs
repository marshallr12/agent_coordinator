//! Judging a launch from its saved identities, without starting anything.
//!
//! A Windows process is identified for good by its pid together with its
//! creation time. A launch's job dies with its last handle, and only the
//! launcher process that made it holds one. So a launch has ended when
//! Windows says both that its job's owner is gone and that its root process
//! is gone; in every other case, including every case Windows will not
//! answer, it may still run and keeps what it holds.
use super::launcher::{ended_within, open_process};
use super::process::creation_time;
use crate::host_runtime::Recovery;

/// What Windows says about a pid now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// A running process with this creation time has the pid.
    Created(u64),
    /// No running process has the pid.
    Gone,
    /// Windows would not say.
    Unknown,
}

/// Whether an identified process is the one that still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alive {
    Yes,
    No,
    Unknown,
}

/// Windows error 87: no process has that pid.
const INVALID_PARAMETER: &str = "Windows error 87";

/// Looks `pid` up: its creation time when it runs.
pub fn lookup(pid: u32) -> Lookup {
    let process = match open_process(pid) {
        Ok(process) => process,
        Err(error) if error.to_string().ends_with(INVALID_PARAMETER) => return Lookup::Gone,
        Err(_) => return Lookup::Unknown,
    };
    match ended_within(process.raw(), 0) {
        Ok(true) => Lookup::Gone,
        Ok(false) => creation_time(process.raw()).map_or(Lookup::Unknown, Lookup::Created),
        Err(_) => Lookup::Unknown,
    }
}

/// The pid and creation time in a `runner:<pid>:<creation time>` job owner.
pub fn parse_owner(owner: &str) -> Option<(u32, u64)> {
    let mut parts = owner.strip_prefix("runner:")?.split(':');
    let identity = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(identity)
}

/// Whether the process `pid` created at `created` is the one running.
fn alive(pid: u32, created: u64, lookup: &impl Fn(u32) -> Lookup) -> Alive {
    match lookup(pid) {
        Lookup::Created(now) if now == created => Alive::Yes,
        Lookup::Created(_) | Lookup::Gone => Alive::No,
        Lookup::Unknown => Alive::Unknown,
    }
}

/// Judges a launch whose root process is `root` (pid, creation time) if it
/// was started, and whose jobs belong to `owner`.
pub fn judge(
    root: Option<(u32, Option<u64>)>,
    owner: Option<&str>,
    lookup: impl Fn(u32) -> Lookup,
) -> Recovery {
    let owner_alive = match owner.and_then(parse_owner) {
        Some((pid, created)) => alive(pid, created, &lookup),
        None => Alive::Unknown,
    };
    let root_alive = match root {
        Some((pid, Some(created))) => alive(pid, created, &lookup),
        Some((_, None)) => Alive::Unknown,
        None => Alive::No,
    };
    if owner_alive == Alive::No && root_alive == Alive::No {
        Recovery::Ended
    } else {
        Recovery::MayBeRunning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup in which only the listed (pid, creation time) pairs run and
    /// pid 13 cannot be queried.
    fn table(running: &'static [(u32, u64)]) -> impl Fn(u32) -> Lookup {
        move |pid| {
            if pid == 13 {
                return Lookup::Unknown;
            }
            let found = running.iter().find(|(p, _)| *p == pid);
            found.map_or(Lookup::Gone, |(_, created)| Lookup::Created(*created))
        }
    }

    #[test]
    fn a_launch_has_ended_only_when_its_owner_is_gone_and_its_root_is_not_alive() {
        let owner = Some("runner:10:500");
        let root = Some((20, Some(700)));
        let ended = judge(root, owner, table(&[]));
        assert_eq!(ended, Recovery::Ended);
        // The pid was reused by a later process: still not this launch.
        assert_eq!(judge(root, owner, table(&[(20, 701)])), Recovery::Ended);
        assert_eq!(judge(None, owner, table(&[(10, 501)])), Recovery::Ended);
    }

    #[test]
    fn a_live_owner_or_root_keeps_the_launch() {
        let owner = Some("runner:10:500");
        let root = Some((20, Some(700)));
        for running in [&[(10, 500)][..], &[(20, 700)], &[(10, 500), (20, 700)]] {
            let running: &'static [(u32, u64)] = Box::leak(running.to_vec().into_boxed_slice());
            assert_eq!(judge(root, owner, table(running)), Recovery::MayBeRunning);
        }
        // Prepared and never started, but its owner still runs.
        assert_eq!(
            judge(None, owner, table(&[(10, 500)])),
            Recovery::MayBeRunning
        );
    }

    #[test]
    fn what_windows_will_not_answer_is_never_taken_for_an_end() {
        let root = Some((20, Some(700)));
        assert_eq!(judge(root, None, table(&[])), Recovery::MayBeRunning);
        assert_eq!(
            judge(root, Some("garbage"), table(&[])),
            Recovery::MayBeRunning
        );
        assert_eq!(
            judge(root, Some("runner:13:1"), table(&[])),
            Recovery::MayBeRunning
        );
        assert_eq!(
            judge(Some((20, None)), Some("runner:10:500"), table(&[(20, 1)])),
            Recovery::MayBeRunning
        );
    }

    #[test]
    fn a_job_owner_names_one_pid_and_one_creation_time() {
        assert_eq!(parse_owner("runner:10:500"), Some((10, 500)));
        for bad in [
            "",
            "runner:",
            "runner:10",
            "runner:10:x",
            "runner:10:5:6",
            "10:500",
        ] {
            assert_eq!(parse_owner(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn this_process_is_found_running_and_an_unused_pid_is_gone() {
        let me = lookup(std::process::id());
        assert!(
            matches!(me, Lookup::Created(created) if created > 0),
            "{me:?}"
        );
        // Pids are multiples of four; an odd, huge one names no process.
        assert_eq!(lookup(0xFFFF_FFF1), Lookup::Gone);
    }
}
