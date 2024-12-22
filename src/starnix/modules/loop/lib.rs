// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use starnix_core::device::kobject::{Device, DeviceMetadata};
use starnix_core::device::DeviceMode;
use starnix_core::fs::sysfs::{BlockDeviceDirectory, BlockDeviceInfo};
use starnix_core::mm::memory::MemoryObject;
use starnix_core::mm::{MemoryAccessorExt, ProtectionFlags, PAGE_SIZE};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::buffers::{InputBuffer, OutputBuffer};
use starnix_core::vfs::{
    default_ioctl, fileops_impl_dataless, fileops_impl_noop_sync, fileops_impl_seekable,
    fileops_impl_seekless, Buffer, FdNumber, FileHandle, FileObject, FileOps, FsNode, FsString,
    InputBufferCallback, PeekBufferSegmentsCallback,
};
use starnix_logging::track_stub;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::user_buffer::UserBuffer;
use starnix_uapi::device_type::{DeviceType, LOOP_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use starnix_uapi::{
    __kernel_old_dev_t, errno, error, loop_info, loop_info64, uapi, BLKFLSBUF, BLKGETSIZE,
    BLKGETSIZE64, LOOP_CHANGE_FD, LOOP_CLR_FD, LOOP_CONFIGURE, LOOP_CTL_ADD, LOOP_CTL_GET_FREE,
    LOOP_CTL_REMOVE, LOOP_GET_STATUS, LOOP_GET_STATUS64, LOOP_SET_BLOCK_SIZE, LOOP_SET_CAPACITY,
    LOOP_SET_DIRECT_IO, LOOP_SET_FD, LOOP_SET_STATUS, LOOP_SET_STATUS64, LO_FLAGS_AUTOCLEAR,
    LO_FLAGS_DIRECT_IO, LO_FLAGS_PARTSCAN, LO_FLAGS_READ_ONLY, LO_KEY_SIZE,
};
use std::collections::btree_map::{BTreeMap, Entry};
use std::sync::Arc;
use zx::VmoChildOptions;

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
    k_device: Option<Device>,

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
            k_device: Default::default(),
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
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        backing_file: FileHandle,
    ) -> Result<(), Errno> {
        if self.backing_file.is_some() {
            return error!(EBUSY);
        }
        self.backing_file = Some(backing_file);
        self.update_size_limit(locked, current_task)?;
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

    fn update_size_limit(
        &mut self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno> {
        if let Some(backing_file) = &self.backing_file {
            let backing_stat = backing_file.node().stat(locked, current_task)?;
            self.size_limit = backing_stat.st_size as u64;
        }
        Ok(())
    }

    fn set_k_device(&mut self, k_device: Device) {
        self.k_device = Some(k_device);
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
        let virtual_block_class = registry.objects.virtual_block_class();
        let device = Arc::new(Self { number: minor, state: Default::default() });
        let device_weak = Arc::<LoopDevice>::downgrade(&device);
        let k_device = registry.add_device(
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
        {
            let mut state = device.state.lock();
            state.set_k_device(k_device);
        }
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
        locked: &mut Locked<'_, FileOpsCore>,
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

    fn get_memory(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        requested_length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        let backing_file = self.device.backing_file().ok_or_else(|| errno!(EBADF))?;

        let state = self.device.state.lock();
        let configured_offset = state.offset;
        let configured_size_limit = match state.size_limit {
            // If the size limit is 0, use all available bytes from the backing file.
            0 => None,
            n => Some(n),
        };

        let backing_memory = backing_file.get_memory(
            locked,
            current_task,
            requested_length.map(|l| l + configured_offset as usize),
            prot,
        )?;
        let backing_memory_size = backing_memory.get_size();

        let slice_len = backing_memory_size
            .min(configured_size_limit.unwrap_or(u64::MAX))
            .min(requested_length.unwrap_or(usize::MAX) as u64);

        let memory_slice = backing_memory
            .create_child(VmoChildOptions::SLICE, configured_offset, slice_len)
            .map_err(|e| errno!(EINVAL, e))?;

        let backing_content_size = backing_memory.get_content_size();
        if backing_content_size < slice_len {
            let new_content_size = backing_content_size.saturating_sub(configured_offset);
            memory_slice.set_content_size(new_content_size);
        }

        Ok(Arc::new(memory_slice))
    }

    fn sync(&self, _file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        if let Some(f) = self.device.backing_file() {
            f.sync(current_task)
        } else {
            Ok(())
        }
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
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
                state.set_backing_file(locked, current_task, backing_file)?;
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
                state.update_size_limit(locked, current_task)?;
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
                    state.set_backing_file(locked, current_task, backing_file)?;
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
            _ => default_ioctl(file, locked, current_task, request, arg),
        }
    }
}

pub fn loop_device_init(locked: &mut Locked<'_, Unlocked>, current_task: &CurrentTask) {
    let kernel = current_task.kernel();

    // Device registry.
    kernel
        .device_registry
        .register_major("loop".into(), DeviceMode::Block, LOOP_MAJOR, get_or_create_loop_device)
        .expect("loop device register failed.");

    // Ensure initial loop devices.
    kernel.expando.get::<LoopDeviceRegistry>().ensure_initial_devices(locked, current_task);
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

    fn get(&self, minor: u32) -> Result<Arc<LoopDevice>, Errno> {
        match self.devices.lock().entry(minor) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(_) => return error!(ENODEV),
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

    fn remove<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        k_device: Option<Device>,
        minor: u32,
    ) -> Result<(), Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(_) => Ok(()),
            Entry::Occupied(e) => {
                if e.get().is_bound() {
                    return error!(EBUSY);
                }
                e.remove();
                let kernel = current_task.kernel();
                let registry = &kernel.device_registry;
                if let Some(dev) = &k_device {
                    registry.remove_device(locked, current_task, dev.clone());
                } else {
                    return Err(errno!(EINVAL));
                }
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
    Ok(Box::new(LoopControlDevice::new(current_task.kernel().expando.get::<LoopDeviceRegistry>())))
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
    fileops_impl_noop_sync!();

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
                let device = self.registry.get(minor)?;
                let k_device = {
                    let state = device.state.lock();
                    state.k_device.clone()
                };
                self.registry.remove(locked, current_task, k_device, minor)?;
                Ok(minor.into())
            }
            _ => default_ioctl(file, locked, current_task, request, arg),
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
        .expando
        .get::<LoopDeviceRegistry>()
        .get_or_create(locked, current_task, id.minor())
        .create_file_ops())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::Proxy;
    use fidl_fuchsia_io as fio;
    use starnix_core::fs::fuchsia::new_remote_file;
    use starnix_core::testing::*;
    use starnix_core::vfs::buffers::*;
    use starnix_core::vfs::{DynamicFile, DynamicFileBuf, DynamicFileSource, FdFlags, FsNodeOps};

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

        let loop_file = anon_test_file(
            &current_task,
            Box::new(LoopDeviceFile { device: Arc::new(LoopDevice::default()) }),
            open_flags,
        );

        let config_addr = map_object_anywhere(
            locked,
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
        spawn_kernel_and_run(|locked, current_task| {
            let expected_contents = b"hello, world!";

            let backing_node = FsNode::new_root(PassthroughTestFile::new_node(expected_contents));
            let backing_file = anon_test_file(
                current_task,
                backing_node.create_file_ops(locked, current_task, OpenFlags::RDONLY).unwrap(),
                OpenFlags::RDONLY,
            );
            let loop_file =
                bind_simple_loop_device(locked, current_task, backing_file, OpenFlags::RDONLY);

            let mut buf = VecOutputBuffer::new(expected_contents.len());
            loop_file.read(locked, current_task, &mut buf).unwrap();

            assert_eq!(buf.data(), expected_contents);
        });
    }

    #[::fuchsia::test]
    async fn offset_works() {
        spawn_kernel_and_run(|locked, current_task| {
            let backing_node = FsNode::new_root(PassthroughTestFile::new_node(b"hello, world!"));
            let backing_file = anon_test_file(
                current_task,
                backing_node.create_file_ops(locked, current_task, OpenFlags::RDONLY).unwrap(),
                OpenFlags::RDONLY,
            );
            let loop_file =
                bind_simple_loop_device(locked, current_task, backing_file, OpenFlags::RDONLY);

            let info_addr = map_object_anywhere(
                locked,
                current_task,
                &uapi::loop_info64 { lo_offset: 3, ..Default::default() },
            );
            loop_file.ioctl(locked, current_task, LOOP_SET_STATUS64, info_addr.into()).unwrap();

            let mut buf = VecOutputBuffer::new(25);
            loop_file.read(locked, current_task, &mut buf).unwrap();

            assert_eq!(buf.data(), b"lo, world!");
        });
    }

    #[ignore]
    #[::fuchsia::test]
    async fn basic_get_memory() {
        let test_data_path = "/pkg/data/testfile.txt";
        let expected_contents = std::fs::read(test_data_path).unwrap();

        let txt_channel: zx::Channel =
            fuchsia_fs::file::open_in_namespace(test_data_path, fio::PERM_READABLE)
                .unwrap()
                .into_channel()
                .unwrap()
                .into();

        spawn_kernel_and_run(move |locked, current_task| {
            let backing_file =
                new_remote_file(current_task, txt_channel.into(), OpenFlags::RDONLY).unwrap();
            let loop_file =
                bind_simple_loop_device(locked, current_task, backing_file, OpenFlags::RDONLY);

            let memory =
                loop_file.get_memory(locked, current_task, None, ProtectionFlags::READ).unwrap();
            let size = memory.get_content_size();
            let memory_contents = memory.read_to_vec(0, size).unwrap();
            assert_eq!(memory_contents, expected_contents);
        });
    }

    #[ignore]
    #[::fuchsia::test]
    async fn get_memory_offset_and_size_limit_work() {
        // VMO slice children require a page-aligned offset, so we need a file that's big enough to
        // have multiple pages to support creating a child with a meaningful offset, our own
        // binary should do the trick.
        let test_data_path = std::env::args().next().unwrap();
        let expected_offset = *PAGE_SIZE;
        let expected_size_limit = *PAGE_SIZE;
        let expected_contents = std::fs::read(&test_data_path).unwrap();
        let expected_contents = expected_contents
            [expected_offset as usize..(expected_offset + expected_size_limit) as usize]
            .to_vec();

        let txt_channel: zx::Channel =
            fuchsia_fs::file::open_in_namespace(&test_data_path, fio::PERM_READABLE)
                .unwrap()
                .into_channel()
                .unwrap()
                .into();

        spawn_kernel_and_run(move |locked, current_task| {
            let backing_file =
                new_remote_file(current_task, txt_channel.into(), OpenFlags::RDONLY).unwrap();
            let loop_file =
                bind_simple_loop_device(locked, current_task, backing_file, OpenFlags::RDONLY);

            let info_addr = map_object_anywhere(
                locked,
                current_task,
                &uapi::loop_info64 {
                    lo_offset: expected_offset,
                    lo_sizelimit: expected_size_limit,
                    ..Default::default()
                },
            );
            loop_file.ioctl(locked, current_task, LOOP_SET_STATUS64, info_addr.into()).unwrap();

            let memory =
                loop_file.get_memory(locked, current_task, None, ProtectionFlags::READ).unwrap();
            let size = memory.get_content_size();
            let memory_contents = memory.read_to_vec(0, size).unwrap();
            assert_eq!(memory_contents, expected_contents);
        });
    }
}
