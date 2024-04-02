// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{kobject::DeviceMetadata, DeviceMode},
    fs::sysfs::{BlockDeviceDirectory, BlockDeviceInfo},
    mm::{MemoryAccessorExt, ProtectionFlags, PAGE_SIZE},
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        default_ioctl, fileops_impl_dataless, fileops_impl_seekable, fileops_impl_seekless, Buffer,
        FdNumber, FileHandle, FileObject, FileOps, FsNode, FsString, InputBufferCallback,
        PeekBufferSegmentsCallback,
    },
};
use bitflags::bitflags;
use fuchsia_zircon::{Vmo, VmoChildOptions};
use starnix_logging::track_stub;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    __kernel_old_dev_t,
    device_type::{DeviceType, LOOP_MAJOR},
    errno, error,
    errors::Errno,
    loop_info, loop_info64,
    open_flags::OpenFlags,
    uapi,
    user_address::UserRef,
    user_buffer::UserBuffer,
    BLKFLSBUF, BLKGETSIZE, BLKGETSIZE64, LOOP_CHANGE_FD, LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_CTL_ADD,
    LOOP_CTL_GET_FREE, LOOP_CTL_REMOVE, LOOP_GET_STATUS, LOOP_GET_STATUS64, LOOP_SET_BLOCK_SIZE,
    LOOP_SET_CAPACITY, LOOP_SET_DIRECT_IO, LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64,
    LO_FLAGS_AUTOCLEAR, LO_FLAGS_DIRECT_IO, LO_FLAGS_PARTSCAN, LO_FLAGS_READ_ONLY, LO_KEY_SIZE,
};
use std::{
    collections::btree_map::{BTreeMap, Entry},
    sync::Arc,
};

// See LOOP_SET_BLOCK_SIZE in <https://man7.org/linux/man-pages/man4/loop.4.html>.
const MIN_BLOCK_SIZE: u32 = 512;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct LoopDeviceFlags: u32 {
        const READ_ONLY = LO_FLAGS_READ_ONLY;
        const AUTOCLEAR = LO_FLAGS_AUTOCLEAR;
        const PARTSCAN = LO_FLAGS_PARTSCAN;
        const DIRECT_IO = LO_FLAGS_DIRECT_IO;
    }
}

#[derive(Debug)]
struct LoopDeviceState {
    backing_file: Option<FileHandle>,
    block_size: u32,

    // See struct loop_info64 for details about these fields.
    offset: u64,
    size_limit: u64,
    flags: LoopDeviceFlags,

    // Encryption is not implemented.
    encrypt_type: u32,
    encrypt_key: Vec<u8>,
    init: [u64; 2],
}

impl Default for LoopDeviceState {
    fn default() -> Self {
        LoopDeviceState {
            backing_file: Default::default(),
            block_size: MIN_BLOCK_SIZE,
            offset: Default::default(),
            size_limit: Default::default(),
            flags: Default::default(),
            encrypt_type: Default::default(),
            encrypt_key: Default::default(),
            init: Default::default(),
        }
    }
}

impl LoopDeviceState {
    fn check_bound(&self) -> Result<(), Errno> {
        if self.backing_file.is_none() {
            error!(ENXIO)
        } else {
            Ok(())
        }
    }

    fn set_backing_file(
        &mut self,
        current_task: &CurrentTask,
        backing_file: FileHandle,
    ) -> Result<(), Errno> {
        if self.backing_file.is_some() {
            return error!(EBUSY);
        }
        self.backing_file = Some(backing_file);
        self.update_size_limit(current_task)?;
        Ok(())
    }

    fn set_info(&mut self, info: &uapi::loop_info64) {
        let encrypt_key_size = info.lo_encrypt_key_size.clamp(0, LO_KEY_SIZE);
        self.offset = info.lo_offset;
        self.size_limit = info.lo_sizelimit;
        self.flags = LoopDeviceFlags::from_bits_truncate(info.lo_flags);
        self.encrypt_type = info.lo_encrypt_type;
        self.encrypt_key = info.lo_encrypt_key[0..(encrypt_key_size as usize)].to_owned();
        self.init = info.lo_init;
    }

    fn update_size_limit(&mut self, current_task: &CurrentTask) -> Result<(), Errno> {
        if let Some(backing_file) = &self.backing_file {
            let backing_stat = backing_file.node().stat(current_task)?;
            self.size_limit = backing_stat.st_size as u64;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct LoopDevice {
    number: u32,
    state: Mutex<LoopDeviceState>,
}

impl LoopDevice {
    fn new<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask, minor: u32) -> Arc<Self>
    where
        L: LockBefore<FileOpsCore>,
    {
        let kernel = current_task.kernel();
        let registry = &kernel.device_registry;
        let loop_device_name = FsString::from(format!("loop{minor}"));
        let virtual_block_class =
            registry.get_or_create_class("block".into(), registry.virtual_bus());
        let device = Arc::new(Self { number: minor, state: Default::default() });
        let device_weak = Arc::<LoopDevice>::downgrade(&device);
        registry.add_device(
            locked,
            current_task,
            loop_device_name.as_ref(),
            DeviceMetadata::new(
                loop_device_name.clone(),
                DeviceType::new(LOOP_MAJOR, minor),
                DeviceMode::Block,
            ),
            virtual_block_class,
            move |dev| BlockDeviceDirectory::new(dev, device_weak.clone()),
        );
        device
    }

    fn create_file_ops(self: &Arc<Self>) -> Box<dyn FileOps> {
        Box::new(LoopDeviceFile { device: self.clone() })
    }

    fn backing_file(&self) -> Option<FileHandle> {
        self.state.lock().backing_file.clone()
    }

    fn is_bound(&self) -> bool {
        self.state.lock().backing_file.is_some()
    }

    fn offset_for_backing_file(&self, offset: usize) -> usize {
        self.state.lock().offset.saturating_add(offset as u64) as usize
    }
}

fn check_block_size(block_size: u32) -> Result<(), Errno> {
    let page_size = *PAGE_SIZE as u32;
    let mut allowed_size = MIN_BLOCK_SIZE;
    while allowed_size <= page_size {
        if block_size == allowed_size {
            return Ok(());
        }
        allowed_size *= 2;
    }
    error!(EINVAL)
}

impl BlockDeviceInfo for LoopDevice {
    fn size(&self) -> Result<usize, Errno> {
        Ok(self.state.lock().size_limit as usize)
    }
}

#[derive(Debug)]
struct CroppedInputBuffer<'a> {
    base: &'a mut dyn InputBuffer,
    size: usize,
    drained: bool,
}

impl<'a> CroppedInputBuffer<'a> {
    fn new(base: &'a mut dyn InputBuffer, size: usize) -> Self {
        debug_assert!(size <= base.bytes_read() + base.available());
        CroppedInputBuffer { base, size, drained: false }
    }
}

impl<'a> Buffer for CroppedInputBuffer<'a> {
    fn segments_count(&self) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }

    fn peek_each_segment(
        &mut self,
        callback: &mut PeekBufferSegmentsCallback<'_>,
    ) -> Result<(), Errno> {
        let mut pos = 0;
        self.base.peek_each_segment(&mut |buffer: &UserBuffer| {
            if pos >= self.size {
                return;
            } else if pos + buffer.length > self.size {
                let cropped_size = self.size - pos;
                pos += buffer.length;
                callback(&UserBuffer { address: buffer.address, length: cropped_size });
            } else {
                pos += buffer.length;
                callback(buffer);
            }
        })
    }
}

impl<'a> InputBuffer for CroppedInputBuffer<'a> {
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        if self.drained {
            return Ok(0);
        }
        let mut pos = self.base.bytes_read();
        self.base.peek_each(&mut |buf: &[u8]| {
            if pos >= self.size {
                return Ok(0);
            }
            let size = std::cmp::min(buf.len(), self.size - pos);
            pos += size;
            callback(&buf[..size])
        })
    }
    fn advance(&mut self, length: usize) -> Result<(), Errno> {
        if length > self.available() {
            return error!(EINVAL);
        }
        self.base.advance(length)
    }
    fn available(&self) -> usize {
        if self.drained || self.size < self.bytes_read() {
            0
        } else {
            self.size - self.bytes_read()
        }
    }
    fn bytes_read(&self) -> usize {
        self.base.bytes_read()
    }
    fn drain(&mut self) -> usize {
        let size = self.available();
        self.drained = true;
        size
    }
}

struct LoopDeviceFile {
    device: Arc<LoopDevice>,
}

impl FileOps for LoopDeviceFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if let Some(backing_file) = self.device.backing_file() {
            backing_file.read_at(
                locked,
                current_task,
                self.device.offset_for_backing_file(offset),
                data,
            )
        } else {
            Ok(0)
        }
    }

    fn write(
        &self,
        locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if let Some(backing_file) = self.device.backing_file() {
            let limit = self.device.state.lock().size_limit as usize;
            if offset >= limit {
                // Can't write past the size limit.
                return Ok(0);
            }
            let mut cropped_buf;
            let data = if offset + data.available() > limit {
                // If the write would exceed the size limit, then crop the input buffer to write
                // to the limit without exceeding it.
                let bytes_to_write = limit - offset;
                let cropped_size = data.bytes_read() + bytes_to_write;
                cropped_buf = CroppedInputBuffer::new(data, cropped_size);
                &mut cropped_buf
            } else {
                data
            };
            let r = backing_file.write_at(
                locked,
                current_task,
                self.device.offset_for_backing_file(offset),
                data,
            );
            r
        } else {
            error!(ENOSPC)
        }
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        requested_length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<Vmo>, Errno> {
        let backing_file = self.device.backing_file().ok_or_else(|| errno!(EBADF))?;

        let state = self.device.state.lock();
        let configured_offset = state.offset;
        let configured_size_limit = match state.size_limit {
            // If the size limit is 0, use all available bytes from the backing file.
            0 => None,
            n => Some(n),
        };

        let backing_vmo = backing_file.get_vmo(
            current_task,
            requested_length.map(|l| l + configured_offset as usize),
            prot,
        )?;
        let backing_vmo_size = backing_vmo.get_size().map_err(|e| errno!(EBADF, e))?;

        let slice_len = backing_vmo_size
            .min(configured_size_limit.unwrap_or(u64::MAX))
            .min(requested_length.unwrap_or(usize::MAX) as u64);

        let vmo_slice = backing_vmo
            .create_child(VmoChildOptions::SLICE, configured_offset, slice_len)
            .map_err(|e| errno!(EINVAL, e))?;

        let backing_content_size = backing_vmo.get_content_size().map_err(|e| errno!(EIO, e))?;
        if backing_content_size < slice_len {
            let new_content_size = backing_content_size.saturating_sub(configured_offset);
            vmo_slice.set_content_size(&new_content_size).map_err(|e| errno!(EIO, e))?;
        }

        Ok(Arc::new(vmo_slice))
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            BLKGETSIZE => {
                let user_size = UserRef::<u64>::from(arg);
                let state = self.device.state.lock();
                state.check_bound()?;
                let size = state.size_limit / (state.block_size as u64);
                std::mem::drop(state);
                current_task.write_object(user_size, &size)?;
                Ok(SUCCESS)
            }
            BLKGETSIZE64 => {
                let user_size = UserRef::<u64>::from(arg);
                let state = self.device.state.lock();
                state.check_bound()?;
                let size = state.size_limit;
                std::mem::drop(state);
                current_task.write_object(user_size, &size)?;
                Ok(SUCCESS)
            }
            BLKFLSBUF => {
                track_stub!(TODO("https://fxbug.dev/322873756"), "Loop device BLKFLSBUF");
                Ok(SUCCESS)
            }
            LOOP_SET_FD => {
                let fd = arg.into();
                let backing_file = current_task.files.get(fd)?;
                let mut state = self.device.state.lock();
                state.set_backing_file(current_task, backing_file)?;
                Ok(SUCCESS)
            }
            LOOP_CLR_FD => {
                let mut state = self.device.state.lock();
                state.check_bound()?;
                *state = Default::default();
                Ok(SUCCESS)
            }
            LOOP_SET_STATUS => {
                let modifiable_flags = LoopDeviceFlags::AUTOCLEAR | LoopDeviceFlags::PARTSCAN;

                let user_info = UserRef::<uapi::loop_info>::from(arg);
                let info = current_task.read_object(user_info)?;
                let flags = LoopDeviceFlags::from_bits_truncate(info.lo_flags as u32);
                let encrypt_key_size = info.lo_encrypt_key_size.clamp(0, LO_KEY_SIZE as i32);
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.flags = (state.flags & !modifiable_flags) | (flags & modifiable_flags);
                state.encrypt_type = info.lo_encrypt_type as u32;
                state.encrypt_key = info.lo_encrypt_key[0..(encrypt_key_size as usize)].to_owned();
                state.init = info.lo_init;
                state.offset = info.lo_offset as u64;
                std::mem::drop(state);
                Ok(SUCCESS)
            }
            LOOP_GET_STATUS => {
                let user_info = UserRef::<uapi::loop_info>::from(arg);
                let node = file.node();
                let (ino, rdev) = {
                    let info = node.info();
                    (info.ino, info.rdev)
                };
                let state = self.device.state.lock();
                state.check_bound()?;
                let info = loop_info {
                    lo_number: self.device.number as i32,
                    lo_device: node.dev().bits() as __kernel_old_dev_t,
                    lo_inode: ino,
                    lo_rdevice: rdev.bits() as __kernel_old_dev_t,
                    lo_offset: state.offset as i32,
                    lo_encrypt_type: state.encrypt_type as i32,
                    lo_flags: state.flags.bits() as i32,
                    lo_init: state.init,
                    ..Default::default()
                };
                std::mem::drop(state);
                current_task.write_object(user_info, &info)?;
                Ok(SUCCESS)
            }
            LOOP_CHANGE_FD => {
                let fd = arg.into();
                let backing_file = current_task.files.get(fd)?;
                let mut state = self.device.state.lock();
                if let Some(_existing_file) = &state.backing_file {
                    // https://man7.org/linux/man-pages/man4/loop.4.html says:
                    //
                    //   This operation is possible only if the loop device is read-only and the
                    //   new backing store is the same size and type as the old backing store.
                    if !state.flags.contains(LoopDeviceFlags::READ_ONLY) {
                        return error!(EINVAL);
                    }
                    track_stub!(
                        TODO("https://fxbug.dev/322874313"),
                        "check backing store size before change loop fd"
                    );
                    state.backing_file = Some(backing_file);
                    Ok(SUCCESS)
                } else {
                    error!(EINVAL)
                }
            }
            LOOP_SET_CAPACITY => {
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.update_size_limit(current_task)?;
                Ok(SUCCESS)
            }
            LOOP_SET_DIRECT_IO => {
                track_stub!(TODO("https://fxbug.dev/322873418"), "Loop device LOOP_SET_DIRECT_IO");
                error!(ENOTTY)
            }
            LOOP_SET_BLOCK_SIZE => {
                let block_size = arg.into();
                check_block_size(block_size)?;
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.block_size = block_size;
                Ok(SUCCESS)
            }
            LOOP_CONFIGURE => {
                let user_config = UserRef::<uapi::loop_config>::from(arg);
                let config = current_task.read_object(user_config)?;
                let fd = FdNumber::from_raw(config.fd as i32);
                check_block_size(config.block_size)?;
                let mut state = self.device.state.lock();
                if let Ok(backing_file) = current_task.files.get(fd) {
                    state.set_backing_file(current_task, backing_file)?;
                }
                state.block_size = config.block_size;
                state.set_info(&config.info);
                std::mem::drop(state);
                Ok(SUCCESS)
            }
            LOOP_SET_STATUS64 => {
                let user_info = UserRef::<uapi::loop_info64>::from(arg);
                let info = current_task.read_object(user_info)?;
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.set_info(&info);
                std::mem::drop(state);
                Ok(SUCCESS)
            }
            LOOP_GET_STATUS64 => {
                let user_info = UserRef::<uapi::loop_info64>::from(arg);
                let node = file.node();
                let (ino, rdev) = {
                    let info = node.info();
                    (info.ino, info.rdev)
                };
                let state = self.device.state.lock();
                state.check_bound()?;
                let info = loop_info64 {
                    lo_device: node.dev().bits(),
                    lo_inode: ino,
                    lo_rdevice: rdev.bits(),
                    lo_offset: state.offset as u64,
                    lo_sizelimit: state.size_limit,
                    lo_number: self.device.number,
                    lo_encrypt_type: state.encrypt_type,
                    lo_flags: state.flags.bits(),
                    lo_init: state.init,
                    ..Default::default()
                };
                std::mem::drop(state);
                current_task.write_object(user_info, &info)?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }
}

pub fn loop_device_init(locked: &mut Locked<'_, Unlocked>, current_task: &CurrentTask) {
    let kernel = current_task.kernel();

    // Device registry.
    kernel
        .device_registry
        .register_major(LOOP_MAJOR, get_or_create_loop_device, DeviceMode::Block)
        .expect("loop device register failed.");

    // Ensure initial loop devices.
    kernel.loop_device_registry.ensure_initial_devices(locked, current_task);
}

#[derive(Debug, Default)]
pub struct LoopDeviceRegistry {
    devices: Mutex<BTreeMap<u32, Arc<LoopDevice>>>,
}

impl LoopDeviceRegistry {
    /// Ensure initial loop devices.
    fn ensure_initial_devices<L>(&self, locked: &mut Locked<'_, L>, current_task: &CurrentTask)
    where
        L: LockBefore<FileOpsCore>,
    {
        for minor in 0..8 {
            self.get_or_create(locked, current_task, minor);
        }
    }

    fn get_or_create<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        minor: u32,
    ) -> Arc<LoopDevice>
    where
        L: LockBefore<FileOpsCore>,
    {
        self.devices
            .lock()
            .entry(minor)
            .or_insert_with(|| LoopDevice::new(locked, current_task, minor))
            .clone()
    }

    fn find<L>(&self, locked: &mut Locked<'_, L>, current_task: &CurrentTask) -> Result<u32, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let mut devices = self.devices.lock();
        for minor in 0..u32::MAX {
            match devices.entry(minor) {
                Entry::Vacant(e) => {
                    e.insert(LoopDevice::new(locked, current_task, minor));
                    return Ok(minor);
                }
                Entry::Occupied(e) => {
                    if !e.get().is_bound() {
                        return Ok(minor);
                    }
                }
            }
        }
        Err(errno!(ENODEV))
    }

    fn add<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        minor: u32,
    ) -> Result<(), Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(e) => {
                e.insert(LoopDevice::new(locked, current_task, minor));
                Ok(())
            }
            Entry::Occupied(_) => {
                error!(EEXIST)
            }
        }
    }

    fn remove(&self, minor: u32) -> Result<(), Errno> {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(_) => Ok(()),
            Entry::Occupied(e) => {
                if e.get().is_bound() {
                    return error!(EBUSY);
                }
                e.remove();
                Ok(())
            }
        }
    }
}

pub fn create_loop_control_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(LoopControlDevice::new(current_task.kernel().loop_device_registry.clone())))
}

struct LoopControlDevice {
    registry: Arc<LoopDeviceRegistry>,
}

impl LoopControlDevice {
    pub fn new(registry: Arc<LoopDeviceRegistry>) -> Self {
        Self { registry }
    }
}

impl FileOps for LoopControlDevice {
    fileops_impl_seekless!();
    fileops_impl_dataless!();

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            LOOP_CTL_GET_FREE => Ok(self.registry.find(locked, current_task)?.into()),
            LOOP_CTL_ADD => {
                let minor = arg.into();
                let registry = Arc::clone(&self.registry);
                // Delegate to the system task to have the permission to create the loop device.
                current_task.kernel().kthreads.spawner().spawn_and_get_result_sync(
                    move |locked, task| registry.add(locked, task, minor),
                )??;
                Ok(minor.into())
            }
            LOOP_CTL_REMOVE => {
                let minor = arg.into();
                self.registry.remove(minor)?;
                Ok(minor.into())
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }
}

fn get_or_create_loop_device(
    locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(current_task
        .kernel()
        .loop_device_registry
        .get_or_create(locked, current_task, id.minor())
        .create_file_ops())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fs::fuchsia::new_remote_file,
        testing::*,
        vfs::{
            buffers::*, Anon, DynamicFile, DynamicFileBuf, DynamicFileSource, FdFlags, FsNodeOps,
        },
    };
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_io as fio;
    use fuchsia_zircon as zx;

    #[derive(Clone)]
    struct PassthroughTestFile(Vec<u8>);

    impl PassthroughTestFile {
        pub fn new_node(bytes: &[u8]) -> impl FsNodeOps {
            DynamicFile::new_node(Self(bytes.to_owned()))
        }
    }

    impl DynamicFileSource for PassthroughTestFile {
        fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
            sink.write(&self.0);
            Ok(())
        }
    }

    fn bind_simple_loop_device(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        backing_file: FileHandle,
        open_flags: OpenFlags,
    ) -> FileHandle {
        let backing_fd = current_task
            .task
            .files
            .add_with_flags(&current_task, backing_file, FdFlags::empty())
            .unwrap();

        let loop_file = Anon::new_file(
            &current_task,
            Box::new(LoopDeviceFile { device: Arc::new(LoopDevice::default()) }),
            open_flags,
        );

        let config_addr = map_object_anywhere(
            &current_task,
            &uapi::loop_config {
                block_size: MIN_BLOCK_SIZE,
                fd: backing_fd.raw() as u32,
                ..Default::default()
            },
        );
        loop_file.ioctl(locked, &current_task, LOOP_CONFIGURE, config_addr.into()).unwrap();

        loop_file
    }

    #[::fuchsia::test]
    async fn basic_read() {
        let expected_contents = b"hello, world!";
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let backing_node = FsNode::new_root(PassthroughTestFile::new_node(expected_contents));
        let backing_file = Anon::new_file(
            &current_task,
            backing_node.create_file_ops(&mut locked, &current_task, OpenFlags::RDONLY).unwrap(),
            OpenFlags::RDONLY,
        );
        let loop_file =
            bind_simple_loop_device(&mut locked, &current_task, backing_file, OpenFlags::RDONLY);

        let mut buf = VecOutputBuffer::new(expected_contents.len());
        loop_file.read(&mut locked, &current_task, &mut buf).unwrap();

        assert_eq!(buf.data(), expected_contents);
    }

    #[::fuchsia::test]
    async fn offset_works() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let backing_node = FsNode::new_root(PassthroughTestFile::new_node(b"hello, world!"));
        let backing_file = Anon::new_file(
            &current_task,
            backing_node.create_file_ops(&mut locked, &current_task, OpenFlags::RDONLY).unwrap(),
            OpenFlags::RDONLY,
        );
        let loop_file =
            bind_simple_loop_device(&mut locked, &current_task, backing_file, OpenFlags::RDONLY);

        let info_addr = map_object_anywhere(
            &current_task,
            &uapi::loop_info64 { lo_offset: 3, ..Default::default() },
        );
        loop_file.ioctl(&mut locked, &current_task, LOOP_SET_STATUS64, info_addr.into()).unwrap();

        let mut buf = VecOutputBuffer::new(25);
        loop_file.read(&mut locked, &current_task, &mut buf).unwrap();

        assert_eq!(buf.data(), b"lo, world!");
    }

    #[::fuchsia::test]
    async fn basic_get_vmo() {
        let test_data_path = "/pkg/data/testfile.txt";
        let expected_contents = std::fs::read(test_data_path).unwrap();
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let txt_channel: zx::Channel =
            fuchsia_fs::file::open_in_namespace(test_data_path, fio::OpenFlags::RIGHT_READABLE)
                .unwrap()
                .into_channel()
                .unwrap()
                .into();

        let backing_file =
            new_remote_file(&current_task, txt_channel.into(), OpenFlags::RDONLY).unwrap();
        let loop_file =
            bind_simple_loop_device(&mut locked, &current_task, backing_file, OpenFlags::RDONLY);

        let vmo = loop_file.get_vmo(&current_task, None, ProtectionFlags::READ).unwrap();
        let size = vmo.get_content_size().unwrap();
        let vmo_contents = vmo.read_to_vec(0, size).unwrap();
        assert_eq!(vmo_contents, expected_contents);
    }

    #[::fuchsia::test]
    async fn get_vmo_offset_and_size_limit_work() {
        // VMO slice children require a page-aligned offset, so we need a file that's big enough to
        // have multiple pages to support creating a child with a meaningful offset, our own
        // binary should do the trick.
        let test_data_path = std::env::args().next().unwrap();
        let expected_offset = *PAGE_SIZE;
        let expected_size_limit = *PAGE_SIZE;
        let expected_contents = std::fs::read(&test_data_path).unwrap();
        let expected_contents = &expected_contents
            [expected_offset as usize..(expected_offset + expected_size_limit) as usize];

        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let txt_channel: zx::Channel =
            fuchsia_fs::file::open_in_namespace(&test_data_path, fio::OpenFlags::RIGHT_READABLE)
                .unwrap()
                .into_channel()
                .unwrap()
                .into();

        let backing_file =
            new_remote_file(&current_task, txt_channel.into(), OpenFlags::RDONLY).unwrap();
        let loop_file =
            bind_simple_loop_device(&mut locked, &current_task, backing_file, OpenFlags::RDONLY);

        let info_addr = map_object_anywhere(
            &current_task,
            &uapi::loop_info64 {
                lo_offset: expected_offset,
                lo_sizelimit: expected_size_limit,
                ..Default::default()
            },
        );
        loop_file.ioctl(&mut locked, &current_task, LOOP_SET_STATUS64, info_addr.into()).unwrap();

        let vmo = loop_file.get_vmo(&current_task, None, ProtectionFlags::READ).unwrap();
        let size = vmo.get_content_size().unwrap();
        let vmo_contents = vmo.read_to_vec(0, size).unwrap();
        assert_eq!(vmo_contents, expected_contents);
    }
}
