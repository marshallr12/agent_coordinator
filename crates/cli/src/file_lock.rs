//! The guard for an exclusive advisory lock on a lock file.
use std::fs::File;

/// Holds a lock file's exclusive advisory lock until dropped.
pub struct HeldLock {
    file: File,
}

impl HeldLock {
    /// Takes over a file whose exclusive lock the caller has just acquired.
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
