// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use crate::bpf::map::{compute_map_storage_size, Map, PinnedMap, RINGBUF_SIGNAL};
use crate::bpf::program::Program;
use crate::bpf::syscalls::BpfTypeFormat;
use crate::mm::memory::MemoryObject;
use crate::mm::{ProtectionFlags, PAGE_SIZE};
use crate::task::{
    CurrentTask, EventHandler, SignalHandler, SignalHandlerInner, Task, WaitCanceler, Waiter,
};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, fs_node_impl_not_dir,
    fs_node_impl_xattr_delegate, CacheMode, FdNumber, FileObject, FileOps, FileSystem,
    FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsStr, MemoryDirectoryFile, MemoryXattrStorage, NamespaceNode, XattrStorage as _,
};
use ebpf::MapSchema;
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    bpf_map_type_BPF_MAP_TYPE_ARRAY, bpf_map_type_BPF_MAP_TYPE_RINGBUF, errno, error, statfs,
    BPF_FS_MAGIC,
};
use std::sync::Arc;
use zx::AsHandleRef;

/// A reference to a BPF object that can be stored in either an FD or an entry in the /sys/fs/bpf
/// filesystem.
#[derive(Debug, Clone)]
pub enum BpfHandle {
    Program(Arc<Program>),

    // Stub used to fake loading of programs of unknown types.
    ProgramStub(u32),

    Map(PinnedMap),
    BpfTypeFormat(Arc<BpfTypeFormat>),
}

impl BpfHandle {
    pub fn as_map(&self) -> Result<&PinnedMap, Errno> {
        match self {
            Self::Map(ref map) => Ok(map),
            _ => error!(EINVAL),
        }
    }
    pub fn as_program(&self) -> Result<&Arc<Program>, Errno> {
        match self {
            Self::Program(ref program) => Ok(program),
            _ => error!(EINVAL),
        }
    }

    // Returns VMO and schema if this handle references a map.
    fn get_map_vmo(&self) -> Result<(Arc<zx::Vmo>, MapSchema), Errno> {
        match self {
            Self::Map(map) => {
                let vmo = map.vmo().ok_or_else(|| errno!(ENODEV))?;
                Ok((vmo, map.schema))
            }
            _ => error!(ENODEV),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Map(_) => "bpf-map",
            Self::Program(_) | Self::ProgramStub(_) => "bpf-prog",
            Self::BpfTypeFormat(_) => "bpf-type",
        }
    }
}

impl From<Program> for BpfHandle {
    fn from(program: Program) -> Self {
        Self::Program(Arc::new(program))
    }
}

impl From<Map> for BpfHandle {
    fn from(map: Map) -> Self {
        Self::Map(Arc::pin(map))
    }
}

impl From<BpfTypeFormat> for BpfHandle {
    fn from(format: BpfTypeFormat) -> Self {
        Self::BpfTypeFormat(Arc::new(format))
    }
}

impl FileOps for BpfHandle {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(TODO("https://fxbug.dev/322874229"), "bpf handle read");
        error!(EINVAL)
    }
    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &crate::task::CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(TODO("https://fxbug.dev/322873841"), "bpf handle write");
        error!(EINVAL)
    }

    fn get_memory(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        let (vmo, schema) = self.get_map_vmo()?;

        // Because of the specific condition needed to map this object, the size must be known.
        let length = length.ok_or_else(|| errno!(EINVAL))?;

        // This cannot be mapped executable.
        if prot.contains(ProtectionFlags::EXEC) {
            return error!(EPERM);
        }

        let memory_object = match schema.map_type {
            bpf_map_type_BPF_MAP_TYPE_RINGBUF => {
                // Starting from the second page, this cannot be mapped writable.
                if length > *PAGE_SIZE as usize {
                    if prot.contains(ProtectionFlags::WRITE) {
                        return error!(EPERM);
                    }
                    // This cannot be mapped outside of the 2 control pages and the 2 data sections.
                    if length > 2 * (*PAGE_SIZE as usize) + 2 * schema.max_entries as usize {
                        return error!(EINVAL);
                    }
                }

                let vmo_dup = vmo
                    .as_handle_ref()
                    .duplicate(zx::Rights::SAME_RIGHTS)
                    .map_err(|_| errno!(EIO))?
                    .into();
                Arc::new(MemoryObject::RingBuf(vmo_dup))
            }

            bpf_map_type_BPF_MAP_TYPE_ARRAY => {
                let array_size =
                    round_up_to_increment(compute_map_storage_size(&schema)?, *PAGE_SIZE as usize)?;
                if length > array_size {
                    return error!(EINVAL);
                }

                let vmo_dup = vmo
                    .as_handle_ref()
                    .duplicate(zx::Rights::SAME_RIGHTS)
                    .map_err(|_| errno!(EIO))?
                    .into();
                Arc::new(MemoryObject::Vmo(vmo_dup))
            }

            // Other maps cannot be mmap'ed.
            _ => return error!(ENODEV),
        };

        Ok(memory_object)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        if !events.contains(FdEvents::POLLIN) {
            return None;
        }

        let Ok((vmo, schema)) = self.get_map_vmo() else {
            return None;
        };

        // Only ringbuffers can be polled.
        if schema.map_type != bpf_map_type_BPF_MAP_TYPE_RINGBUF {
            return None;
        }

        let handler = SignalHandler {
            inner: SignalHandlerInner::ZxHandle(|signals| {
                if signals.contains(RINGBUF_SIGNAL) {
                    FdEvents::POLLIN
                } else {
                    FdEvents::empty()
                }
            }),
            event_handler: handler,
            err_code: None,
        };

        // Reset the signal before waiting. The case when the ring buffer already has some data
        // is handled by the caller: it should call `query_events` after starting the waiter.
        vmo.as_handle_ref()
            .signal(RINGBUF_SIGNAL, zx::Signals::empty())
            .expect("Failed to set signal or a ring buffer VMO");

        let canceler = waiter
            .wake_on_zircon_signals(&vmo.as_handle_ref(), RINGBUF_SIGNAL, handler)
            .expect("Failed to wait for signals on ringbuf VMO");
        Some(WaitCanceler::new_vmo(Arc::downgrade(&vmo), canceler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        match self {
            Self::Map(map) if map.schema.map_type == bpf_map_type_BPF_MAP_TYPE_RINGBUF => {
                if map.can_read() {
                    Ok(FdEvents::POLLIN)
                } else {
                    Ok(FdEvents::empty())
                }
            }
            _ => error!(ENODEV),
        }
    }
}

pub fn get_bpf_object(task: &Task, fd: FdNumber) -> Result<BpfHandle, Errno> {
    Ok(task.files.get(fd)?.downcast_file::<BpfHandle>().ok_or_else(|| errno!(EBADF))?.clone())
}

pub struct BpfFs;
impl BpfFs {
    pub fn new_fs(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, BpfFs, options)?;
        let node = FsNode::new_root_with_properties(BpfFsDir::new(), |info| {
            info.mode |= FileMode::ISVTX;
        });
        fs.set_root_node(node);
        Ok(fs)
    }
}

impl FileSystemOps for BpfFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(BPF_FS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "bpf".into()
    }

    fn rename(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        Ok(())
    }
}

pub struct BpfFsDir {
    xattrs: MemoryXattrStorage,
}

impl BpfFsDir {
    fn new() -> Self {
        Self { xattrs: MemoryXattrStorage::default() }
    }

    pub fn register_pin<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &NamespaceNode,
        name: &FsStr,
        object: BpfHandle,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        node.entry.create_entry(
            locked,
            current_task,
            &node.mount,
            name,
            |_locked, dir, _mount, _name| {
                Ok(dir.fs().create_node(
                    current_task,
                    BpfFsObject::new(object),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o600), current_task.as_fscred()),
                ))
            },
        )?;
        Ok(())
    }
}

impl FsNodeOps for BpfFsDir {
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        _name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        Ok(node.fs().create_node(
            current_task,
            BpfFsDir::new(),
            FsNodeInfo::new_factory(mode | FileMode::ISVTX, owner),
        ))
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }

    fn link(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        Ok(())
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        Ok(())
    }
}

pub struct BpfFsObject {
    pub handle: BpfHandle,
    xattrs: MemoryXattrStorage,
}

impl BpfFsObject {
    fn new(handle: BpfHandle) -> Self {
        Self { handle, xattrs: MemoryXattrStorage::default() }
    }
}

impl FsNodeOps for BpfFsObject {
    fs_node_impl_not_dir!();
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        error!(EIO)
    }
}
