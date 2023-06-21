// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::Mutex,
    mm::*,
    syscalls::*,
    task::*,
    types::*,
};
use bitflags::bitflags;
use std::{
    collections::btree_map::{BTreeMap, Entry},
    sync::Arc,
};

// See LOOP_SET_BLOCK_SIZE in <https://man7.org/linux/man-pages/man4/loop.4.html>.
const MIN_BLOCK_SIZE: u32 = 512;

bitflags! {
    #[derive(Default)]
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

    fn set_backing_file(&mut self, backing_file: FileHandle) -> Result<(), Errno> {
        if self.backing_file.is_some() {
            return error!(EBUSY);
        }
        self.backing_file = Some(backing_file);
        self.update_size_limit();
        Ok(())
    }

    fn set_info(&mut self, info: &uapi::loop_info64) {
        let encrypt_key_size = info.lo_encrypt_key_size.clamp(0, LO_KEY_SIZE);
        self.size_limit = info.lo_sizelimit;
        self.flags = LoopDeviceFlags::from_bits_truncate(info.lo_flags);
        self.encrypt_type = info.lo_encrypt_type;
        self.encrypt_key = info.lo_encrypt_key[0..(encrypt_key_size as usize)].to_owned();
        self.init = info.lo_init;
    }

    fn update_size_limit(&mut self) {
        if let Some(backing_file) = &self.backing_file {
            self.size_limit = backing_file.node().info().size as u64;
        }
    }
}

#[derive(Debug, Default)]
struct LoopDevice {
    number: u32,
    state: Mutex<LoopDeviceState>,
}

impl LoopDevice {
    fn new(minor: u32) -> Arc<Self> {
        Arc::new(LoopDevice { number: minor, state: Default::default() })
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

struct LoopDeviceFile {
    device: Arc<LoopDevice>,
}

impl FileOps for LoopDeviceFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if let Some(backing_file) = self.device.backing_file() {
            backing_file.read_at(current_task, offset, data)
        } else {
            Ok(0)
        }
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if let Some(backing_file) = self.device.backing_file() {
            backing_file.write_at(current_task, offset, data)
        } else {
            error!(ENOSPC)
        }
    }

    fn ioctl(
        &self,
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
                current_task.mm.write_object(user_size, &size)?;
                Ok(SUCCESS)
            }
            BLKGETSIZE64 => {
                let user_size = UserRef::<u64>::from(arg);
                let state = self.device.state.lock();
                state.check_bound()?;
                let size = state.size_limit;
                std::mem::drop(state);
                current_task.mm.write_object(user_size, &size)?;
                Ok(SUCCESS)
            }
            LOOP_SET_FD => {
                let fd = arg.into();
                let backing_file = current_task.files.get_unless_opath(fd)?;
                let mut state = self.device.state.lock();
                state.set_backing_file(backing_file)?;
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
                let info = current_task.mm.read_object(user_info)?;
                let flags = LoopDeviceFlags::from_bits_truncate(info.lo_flags as u32);
                let encrypt_key_size = info.lo_encrypt_key_size.clamp(0, LO_KEY_SIZE as i32);
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.flags = (state.flags & !modifiable_flags) | (flags & modifiable_flags);
                state.encrypt_type = info.lo_encrypt_type as u32;
                state.encrypt_key = info.lo_encrypt_key[0..(encrypt_key_size as usize)].to_owned();
                state.init = info.lo_init;
                std::mem::drop(state);
                *file.offset.lock() = info.lo_offset as i64;
                Ok(SUCCESS)
            }
            LOOP_GET_STATUS => {
                let user_info = UserRef::<uapi::loop_info>::from(arg);
                let node = file.node();
                let (ino, rdev) = {
                    let info = node.info();
                    (info.ino, info.rdev)
                };
                let offset = *file.offset.lock();
                let state = self.device.state.lock();
                state.check_bound()?;
                let info = loop_info {
                    lo_number: self.device.number as i32,
                    lo_device: node.dev().bits() as __kernel_old_dev_t,
                    lo_inode: ino,
                    lo_rdevice: rdev.bits() as __kernel_old_dev_t,
                    lo_offset: offset as i32,
                    lo_encrypt_type: state.encrypt_type as i32,
                    lo_flags: state.flags.bits() as i32,
                    lo_init: state.init,
                    ..Default::default()
                };
                std::mem::drop(state);
                current_task.mm.write_object(user_info, &info)?;
                Ok(SUCCESS)
            }
            LOOP_CHANGE_FD => {
                let fd = arg.into();
                let backing_file = current_task.files.get_unless_opath(fd)?;
                let mut state = self.device.state.lock();
                if let Some(_existing_file) = &state.backing_file {
                    // https://man7.org/linux/man-pages/man4/loop.4.html says:
                    //
                    //   This operation is possible only if the loop device is read-only and the
                    //   new backing store is the same size and type as the old backing store.
                    //
                    // TODO: Add a check for the backing store size, once we know what that means.
                    if !state.flags.contains(LoopDeviceFlags::READ_ONLY) {
                        return error!(EINVAL);
                    }
                    state.backing_file = Some(backing_file);
                    Ok(SUCCESS)
                } else {
                    error!(EINVAL)
                }
            }
            LOOP_SET_CAPACITY => {
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.update_size_limit();
                Ok(SUCCESS)
            }
            LOOP_SET_DIRECT_IO => {
                not_implemented!("Loop device does not implement LOOP_SET_DIRECT_IO");
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
                let config = current_task.mm.read_object(user_config)?;
                let fd = FdNumber::from_raw(config.fd as i32);
                let backing_file = current_task.files.get_unless_opath(fd)?;
                check_block_size(config.block_size)?;
                let mut state = self.device.state.lock();
                state.set_backing_file(backing_file)?;
                state.block_size = config.block_size;
                state.set_info(&config.info);
                std::mem::drop(state);
                *file.offset.lock() = config.info.lo_offset as i64;
                Ok(SUCCESS)
            }
            LOOP_SET_STATUS64 => {
                let user_info = UserRef::<uapi::loop_info64>::from(arg);
                let info = current_task.mm.read_object(user_info)?;
                let mut state = self.device.state.lock();
                state.check_bound()?;
                state.set_info(&info);
                std::mem::drop(state);
                *file.offset.lock() = info.lo_offset as i64;
                Ok(SUCCESS)
            }
            LOOP_GET_STATUS64 => {
                let user_info = UserRef::<uapi::loop_info64>::from(arg);
                let node = file.node();
                let (ino, rdev) = {
                    let info = node.info();
                    (info.ino, info.rdev)
                };
                let offset = *file.offset.lock();
                let state = self.device.state.lock();
                state.check_bound()?;
                let info = loop_info64 {
                    lo_device: node.dev().bits(),
                    lo_inode: ino,
                    lo_rdevice: rdev.bits(),
                    lo_offset: offset as u64,
                    lo_sizelimit: state.size_limit,
                    lo_number: self.device.number,
                    lo_encrypt_type: state.encrypt_type,
                    lo_flags: state.flags.bits(),
                    lo_init: state.init,
                    ..Default::default()
                };
                std::mem::drop(state);
                current_task.mm.write_object(user_info, &info)?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }
}

#[derive(Debug, Default)]
pub struct LoopDeviceRegistry {
    devices: Mutex<BTreeMap<u32, Arc<LoopDevice>>>,
}

impl LoopDeviceRegistry {
    fn get_or_create(&self, minor: u32) -> Arc<LoopDevice> {
        self.devices.lock().entry(minor).or_insert_with(|| LoopDevice::new(minor)).clone()
    }

    fn find(&self) -> Result<u32, Errno> {
        let mut devices = self.devices.lock();
        let mut minor = 0;
        loop {
            match devices.entry(minor) {
                Entry::Vacant(e) => {
                    e.insert(LoopDevice::new(minor));
                    return Ok(minor);
                }
                Entry::Occupied(e) => {
                    if e.get().is_bound() {
                        minor += 1;
                        continue;
                    }
                    return Ok(minor);
                }
            }
        }
    }

    fn add(&self, minor: u32) -> Result<(), Errno> {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(e) => {
                e.insert(LoopDevice::new(minor));
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

    fn ensure_initial_devices(&self) {
        for minor in 0..8 {
            self.get_or_create(minor);
        }
    }
}

pub struct LoopControlDevice {
    registry: Arc<LoopDeviceRegistry>,
}

impl LoopControlDevice {
    pub fn create_file_ops(kernel: &Kernel) -> Box<dyn FileOps> {
        let registry = kernel.loop_device_registry.clone();
        registry.ensure_initial_devices();
        Box::new(LoopControlDevice { registry })
    }
}

impl FileOps for LoopControlDevice {
    fileops_impl_seekless!();
    fileops_impl_dataless!();

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            LOOP_CTL_GET_FREE => Ok(self.registry.find()?.into()),
            LOOP_CTL_ADD => {
                let minor = arg.into();
                self.registry.add(minor)?;
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

pub fn create_loop_device(
    current_task: &CurrentTask,
    id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(current_task.kernel().loop_device_registry.get_or_create(id.minor()).create_file_ops())
}
