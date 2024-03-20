// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::TaskBuilder;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use selinux::security_server::SecurityServer;
use starnix_sync::{FileOpsCore, FileOpsIoctl, Locked, Unlocked, WriteOps};
use std::{ffi::CString, mem::MaybeUninit, sync::Arc};
use zerocopy::{AsBytes, NoCell};

use crate::{
    device::init_common_devices,
    fs::{fuchsia::RemoteFs, tmpfs::TmpFs},
    mm::{
        syscalls::{do_mmap, sys_mremap},
        MemoryAccessor, MemoryAccessorExt, PAGE_SIZE,
    },
    task::{CurrentTask, Kernel, Task},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_node_impl_not_dir, Anon, CacheMode, FdNumber, FileHandle,
        FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsContext, FsNode, FsNodeOps, FsStr,
    },
};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_uapi::{
    errors::Errno, open_flags::OpenFlags, statfs, user_address::UserAddress, vfs::default_statfs,
    MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE,
};

/// Create a FileSystemHandle for use in testing.
///
/// Open "/pkg" and returns an FsContext rooted in that directory.
fn create_pkgfs(kernel: &Arc<Kernel>) -> FileSystemHandle {
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;
    let (server, client) = zx::Channel::create();
    fdio::open("/pkg", rights, server).expect("failed to open /pkg");
    RemoteFs::new_fs(
        kernel,
        client,
        FileSystemOptions { source: "/pkg".into(), ..Default::default() },
        rights,
    )
    .unwrap()
}

/// Creates a `Kernel`, `Task`, and `Locked<Unlocked>` with the package file system for testing purposes.
///
/// The `Task` is backed by a real process, and can be used to test syscalls.
pub fn create_kernel_task_and_unlocked_with_pkgfs<'l>(
) -> (Arc<Kernel>, AutoReleasableTask, Locked<'l, Unlocked>) {
    create_kernel_task_and_unlocked_with_fs_and_selinux(create_pkgfs, None)
}

pub fn create_kernel_and_task() -> (Arc<Kernel>, AutoReleasableTask) {
    let (kernel, task, _) = create_kernel_task_and_unlocked();
    (kernel, task)
}

pub fn create_kernel_and_task_with_selinux(
    security_server: Arc<SecurityServer>,
) -> (Arc<Kernel>, AutoReleasableTask) {
    let (kernel, task, _) =
        create_kernel_task_and_unlocked_with_fs_and_selinux(TmpFs::new_fs, Some(security_server));
    (kernel, task)
}

pub fn create_kernel_task_and_unlocked<'l>(
) -> (Arc<Kernel>, AutoReleasableTask, Locked<'l, Unlocked>) {
    create_kernel_task_and_unlocked_with_fs_and_selinux(TmpFs::new_fs, None)
}

pub fn create_kernel_task_and_unlocked_with_selinux<'l>(
    security_server: Arc<SecurityServer>,
) -> (Arc<Kernel>, AutoReleasableTask, Locked<'l, Unlocked>) {
    create_kernel_task_and_unlocked_with_fs_and_selinux(TmpFs::new_fs, Some(security_server))
}

/// Creates a `Kernel`, `Task`, and `Locked<Unlocked>` for testing purposes.
///
/// The `Task` is backed by a real process, and can be used to test syscalls.
fn create_kernel_task_and_unlocked_with_fs_and_selinux<'l>(
    create_fs: impl FnOnce(&Arc<Kernel>) -> FileSystemHandle,
    security_server: Option<Arc<SecurityServer>>,
) -> (Arc<Kernel>, AutoReleasableTask, Locked<'l, Unlocked>) {
    let mut locked = Unlocked::new();
    let kernel = Kernel::new(
        b"".into(),
        Default::default(),
        None,
        None,
        None,
        fuchsia_inspect::Node::default(),
        None,
        security_server,
    )
    .expect("failed to create kernel");

    let init_pid = kernel.pids.write().allocate_pid();
    assert_eq!(init_pid, 1);
    let fs = FsContext::new(create_fs(&kernel));
    let init_task = CurrentTask::create_init_process(
        &mut locked,
        &kernel,
        init_pid,
        CString::new("test-task").unwrap(),
        fs.clone(),
        &[],
    )
    .expect("failed to create first task");
    let system_task =
        CurrentTask::create_system_task(&mut locked, &kernel, fs).expect("create system task");
    kernel.kthreads.init(system_task).expect("failed to initialize kthreads");

    init_common_devices(&mut locked, &kernel.kthreads.system_task());

    // Take the lock on thread group and task in the correct order to ensure any wrong ordering
    // will trigger the tracing-mutex at the right call site.
    {
        let _l1 = init_task.thread_group.read();
        let _l2 = init_task.read();
    }

    (kernel, init_task.into(), locked)
}

/// Creates a new `Task` in the provided kernel.
///
/// The `Task` is backed by a real process, and can be used to test syscalls.
pub fn create_task(
    locked: &mut Locked<'_, Unlocked>,
    kernel: &Arc<Kernel>,
    task_name: &str,
) -> AutoReleasableTask {
    let task =
        CurrentTask::create_init_child_process(locked, kernel, &CString::new(task_name).unwrap())
            .expect("failed to create second task");

    // Take the lock on thread group and task in the correct order to ensure any wrong ordering
    // will trigger the tracing-mutex at the right call site.
    {
        let _l1 = task.thread_group.read();
        let _l2 = task.read();
    }

    task.into()
}

/// Maps a region of mery at least `len` bytes long with `PROT_READ | PROT_WRITE`,
/// `MAP_ANONYMOUS | MAP_PRIVATE`, returning the mapped address.
pub fn map_memory_anywhere(current_task: &CurrentTask, len: u64) -> UserAddress {
    map_memory(current_task, UserAddress::NULL, len)
}

/// Maps a region of memory large enough for the object with `PROT_READ | PROT_WRITE`,
/// `MAP_ANONYMOUS | MAP_PRIVATE` and writes the object to it, returning the mapped address.
///
/// Useful for syscall in-pointer parameters.
pub fn map_object_anywhere<T: AsBytes + NoCell>(
    current_task: &CurrentTask,
    object: &T,
) -> UserAddress {
    let addr = map_memory_anywhere(current_task, std::mem::size_of::<T>() as u64);
    current_task.write_object(addr.into(), object).expect("could not write object");
    addr
}

/// Maps `length` at `address` with `PROT_READ | PROT_WRITE`, `MAP_ANONYMOUS | MAP_PRIVATE`.
///
/// Returns the address returned by `sys_mmap`.
pub fn map_memory(current_task: &CurrentTask, address: UserAddress, length: u64) -> UserAddress {
    map_memory_with_flags(current_task, address, length, MAP_ANONYMOUS | MAP_PRIVATE)
}

/// Maps `length` at `address` with `PROT_READ | PROT_WRITE` and the specified flags.
///
/// Returns the address returned by `sys_mmap`.
pub fn map_memory_with_flags(
    current_task: &CurrentTask,
    address: UserAddress,
    length: u64,
    flags: u32,
) -> UserAddress {
    do_mmap(
        current_task,
        address,
        length as usize,
        PROT_READ | PROT_WRITE,
        flags,
        FdNumber::from_raw(-1),
        0,
    )
    .expect("Could not map memory")
}

/// Convenience wrapper around [`sys_mremap`] which extracts the returned [`UserAddress`] from
/// the generic [`SyscallResult`].
pub fn remap_memory(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    old_addr: UserAddress,
    old_length: u64,
    new_length: u64,
    flags: u32,
    new_addr: UserAddress,
) -> Result<UserAddress, Errno> {
    sys_mremap(
        locked,
        current_task,
        old_addr,
        old_length as usize,
        new_length as usize,
        flags,
        new_addr,
    )
}

/// Fills one page in the `current_task`'s address space starting at `addr` with the ASCII character
/// `data`. Panics if the write failed.
///
/// This method uses the `#[track_caller]` attribute, which will display the caller's file and line
/// number in the event of a panic. This makes it easier to find test regressions.
#[track_caller]
pub fn fill_page(current_task: &CurrentTask, addr: UserAddress, data: char) {
    let data = [data as u8].repeat(*PAGE_SIZE as usize);
    if let Err(err) = current_task.write_memory(addr, &data) {
        panic!("write page: failed to fill page @ {addr:?} with {data:?}: {err:?}");
    }
}

/// Checks that the page in `current_task`'s address space starting at `addr` is readable.
/// Panics if the read failed, or the page was not filled with the ASCII character `data`.
///
/// This method uses the `#[track_caller]` attribute, which will display the caller's file and line
/// number in the event of a panic. This makes it easier to find test regressions.
#[track_caller]
pub fn check_page_eq(current_task: &CurrentTask, addr: UserAddress, data: char) {
    let buf = match current_task.read_memory_to_vec(addr, *PAGE_SIZE as usize) {
        Ok(b) => b,
        Err(err) => panic!("read page: failed to read page @ {addr:?}: {err:?}"),
    };
    assert!(
        buf.into_iter().all(|c| c == data as u8),
        "unexpected payload: page @ {addr:?} should be filled with {data:?}"
    );
}

/// Checks that the page in `current_task`'s address space starting at `addr` is readable.
/// Panics if the read failed, or the page *was* filled with the ASCII character `data`.
///
/// This method uses the `#[track_caller]` attribute, which will display the caller's file and line
/// number in the event of a panic. This makes it easier to find test regressions.
#[track_caller]
pub fn check_page_ne(current_task: &CurrentTask, addr: UserAddress, data: char) {
    let buf = match current_task.read_memory_to_vec(addr, *PAGE_SIZE as usize) {
        Ok(b) => b,
        Err(err) => panic!("read page: failed to read page @ {addr:?}: {err:?}"),
    };
    assert!(
        !buf.into_iter().all(|c| c == data as u8),
        "unexpected payload: page @ {addr:?} should not be filled with {data:?}"
    );
}

/// Checks that the page in `current_task`'s address space starting at `addr` is unmapped.
/// Panics if the read succeeds, or if an error other than `EFAULT` occurs.
///
/// This method uses the `#[track_caller]` attribute, which will display the caller's file and line
/// number in the event of a panic. This makes it easier to find test regressions.
#[track_caller]
pub fn check_unmapped(current_task: &CurrentTask, addr: UserAddress) {
    match current_task.read_memory_to_vec(addr, *PAGE_SIZE as usize) {
        Ok(_) => panic!("read page: page @ {addr:?} should be unmapped"),
        Err(err) if err == starnix_uapi::errors::EFAULT => {}
        Err(err) => {
            panic!("read page: expected EFAULT reading page @ {addr:?} but got {err:?} instead")
        }
    }
}

/// An FsNodeOps implementation that panics if you try to open it. Useful as a stand-in for testing
/// APIs that require a FsNodeOps implementation but don't actually use it.
pub struct PanickingFsNode;

impl FsNodeOps for PanickingFsNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        panic!("should not be called")
    }
}

/// An implementation of [`FileOps`] that panics on any read, write, or ioctl operation.
pub struct PanickingFile;

impl PanickingFile {
    /// Creates a [`FileObject`] whose implementation panics on reads, writes, and ioctls.
    pub fn new_file(current_task: &CurrentTask) -> FileHandle {
        Anon::new_file(current_task, Box::new(PanickingFile), OpenFlags::RDWR)
    }
}

impl FileOps for PanickingFile {
    fileops_impl_nonseekable!();

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        panic!("write called on TestFile")
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        panic!("read called on TestFile")
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, FileOpsIoctl>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _request: u32,
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        panic!("ioctl called on TestFile")
    }
}

/// Helper to write out data to a task's memory sequentially.
pub struct UserMemoryWriter<'a> {
    // The task's memory manager.
    mm: &'a Task,
    // The address to which to write the next bit of data.
    current_addr: UserAddress,
}

impl<'a> UserMemoryWriter<'a> {
    /// Constructs a new `UserMemoryWriter` to write to `task`'s memory at `addr`.
    pub fn new(task: &'a Task, addr: UserAddress) -> Self {
        Self { mm: task, current_addr: addr }
    }

    /// Writes all of `data` to the current address in the task's address space, incrementing the
    /// current address by the size of `data`. Returns the address at which the data starts.
    /// Panics on failure.
    pub fn write(&mut self, data: &[u8]) -> UserAddress {
        let bytes_written = self.mm.write_memory(self.current_addr, data).unwrap();
        assert_eq!(bytes_written, data.len());
        let start_addr = self.current_addr;
        self.current_addr += bytes_written;
        start_addr
    }

    /// Writes `object` to the current address in the task's address space, incrementing the
    /// current address by the size of `object`. Returns the address at which the data starts.
    /// Panics on failure.
    pub fn write_object<T: AsBytes + NoCell>(&mut self, object: &T) -> UserAddress {
        self.write(object.as_bytes())
    }

    /// Returns the current address at which data will be next written.
    pub fn current_address(&self) -> UserAddress {
        self.current_addr
    }
}

#[derive(Debug)]
pub struct AutoReleasableTask(Option<CurrentTask>);

impl AutoReleasableTask {
    fn as_ref(this: &Self) -> &CurrentTask {
        this.0.as_ref().unwrap()
    }

    fn as_mut(this: &mut Self) -> &mut CurrentTask {
        this.0.as_mut().unwrap()
    }
}

impl From<CurrentTask> for AutoReleasableTask {
    fn from(task: CurrentTask) -> Self {
        Self(Some(task))
    }
}

impl From<TaskBuilder> for AutoReleasableTask {
    fn from(builder: TaskBuilder) -> Self {
        CurrentTask::from(builder).into()
    }
}

impl Drop for AutoReleasableTask {
    fn drop(&mut self) {
        let mut locked = Unlocked::new(); // TODO(mariagl): Find a way to avoid creating a new locked context here.
        self.0.take().unwrap().release(&mut locked);
    }
}

impl std::ops::Deref for AutoReleasableTask {
    type Target = CurrentTask;

    fn deref(&self) -> &Self::Target {
        AutoReleasableTask::as_ref(self)
    }
}

impl std::ops::DerefMut for AutoReleasableTask {
    fn deref_mut(&mut self) -> &mut Self::Target {
        AutoReleasableTask::as_mut(self)
    }
}

impl std::borrow::Borrow<CurrentTask> for AutoReleasableTask {
    fn borrow(&self) -> &CurrentTask {
        AutoReleasableTask::as_ref(self)
    }
}

impl std::convert::AsRef<CurrentTask> for AutoReleasableTask {
    fn as_ref(&self) -> &CurrentTask {
        AutoReleasableTask::as_ref(self)
    }
}

impl MemoryAccessor for AutoReleasableTask {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        (**self).read_memory(addr, bytes)
    }
    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        (**self).read_memory_partial_until_null_byte(addr, bytes)
    }
    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        (**self).read_memory_partial(addr, bytes)
    }
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        (**self).write_memory(addr, bytes)
    }
    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        (**self).write_memory_partial(addr, bytes)
    }
    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        (**self).zero(addr, length)
    }
}

struct TestFs;
impl FileSystemOps for TestFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(0))
    }
    fn name(&self) -> &'static FsStr {
        "test".into()
    }

    fn generate_node_ids(&self) -> bool {
        false
    }
}

pub fn create_fs(kernel: &Arc<Kernel>, ops: impl FsNodeOps) -> FileSystemHandle {
    let test_fs = FileSystem::new(&kernel, CacheMode::Uncached, TestFs, Default::default());
    let bus_dir_node = FsNode::new_root(ops);
    test_fs.set_root_node(bus_dir_node);
    test_fs
}
