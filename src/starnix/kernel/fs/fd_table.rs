// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;

use crate::fs::*;
use crate::lock::Mutex;
use crate::task::Task;
use crate::types::*;

bitflags! {
    pub struct FdFlags: u32 {
        const CLOEXEC = FD_CLOEXEC;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FdTableId(usize);

impl FdTableId {
    fn new(id: *const HashMap<FdNumber, FdTableEntry>) -> Self {
        Self(id as usize)
    }
}

#[derive(Debug, Clone)]
pub struct FdTableEntry {
    pub file: FileHandle,

    // Identifier of the FdTable containing this entry.
    fd_table_id: FdTableId,

    // Rather than using a separate "flags" field, we could maintain this data
    // as a bitfield over the file descriptors because there is only one flag
    // currently (CLOEXEC) and file descriptor numbers tend to cluster near 0.
    flags: FdFlags,
}

impl Drop for FdTableEntry {
    fn drop(&mut self) {
        self.file.name.entry.node.record_lock_release(RecordLockOwner::FdTable(self.fd_table_id));
        self.file.flush();
    }
}

impl FdTableEntry {
    fn new(file: FileHandle, fd_table_id: FdTableId, flags: FdFlags) -> FdTableEntry {
        FdTableEntry { file, fd_table_id, flags }
    }
}

/// Having the map a separate data structure allows us to memoize next_fd, which is the
/// lowest numbered file descriptor not in use.
#[derive(Clone, Debug)]
struct FdMap {
    map: HashMap<FdNumber, FdTableEntry>,
    next_fd: FdNumber,
}

impl Default for FdMap {
    fn default() -> Self {
        FdMap { map: HashMap::default(), next_fd: FdNumber::from_raw(0) }
    }
}

impl FdMap {
    fn insert_entry(
        &mut self,
        task: &Task,
        fd: FdNumber,
        entry: FdTableEntry,
    ) -> Result<(), Errno> {
        if fd.raw() as u64 >= task.thread_group.get_rlimit(Resource::NOFILE) {
            return error!(EMFILE);
        }
        if fd.raw() == self.next_fd.raw() {
            self.next_fd = self.calculate_lowest_available_fd(&FdNumber::from_raw(fd.raw() + 1));
        }
        self.map.insert(fd, entry);
        Ok(())
    }

    fn remove_entry(&mut self, fd: &FdNumber) -> Option<FdTableEntry> {
        let removed = self.map.remove(fd);
        if removed.is_some() && fd.raw() < self.next_fd.raw() {
            self.next_fd = *fd;
        }
        removed
    }

    // Returns the (possibly memoized) lowest available FD >= minfd in this map.
    fn get_lowest_available_fd(&self, minfd: FdNumber) -> FdNumber {
        if minfd.raw() > self.next_fd.raw() {
            return self.calculate_lowest_available_fd(&minfd);
        }
        self.next_fd
    }

    // Recalculates the lowest available FD >= minfd based on the contents of the map.
    fn calculate_lowest_available_fd(&self, minfd: &FdNumber) -> FdNumber {
        let mut fd = *minfd;
        while self.map.contains_key(&fd) {
            fd = FdNumber::from_raw(fd.raw() + 1);
        }
        fd
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&FdNumber, &mut FdTableEntry) -> bool,
    {
        self.map.retain(f);
        self.next_fd = self.calculate_lowest_available_fd(&FdNumber::from_raw(0));
    }
}

#[derive(Debug, Default)]
struct FdTableInner {
    map_handle: Mutex<FdMap>,
}

impl FdTableInner {
    fn id(&self) -> FdTableId {
        FdTableId::new(&self.map_handle.lock().map as *const HashMap<FdNumber, FdTableEntry>)
    }

    fn unshare(&self) -> FdTableInner {
        let inner = {
            let new_fdmap = self.map_handle.lock().clone();
            FdTableInner { map_handle: Mutex::new(new_fdmap) }
        };
        let id = inner.id();
        inner.map_handle.lock().map.values_mut().for_each(|entry| entry.fd_table_id = id);
        inner
    }
}

#[derive(Debug, Default)]
pub struct FdTable {
    // TODO(fxb/122600) The external mutex is only used to be able to drop the file descriptor
    // while keeping the table itself. It will be unneeded once the live state of a task is deleted
    // as soon as the task dies, instead of relying on Drop.
    table: Mutex<Arc<FdTableInner>>,
}

pub enum TargetFdNumber {
    /// The duplicated FdNumber will be the smallest available FdNumber.
    Default,

    /// The duplicated FdNumber should be this specific FdNumber.
    Specific(FdNumber),

    /// The duplicated FdNumber should be greater than this FdNumber.
    Minimum(FdNumber),
}

impl FdTable {
    pub fn id(&self) -> FdTableId {
        self.table.lock().id()
    }

    pub fn fork(&self) -> FdTable {
        let inner = self.table.lock().unshare();
        FdTable { table: Mutex::new(Arc::new(inner)) }
    }

    pub fn unshare(&self) {
        let mut table = self.table.lock();
        let unshared_inner = table.unshare();
        *table = Arc::new(unshared_inner);
    }

    pub fn exec(&self) {
        let inner = self.table.lock();
        let mut state = inner.map_handle.lock();
        state.retain(|_fd, entry| !entry.flags.contains(FdFlags::CLOEXEC));
    }

    pub fn insert(&self, task: &Task, fd: FdNumber, file: FileHandle) -> Result<(), Errno> {
        self.insert_with_flags(task, fd, file, FdFlags::empty())
    }

    pub fn insert_with_flags(
        &self,
        task: &Task,
        fd: FdNumber,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<(), Errno> {
        let id = self.id();
        let inner = self.table.lock();
        let mut state = inner.map_handle.lock();
        state.insert_entry(task, fd, FdTableEntry::new(file, id, flags))
    }

    pub fn add_with_flags(
        &self,
        task: &Task,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        let id = self.id();
        let inner = self.table.lock();
        let mut state = inner.map_handle.lock();
        let fd = state.next_fd;
        state.insert_entry(task, fd, FdTableEntry::new(file, id, flags))?;
        Ok(fd)
    }

    // Duplicates a file handle.
    // If target is  TargetFdNumber::Minimum, a new FdNumber is allocated. Returns the new FdNumber.
    pub fn duplicate(
        &self,
        task: &Task,
        oldfd: FdNumber,
        target: TargetFdNumber,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        // Drop the file object only after releasing the writer lock in case
        // the close() function on the FileOps calls back into the FdTable.
        let _removed_file;
        let result = {
            let id = self.id();
            let inner = self.table.lock();
            let mut state = inner.map_handle.lock();
            let file = state
                .map
                .get(&oldfd)
                .map(|entry| entry.file.clone())
                .ok_or_else(|| errno!(EBADF))?;

            let fd = match target {
                TargetFdNumber::Specific(fd) => {
                    _removed_file = state.remove_entry(&fd);
                    fd
                }
                TargetFdNumber::Minimum(fd) => state.get_lowest_available_fd(fd),
                TargetFdNumber::Default => state.get_lowest_available_fd(FdNumber::from_raw(0)),
            };
            state.insert_entry(task, fd, FdTableEntry::new(file, id, flags))?;
            Ok(fd)
        };
        result
    }

    pub fn get(&self, fd: FdNumber) -> Result<FileHandle, Errno> {
        self.get_with_flags(fd).map(|(file, _flags)| file)
    }

    pub fn get_with_flags(&self, fd: FdNumber) -> Result<(FileHandle, FdFlags), Errno> {
        let inner = self.table.lock();
        let state = inner.map_handle.lock();
        state
            .map
            .get(&fd)
            .map(|entry| (entry.file.clone(), entry.flags))
            .ok_or_else(|| errno!(EBADF))
    }

    pub fn get_unless_opath(&self, fd: FdNumber) -> Result<FileHandle, Errno> {
        let file = self.get(fd)?;
        if file.flags().contains(OpenFlags::PATH) {
            return error!(EBADF);
        }
        Ok(file)
    }

    pub fn close(&self, fd: FdNumber) -> Result<(), Errno> {
        // Drop the file object only after releasing the writer lock in case
        // the close() function on the FileOps calls back into the FdTable.
        let removed = {
            let inner = self.table.lock();
            let mut state = inner.map_handle.lock();
            state.remove_entry(&fd)
        };
        if removed.is_some() {
            Ok(())
        } else {
            Err(errno!(EBADF))
        }
    }

    /// Drop the fd table, closing any files opened exclusively by this table.
    // TODO(fxb/122600) This will be unneeded once the live state of a task is deleted as soon as
    // the task dies, instead of relying on Drop.
    pub fn drop_local(&self) {
        // Replace the file table with an empty one. Extract it first so that the drop happens
        // without the lock in case a file call back to the table when it is closed.
        let _internal_state = { std::mem::take(self.table.lock().deref_mut()) };
    }

    pub fn get_fd_flags(&self, fd: FdNumber) -> Result<FdFlags, Errno> {
        self.get_with_flags(fd).map(|(_file, flags)| flags)
    }

    pub fn set_fd_flags(&self, fd: FdNumber, flags: FdFlags) -> Result<(), Errno> {
        self.table
            .lock()
            .map_handle
            .lock()
            .map
            .get_mut(&fd)
            .map(|entry| {
                entry.flags = flags;
            })
            .ok_or_else(|| errno!(EBADF))
    }

    pub fn retain<F>(&self, f: F)
    where
        F: Fn(FdNumber, &mut FdFlags) -> bool,
    {
        self.table.lock().map_handle.lock().map.retain(|fd, entry| f(*fd, &mut entry.flags));
    }

    /// Returns a vector of all current file descriptors in the table.
    pub fn get_all_fds(&self) -> Vec<FdNumber> {
        self.table.lock().map_handle.lock().map.keys().cloned().collect()
    }
}

impl Clone for FdTable {
    fn clone(&self) -> Self {
        FdTable { table: Mutex::new(self.table.lock().clone()) }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::fs::fuchsia::SyslogFile;
    use crate::task::*;
    use crate::testing::*;

    fn add(
        current_task: &CurrentTask,
        files: &FdTable,
        file: FileHandle,
    ) -> Result<FdNumber, Errno> {
        files.add_with_flags(current_task, file, FdFlags::empty())
    }

    #[::fuchsia::test]
    async fn test_fd_table_install() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd0.raw(), 0);
        let fd1 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd1.raw(), 1);

        assert!(Arc::ptr_eq(&files.get(fd0).unwrap(), &file));
        assert!(Arc::ptr_eq(&files.get(fd1).unwrap(), &file));
        assert_eq!(files.get(FdNumber::from_raw(fd1.raw() + 1)).map(|_| ()), error!(EBADF));
    }

    #[::fuchsia::test]
    async fn test_fd_table_fork() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file).unwrap();
        let fd2 = FdNumber::from_raw(2);

        let forked = files.fork();

        assert_eq!(Arc::as_ptr(&files.get(fd0).unwrap()), Arc::as_ptr(&forked.get(fd0).unwrap()));
        assert_eq!(Arc::as_ptr(&files.get(fd1).unwrap()), Arc::as_ptr(&forked.get(fd1).unwrap()));
        assert!(files.get(fd2).is_err());
        assert!(forked.get(fd2).is_err());

        files.set_fd_flags(fd0, FdFlags::CLOEXEC).unwrap();
        assert_eq!(FdFlags::CLOEXEC, files.get_fd_flags(fd0).unwrap());
        assert_ne!(FdFlags::CLOEXEC, forked.get_fd_flags(fd0).unwrap());
    }

    #[::fuchsia::test]
    async fn test_fd_table_exec() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file).unwrap();

        files.set_fd_flags(fd0, FdFlags::CLOEXEC).unwrap();

        assert!(files.get(fd0).is_ok());
        assert!(files.get(fd1).is_ok());

        files.exec();

        assert!(files.get(fd0).is_err());
        assert!(files.get(fd1).is_ok());
    }

    #[::fuchsia::test]
    async fn test_fd_table_pack_values() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        // Add two FDs.
        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd0.raw(), 0);
        assert_eq!(fd1.raw(), 1);

        // Close FD 0
        assert!(files.close(fd0).is_ok());
        assert!(files.close(fd0).is_err());
        // Now it's gone.
        assert!(files.get(fd0).is_err());

        // The next FD we insert fills in the hole we created.
        let another_fd = add(&current_task, &files, file).unwrap();
        assert_eq!(another_fd.raw(), 0);
    }
}
