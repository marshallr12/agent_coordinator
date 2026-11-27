//! The guard for an advisory lock on a lock file.
use std::fs::File;

/// Holds a lock file's advisory lock until dropped.
pub struct HeldLock {
    file: File,
}

impl HeldLock {
    /// Takes over a file whose lock the caller has just acquired.
    pub fn holding(file: File) -> Self {
        Self { file }
    }

    /// Duplicates the locked descriptor, standing in for a child that another
    /// thread forked while the lock was held.
    #[cfg(test)]
    pub fn forked_child_descriptor(&self) -> File {
        self.file.try_clone().unwrap()
    }
}

impl Drop for HeldLock {
    /// Unlocks explicitly: closing the descriptor alone keeps a `flock` held
    /// while any other process still has a duplicate of the open file
    /// description, as a child forked by another thread does until its exec.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_guard_releases_the_lock_while_a_forked_child_still_holds_the_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lock");
        let file = File::create(&path).unwrap();
        file.try_lock().unwrap();
        let held = HeldLock::holding(file);
        let forked_child = held.forked_child_descriptor();
        drop(held);
        File::open(&path).unwrap().try_lock().unwrap();
        drop(forked_child);
    }
}
