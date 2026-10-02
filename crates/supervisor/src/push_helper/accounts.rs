//! The Unix accounts `launch-root` starts its children as, and the one place
//! that moves a child to another account.
use anyhow::{Context, Result, ensure};
use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// The most supplementary groups looked up for one account.
const MAX_GROUPS: libc::c_int = 65_536;

/// A resolved account: its user id, primary group id and every group the
/// user database gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
}

impl Account {
    /// Looks `name` up in the user database. An account with root's user id
    /// is refused: these accounts exist to hold less than root does.
    pub fn lookup(name: &str) -> Result<Self> {
        let c_name = CString::new(name).context("account names contain no NUL")?;
        let (uid, gid) = passwd_ids(&c_name, name)?;
        ensure!(
            uid != 0,
            "account {name:?} is root; it must be unprivileged"
        );
        let groups = group_list(&c_name, gid).with_context(|| format!("groups of {name:?}"))?;
        Ok(Self { uid, gid, groups })
    }

    /// This process's own real user and group, for test runs that switch
    /// nothing.
    #[cfg(test)]
    pub fn current() -> Self {
        // SAFETY: getuid and getgid take no arguments, cannot fail and touch
        // no memory.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Self {
            uid,
            gid,
            groups: vec![gid],
        }
    }
}

/// This process's effective user id.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// Refuses unless this process runs with effective user id 0: only root can
/// start the helper and the launch as their own accounts.
pub fn require_root(euid: u32) -> Result<()> {
    ensure!(
        euid == 0,
        "launch-root must run as root: it starts the push helper and the launch as their own accounts"
    );
    Ok(())
}

/// The user and primary group ids of the account `name`.
fn passwd_ids(c_name: &CString, name: &str) -> Result<(u32, u32)> {
    // SAFETY: passwd is plain old data; all-zero is a valid value to overwrite.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live storage owned by this frame, and
    // `buffer.len()` is the capacity getpwnam_r may write into.
    let status = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    ensure!(
        status == 0,
        "look up account {name:?}: {}",
        io::Error::from_raw_os_error(status)
    );
    ensure!(!found.is_null(), "account {name:?} does not exist");
    Ok((entry.pw_uid, entry.pw_gid))
}

/// Every group of the account `c_name` whose primary group is `gid`, as
/// `initgroups` would set them.
fn group_list(c_name: &CString, gid: u32) -> Result<Vec<u32>> {
    let mut capacity: libc::c_int = 64;
    loop {
        let mut groups = vec![0 as libc::gid_t; usize::try_from(capacity)?];
        let mut count = capacity;
        // SAFETY: `groups` holds `count` writable entries; getgrouplist writes
        // at most that many and stores the number it needs in `count`.
        let status =
            unsafe { libc::getgrouplist(c_name.as_ptr(), gid, groups.as_mut_ptr(), &mut count) };
        if status >= 0 {
            groups.truncate(usize::try_from(count)?);
            return Ok(groups);
        }
        ensure!(
            count > capacity && count <= MAX_GROUPS,
            "the group list cannot be read"
        );
        capacity = count;
    }
}

/// Makes `command` run as `account`: in the child, after fork and before
/// exec, it sets the supplementary groups, then the group, then the user, and
/// fails the spawn unless the switch took. Needs root.
pub fn run_as(command: &mut Command, account: &Account) {
    let account = account.clone();
    // SAFETY: the closure only makes async-signal-safe system calls on data
    // captured before the fork, and allocates nothing.
    unsafe {
        command.pre_exec(move || become_account(&account));
    }
}

/// Switches this (child) process to `account` for good.
fn become_account(account: &Account) -> io::Result<()> {
    let groups = &account.groups;
    // SAFETY: setgroups reads `groups.len()` ids from a live slice; the other
    // calls take plain integers or nothing and touch no memory.
    unsafe {
        if libc::setgroups(groups.len(), groups.as_ptr()) != 0
            || libc::setgid(account.gid) != 0
            || libc::setuid(account.uid) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let switched = libc::getuid() == account.uid
            && libc::geteuid() == account.uid
            && libc::getgid() == account.gid
            && libc::getegid() == account.gid;
        if !switched || libc::setuid(0) == 0 {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
    }
    Ok(())
}
