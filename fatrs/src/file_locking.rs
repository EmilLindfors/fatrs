//! File-level locking for concurrent access control.
//!
//! This module provides shared (read) and exclusive (write) locks for files,
//! enabling safe concurrent access from multiple tasks or cores.
//!
//! # Lock Types
//!
//! - **Shared locks** allow multiple concurrent readers
//! - **Exclusive locks** allow a single writer with no readers
//!
//! # Example
//!
//! ```rust,ignore
//! // Multiple readers can access concurrently
//! let file1 = fs.root_dir().open_file("data.txt").await?; // Shared lock
//! let file2 = fs.root_dir().open_file("data.txt").await?; // Also shared - OK!
//!
//! // Writer needs exclusive access
//! let file3 = fs.root_dir().create_file("log.txt").await?; // Exclusive lock
//! // Another open for write would fail with Error::FileLocked
//! ```

#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::collections::BTreeMap;
#[cfg(feature = "std")]
use std::collections::BTreeMap;

use core::fmt::Debug;

/// Lock type for file access control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockType {
    /// Shared lock - allows multiple concurrent readers.
    /// Used when opening files for read-only access.
    Shared,
    /// Exclusive lock - single writer, no readers allowed.
    /// Used when opening files for write access.
    Exclusive,
}

/// State of a file lock.
#[derive(Clone, Debug, Default)]
pub struct FileLockState {
    /// Number of shared readers (0 if exclusive lock held).
    readers: u32,
    /// True if exclusive lock is held.
    exclusive: bool,
}

impl FileLockState {
    /// Create a new lock state with a shared reader.
    fn new_shared() -> Self {
        Self {
            readers: 1,
            exclusive: false,
        }
    }

    /// Create a new lock state with an exclusive writer.
    fn new_exclusive() -> Self {
        Self {
            readers: 0,
            exclusive: true,
        }
    }

    /// Check if this lock state is empty (no locks held).
    fn is_empty(&self) -> bool {
        self.readers == 0 && !self.exclusive
    }
}

/// File lock manager for tracking locks across all open files.
///
/// Uses the file's first cluster as the key for identifying files.
/// This works because each file has a unique first cluster.
#[derive(Debug)]
pub struct FileLockManager {
    /// Maps first_cluster -> lock state.
    /// Using BTreeMap for no_std compatibility (works with alloc, no HashMap needed).
    locks: BTreeMap<u32, FileLockState>,
}

impl FileLockManager {
    /// Create a new empty file lock manager.
    pub fn new() -> Self {
        Self {
            locks: BTreeMap::new(),
        }
    }

    /// Attempt to acquire a lock on a file.
    ///
    /// # Arguments
    ///
    /// * `cluster` - The first cluster of the file (unique identifier)
    /// * `lock_type` - Whether to acquire a shared or exclusive lock
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the lock was acquired
    /// * `Err(())` if the lock could not be acquired (file is locked)
    ///
    /// # Lock Compatibility
    ///
    /// | Requested | Current State | Result |
    /// |-----------|---------------|--------|
    /// | Shared    | No locks      | OK     |
    /// | Shared    | Shared(n)     | OK     |
    /// | Shared    | Exclusive     | FAIL   |
    /// | Exclusive | No locks      | OK     |
    /// | Exclusive | Shared(n)     | FAIL   |
    /// | Exclusive | Exclusive     | FAIL   |
    pub fn try_lock(&mut self, cluster: u32, lock_type: LockType) -> Result<(), ()> {
        match self.locks.get_mut(&cluster) {
            Some(state) => {
                match lock_type {
                    LockType::Shared => {
                        if state.exclusive {
                            // Can't read while someone is writing
                            return Err(());
                        }
                        state.readers += 1;
                    }
                    LockType::Exclusive => {
                        if state.exclusive || state.readers > 0 {
                            // Can't write while someone is reading or writing
                            return Err(());
                        }
                        state.exclusive = true;
                    }
                }
            }
            None => {
                // No existing lock, create new one
                let state = match lock_type {
                    LockType::Shared => FileLockState::new_shared(),
                    LockType::Exclusive => FileLockState::new_exclusive(),
                };
                self.locks.insert(cluster, state);
            }
        }
        Ok(())
    }

    /// Release a lock on a file.
    ///
    /// # Arguments
    ///
    /// * `cluster` - The first cluster of the file
    /// * `lock_type` - The type of lock to release (must match what was acquired)
    ///
    /// # Panics
    ///
    /// In debug builds, panics if trying to release a lock that wasn't held.
    pub fn unlock(&mut self, cluster: u32, lock_type: LockType) {
        if let Some(state) = self.locks.get_mut(&cluster) {
            match lock_type {
                LockType::Shared => {
                    debug_assert!(state.readers > 0, "Tried to unlock shared lock that wasn't held");
                    state.readers = state.readers.saturating_sub(1);
                }
                LockType::Exclusive => {
                    debug_assert!(state.exclusive, "Tried to unlock exclusive lock that wasn't held");
                    state.exclusive = false;
                }
            }
            // Remove entry if no locks held (cleanup)
            if state.is_empty() {
                self.locks.remove(&cluster);
            }
        } else {
            debug_assert!(false, "Tried to unlock file that has no lock entry");
        }
    }

    /// Check if a file is currently locked.
    ///
    /// # Arguments
    ///
    /// * `cluster` - The first cluster of the file
    ///
    /// # Returns
    ///
    /// `true` if the file has any active locks (shared or exclusive)
    pub fn is_locked(&self, cluster: u32) -> bool {
        self.locks.get(&cluster).is_some_and(|s| !s.is_empty())
    }

    /// Get the current lock state for a file.
    ///
    /// # Arguments
    ///
    /// * `cluster` - The first cluster of the file
    ///
    /// # Returns
    ///
    /// The current lock state, or `None` if no locks are held
    pub fn get_lock_state(&self, cluster: u32) -> Option<&FileLockState> {
        self.locks.get(&cluster)
    }

    /// Get the number of currently locked files.
    pub fn locked_file_count(&self) -> usize {
        self.locks.len()
    }
}

impl Default for FileLockManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_locks_allow_multiple_readers() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Multiple shared locks should succeed
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());

        // Verify state
        let state = manager.get_lock_state(cluster).unwrap();
        assert_eq!(state.readers, 3);
        assert!(!state.exclusive);
    }

    #[test]
    fn test_exclusive_lock_blocks_shared() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire exclusive lock
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_ok());

        // Shared lock should fail
        assert!(manager.try_lock(cluster, LockType::Shared).is_err());
    }

    #[test]
    fn test_shared_lock_blocks_exclusive() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire shared lock
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());

        // Exclusive lock should fail
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_err());
    }

    #[test]
    fn test_exclusive_lock_blocks_exclusive() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire exclusive lock
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_ok());

        // Another exclusive lock should fail
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_err());
    }

    #[test]
    fn test_unlock_shared() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire two shared locks
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());

        // Release one
        manager.unlock(cluster, LockType::Shared);
        assert_eq!(manager.get_lock_state(cluster).unwrap().readers, 1);

        // Exclusive should still fail
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_err());

        // Release the last one
        manager.unlock(cluster, LockType::Shared);

        // Now exclusive should succeed
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_ok());
    }

    #[test]
    fn test_unlock_exclusive() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire exclusive lock
        assert!(manager.try_lock(cluster, LockType::Exclusive).is_ok());

        // Release it
        manager.unlock(cluster, LockType::Exclusive);

        // Entry should be removed
        assert!(!manager.is_locked(cluster));

        // Now shared should succeed
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());
    }

    #[test]
    fn test_different_files_independent() {
        let mut manager = FileLockManager::new();
        let cluster1 = 100;
        let cluster2 = 200;

        // Lock file 1 exclusively
        assert!(manager.try_lock(cluster1, LockType::Exclusive).is_ok());

        // File 2 should be independent
        assert!(manager.try_lock(cluster2, LockType::Exclusive).is_ok());
        assert!(manager.try_lock(cluster2, LockType::Shared).is_err()); // blocked by its own exclusive

        // Verify count
        assert_eq!(manager.locked_file_count(), 2);
    }

    #[test]
    fn test_cleanup_on_unlock() {
        let mut manager = FileLockManager::new();
        let cluster = 100;

        // Acquire and release
        assert!(manager.try_lock(cluster, LockType::Shared).is_ok());
        assert_eq!(manager.locked_file_count(), 1);

        manager.unlock(cluster, LockType::Shared);
        assert_eq!(manager.locked_file_count(), 0);
        assert!(!manager.is_locked(cluster));
    }
}
