// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use crate::{fs::fs_node::*, types::*};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileWriteGuardMode {
    // File open for write.
    WriteFile,

    // Writable mapping.
    WriteMapping,

    // Mapped for execution.
    Exec,
}

// Tracks FileWriteGuard state for FsNode instances. Used to implement executable write blocking
// (see `ETXTBSY`) and file seals (see `memfd_create`).
// Note that this is not related to the `flock` file state, which is stored in `FlockInfo`.
#[derive(Default)]
pub struct FileWriteGuardState {
    // Positive values indicate number of write locks, negative - execution locks.
    // This implies that write and exec locks cannot be held similtaneously.
    write_exec_locks: isize,

    // Number of WriteMapping guards.
    num_write_mappings: usize,

    // Seals are not allowed by default.
    seals: Option<SealFlags>,
}

impl FileWriteGuardState {
    pub fn create_write_guard(
        &mut self,
        fs_node: FsNodeHandle,
        mode: FileWriteGuardMode,
    ) -> Result<FileWriteGuard, Errno> {
        match mode {
            FileWriteGuardMode::WriteFile => {
                if self.write_exec_locks < 0 {
                    return error!(ETXTBSY);
                }

                // Do not check write seals: file with a write seals can be opened,
                // but `write()` will fail.

                self.write_exec_locks += 1;
            }
            FileWriteGuardMode::WriteMapping => {
                self.check_no_seal(SealFlags::WRITE | SealFlags::FUTURE_WRITE)?;

                // File must be open for write in order to be mapped.
                assert!(self.write_exec_locks > 0);

                self.write_exec_locks += 1;
                self.num_write_mappings += 1;
            }
            FileWriteGuardMode::Exec => {
                if self.write_exec_locks > 0 {
                    return error!(ETXTBSY);
                }
                self.write_exec_locks -= 1;
            }
        };

        Ok(FileWriteGuard { mode, node: fs_node })
    }

    pub fn enable_sealing(&mut self, initial_seals: SealFlags) {
        self.seals = Some(initial_seals);
    }

    /// Add a new seal to the current set, if allowed.
    pub fn try_add_seal(&mut self, flags: SealFlags) -> Result<(), Errno> {
        if let Some(seals) = self.seals.as_mut() {
            if seals.contains(SealFlags::SEAL) {
                // More seals cannot be added.
                return error!(EPERM);
            }

            // Write seal cannot be added when we have writable mappings.
            if flags.contains(SealFlags::WRITE) && self.num_write_mappings > 0 {
                return error!(EBUSY);
            }

            seals.insert(flags);

            Ok(())
        } else {
            // Seals are not allowed for this file.
            error!(EINVAL)
        }
    }

    /// Fails with EPERM if the current seal flags contain any of the given `flags`.
    pub fn check_no_seal(&self, flags: SealFlags) -> Result<(), Errno> {
        if let Some(seals) = self.seals.as_ref() {
            if seals.intersects(flags) {
                return error!(EPERM);
            }
        }
        Ok(())
    }

    /// Returns current seals.
    pub fn get_seals(&self) -> Result<SealFlags, Errno> {
        self.seals.ok_or_else(|| errno!(EINVAL))
    }
}

// Lock guard held by objects that need to lock access to the file
// (FileObject and mm::Mapping).
pub struct FileWriteGuard {
    mode: FileWriteGuardMode,
    node: FsNodeHandle,
}

impl FileWriteGuard {
    pub fn into_ref(self) -> FileWriteGuardRef {
        FileWriteGuardRef(Some(Arc::new(self)))
    }
}

impl Drop for FileWriteGuard {
    fn drop(&mut self) {
        let mut state = self.node.write_guard_state.lock();
        match self.mode {
            FileWriteGuardMode::WriteFile => {
                assert!(state.write_exec_locks > 0);
                state.write_exec_locks -= 1;
            }
            FileWriteGuardMode::WriteMapping => {
                assert!(state.write_exec_locks > 0);
                state.write_exec_locks -= 1;
                assert!(state.num_write_mappings > 0);
                state.num_write_mappings -= 1;
            }
            FileWriteGuardMode::Exec => {
                assert!(state.write_exec_locks < 0);
                state.write_exec_locks += 1;
            }
        };
    }
}

impl std::fmt::Debug for FileWriteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FileWriteGuard::Locked").field(&self.mode).finish()
    }
}

#[derive(Clone, Debug)]
pub struct FileWriteGuardRef(pub Option<Arc<FileWriteGuard>>);

// `Eq` is required for `mm::Mapping`.
impl PartialEq for FileWriteGuardRef {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl Eq for FileWriteGuardRef {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    fn create_fs_node() -> Arc<FsNode> {
        let (_kernel, current_task) = create_kernel_and_task();

        current_task
            .fs()
            .root()
            .create_node(&current_task, b"foo", FileMode::IFREG, DeviceType::NONE)
            .expect("create_node")
            .entry
            .node
            .clone()
    }

    #[::fuchsia::test]
    async fn test_write_exec_locking() {
        let fs_node = create_fs_node();

        let write_guard = fs_node
            .create_write_guard(FileWriteGuardMode::WriteFile)
            .expect("FsNode::lock failed unexpectedly");

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::Exec).unwrap_err(),
            errno!(ETXTBSY)
        );

        let write_mapping_guard = fs_node
            .create_write_guard(FileWriteGuardMode::WriteMapping)
            .expect("FsNode::lock failed unexpectedly")
            .into_ref();

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::Exec).unwrap_err(),
            errno!(ETXTBSY)
        );

        let write_mapping_guard_2 = write_mapping_guard.clone();

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::Exec).unwrap_err(),
            errno!(ETXTBSY)
        );

        std::mem::drop(write_guard);

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::Exec).unwrap_err(),
            errno!(ETXTBSY)
        );

        std::mem::drop(write_mapping_guard);

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::Exec).unwrap_err(),
            errno!(ETXTBSY)
        );

        std::mem::drop(write_mapping_guard_2);

        let exec_guard = fs_node
            .create_write_guard(FileWriteGuardMode::Exec)
            .expect("FsNode::lock failed unexpectedly");

        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::WriteFile).unwrap_err(),
            errno!(ETXTBSY)
        );

        std::mem::drop(exec_guard);

        fs_node
            .create_write_guard(FileWriteGuardMode::WriteFile)
            .expect("FsNode::lock failed unexpectedly");
    }

    #[::fuchsia::test]
    async fn test_no_seals() {
        let mut state = FileWriteGuardState::default();

        // By default seals are not enabled.
        assert_eq!(state.try_add_seal(SealFlags::WRITE), error!(EINVAL));
        assert_eq!(state.check_no_seal(SealFlags::WRITE), Ok(()));
        assert_eq!(state.get_seals(), error!(EINVAL));
    }

    #[::fuchsia::test]
    async fn test_seals() {
        let fs_node = create_fs_node();

        {
            let mut state = fs_node.write_guard_state.lock();

            state.enable_sealing(SealFlags::empty());

            assert_eq!(state.check_no_seal(SealFlags::WRITE), Ok(()));
            assert_eq!(state.get_seals(), Ok(SealFlags::empty()));

            // Apply WRITE seal.
            assert_eq!(state.try_add_seal(SealFlags::WRITE), Ok(()));
            assert_eq!(state.check_no_seal(SealFlags::WRITE), error!(EPERM));
            assert_eq!(state.get_seals(), Ok(SealFlags::WRITE));
        }

        // Files with WRITE seal can be opened for write.
        let file_guard = fs_node
            .create_write_guard(FileWriteGuardMode::WriteFile)
            .expect("lock(WriteFile) failed");

        // Files with WRITE seal cannot be mapped.
        assert_eq!(
            fs_node.create_write_guard(FileWriteGuardMode::WriteMapping).unwrap_err(),
            errno!(EPERM)
        );

        std::mem::drop(file_guard);
    }

    #[::fuchsia::test]
    async fn test_seals_block_when_mapped() {
        let fs_node = create_fs_node();
        fs_node.write_guard_state.lock().enable_sealing(SealFlags::empty());

        let _write_guard = fs_node
            .create_write_guard(FileWriteGuardMode::WriteFile)
            .expect("FsNode::lock failed unexpectedly");
        let write_mapping_guard = fs_node
            .create_write_guard(FileWriteGuardMode::WriteMapping)
            .expect("FsNode::lock failed unexpectedly");

        // Should fail since the file is mapped.
        {
            let mut state = fs_node.write_guard_state.lock();
            assert_eq!(state.try_add_seal(SealFlags::WRITE), error!(EBUSY));
            assert_eq!(state.check_no_seal(SealFlags::WRITE), Ok(()));
        }

        std::mem::drop(write_mapping_guard);

        // Should succeed after file is unmapped.
        {
            let mut state = fs_node.write_guard_state.lock();
            assert_eq!(state.try_add_seal(SealFlags::WRITE), Ok(()));
            assert_eq!(state.check_no_seal(SealFlags::WRITE), error!(EPERM));
        }
    }

    #[::fuchsia::test]
    async fn test_seals_sealed() {
        let fs_node = create_fs_node();
        let mut state = fs_node.write_guard_state.lock();

        state.enable_sealing(SealFlags::SEAL);

        assert_eq!(state.get_seals(), Ok(SealFlags::SEAL));

        assert_eq!(state.try_add_seal(SealFlags::WRITE), error!(EPERM));
        assert_eq!(state.get_seals(), Ok(SealFlags::SEAL));
    }
}
