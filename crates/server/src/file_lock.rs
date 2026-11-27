//! The guard for an advisory lock on a lock file.
use std::fs::File;

/// Holds a lock file's shared or exclusive advisory lock until dropped.
pub(crate) struct HeldLock {
    file: File,
}

impl HeldLock {
    /// Takes over a file whose lock the caller has just acquired.
    pub(crate) fn holding(file: File) -> Self {
        Self { file }
    }

    /// Duplicates the locked descriptor, standing in for a child forked while
    /// the lock was held.
    #[cfg(test)]
    pub(crate) fn forked_child_descriptor(&self) -> File {
        self.file.try_clone().unwrap()
    }
}

impl Drop for HeldLock {
    /// Unlocks explicitly: closing the descriptor alone keeps a `flock` held
    /// while any other process still has a duplicate of the open file
    /// description, as a forked child does until its exec.
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
