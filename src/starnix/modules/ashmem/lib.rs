// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::{
    ASHMEM_GET_NAME, ASHMEM_GET_PIN_STATUS, ASHMEM_GET_PROT_MASK, ASHMEM_GET_SIZE,
    ASHMEM_IS_PINNED, ASHMEM_IS_UNPINNED, ASHMEM_NOT_PURGED, ASHMEM_PIN, ASHMEM_PURGE_ALL_CACHES,
    ASHMEM_SET_NAME, ASHMEM_SET_PROT_MASK, ASHMEM_SET_SIZE, ASHMEM_UNPIN, ASHMEM_WAS_PURGED,
};
use once_cell::sync::OnceCell;
use range_map::RangeMap;
use starnix_core::device::DeviceOps;
use starnix_core::mm::memory::MemoryObject;
use starnix_core::mm::{
    DesiredAddress, MappingName, MappingOptions, MemoryAccessor, MemoryAccessorExt,
    ProtectionFlags, PAGE_SIZE,
};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    default_ioctl, default_seek, fileops_impl_noop_sync, FileObject, FileOps, FileWriteGuardRef,
    FsNode, FsString, InputBuffer, NamespaceNode, OutputBuffer, SeekTarget,
};
use starnix_lifecycle::AtomicU32Counter;
use starnix_sync::{DeviceOpen, FileOpsCore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::errors::Errno;
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::{
    ashmem_pin, device_type, errno, error, off_t, ASHMEM_GET_FILE_ID, ASHMEM_NAME_LEN,
};
use std::sync::Arc;

/// Initializes the ashmem device.
pub fn ashmem_device_init(locked: &mut Locked<'_, Unlocked>, system_task: &CurrentTask) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    registry
        .register_misc_device(locked, system_task, "ashmem".into(), AshmemDevice::new())
        .expect("can register ashmem");
}

#[derive(Clone)]
pub struct AshmemDevice {
    pub next_id: Arc<AtomicU32Counter>,
}

pub struct Ashmem {
    memory: OnceCell<Arc<MemoryObject>>,
    state: Mutex<AshmemState>,
}

struct AshmemState {
    size: usize,
    name: FsString,
    prot_flags: ProtectionFlags,
    unpinned: RangeMap<u32, bool>,
    id: u32,
}

impl AshmemDevice {
    pub fn new() -> AshmemDevice {
        AshmemDevice { next_id: Arc::new(AtomicU32Counter::new(1)) }
    }
}

impl DeviceOps for AshmemDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let ashmem = Ashmem::new(self.next_id.next());
        Ok(Box::new(ashmem))
    }
}

impl Ashmem {
    fn new(id: u32) -> Ashmem {
        let state = AshmemState {
            size: 0,
            name: b"dev/ashmem\0".into(),
            prot_flags: ProtectionFlags::ACCESS_FLAGS,
            unpinned: RangeMap::<u32, bool>::new(),
            id: id,
        };

        Ashmem { memory: OnceCell::new(), state: Mutex::new(state) }
    }

    fn memory(&self) -> Result<&Arc<MemoryObject>, Errno> {
        self.memory.get().ok_or_else(|| errno!(EINVAL))
    }

    fn is_mapped(&self) -> bool {
        self.memory.get().is_some()
    }
}

impl FileOps for Ashmem {
    fileops_impl_noop_sync!();

    fn is_seekable(&self) -> bool {
        true
    }

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        if !self.is_mapped() {
            return error!(EBADF);
        }
        let eof_offset = self.state.lock().size;
        default_seek(current_offset, target, |offset| {
            offset.checked_add(eof_offset.try_into().unwrap()).ok_or_else(|| errno!(EINVAL))
        })
    }

    fn read(
        &self,
        _locked: &mut starnix_sync::Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let memory = self.memory().map_err(|_| errno!(EBADF))?;
        let file_length = self.state.lock().size;
        let actual = {
            let want_read = data.available();
            if offset < file_length {
                let to_read =
                    if file_length < offset + want_read { file_length - offset } else { want_read };
                let buf =
                    memory.read_to_vec(offset as u64, to_read as u64).map_err(|_| errno!(EIO))?;
                data.write_all(&buf[..])?;
                to_read
            } else {
                0
            }
        };
        Ok(actual)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }

    fn mmap(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        mapping_options: MappingOptions,
        _filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        let state = self.state.lock();
        let size_paged_aligned = round_up_to_increment(state.size, *PAGE_SIZE as usize)?;

        // Filter protections
        if !state.prot_flags.contains(prot_flags) {
            return error!(EINVAL);
        }
        // Filter size
        if size_paged_aligned < length {
            return error!(EINVAL);
        }

        let memory = self
            .memory
            .get_or_try_init(|| {
                if size_paged_aligned == 0 {
                    return error!(EINVAL);
                }
                // Round up to page boundary
                let vmo = zx::Vmo::create(size_paged_aligned as u64).map_err(|_| errno!(ENOMEM))?;
                let memory = MemoryObject::from(vmo);
                Ok(Arc::new(memory))
            })?
            .clone();

        let mapped_addr = current_task.mm().ok_or_else(|| errno!(EINVAL))?.map_memory(
            addr,
            memory,
            memory_offset,
            length,
            prot_flags,
            file.max_access_for_memory_mapping(),
            mapping_options,
            MappingName::Ashmem(state.name.clone()),
            FileWriteGuardRef(None),
        )?;

        Ok(mapped_addr)
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
            ASHMEM_SET_SIZE => {
                let mut state = self.state.lock();

                if self.is_mapped() {
                    return error!(EINVAL);
                }
                state.size = arg.into();
                Ok(SUCCESS)
            }
            ASHMEM_GET_SIZE => Ok(self.state.lock().size.into()),
            ASHMEM_SET_NAME => {
                let mut state = self.state.lock();

                if self.is_mapped() {
                    return error!(EINVAL);
                }
                let mut name =
                    current_task.read_c_string_to_vec(arg.into(), ASHMEM_NAME_LEN as usize)?;
                name.push(0); // Add a null terminator

                state.name = name.into();
                Ok(SUCCESS)
            }
            ASHMEM_GET_NAME => {
                let state = self.state.lock();
                let name = &state.name[..];

                current_task.write_memory(arg.into(), name)?;
                Ok(SUCCESS)
            }
            ASHMEM_SET_PROT_MASK => {
                let mut state = self.state.lock();
                let prot_flags =
                    ProtectionFlags::from_access_bits(arg.into()).ok_or_else(|| errno!(EINVAL))?;

                // Do not allow protections to be increased
                if !state.prot_flags.contains(prot_flags) {
                    return error!(EINVAL);
                }

                state.prot_flags = prot_flags;
                Ok(SUCCESS)
            }
            ASHMEM_GET_PROT_MASK => Ok(self.state.lock().prot_flags.bits().into()),
            ASHMEM_PIN | ASHMEM_UNPIN | ASHMEM_GET_PIN_STATUS => {
                let mut state = self.state.lock();

                if !self.is_mapped() {
                    return error!(EINVAL);
                }

                let user_ref = UserRef::<ashmem_pin>::new(arg.into());
                let pin = current_task.read_object(user_ref)?;
                let (lo, hi) =
                    (pin.offset, pin.offset.checked_add(pin.len).ok_or_else(|| errno!(EFAULT))?);

                // Bounds check
                if (lo as usize) >= state.size || (hi as usize) > state.size {
                    return error!(EINVAL);
                }

                // Aligned to page size
                if (lo as u64) % *PAGE_SIZE != 0 || (hi as u64) % *PAGE_SIZE != 0 {
                    return error!(EINVAL);
                }

                match request {
                    ASHMEM_PIN => {
                        for is_purged in state.unpinned.remove(&(lo..hi)).iter() {
                            if *is_purged {
                                return Ok(ASHMEM_WAS_PURGED.into());
                            }
                        }

                        return Ok(ASHMEM_NOT_PURGED.into());
                    }
                    ASHMEM_UNPIN => {
                        state.unpinned.insert(lo..hi, false);
                        return Ok(ASHMEM_IS_UNPINNED.into());
                    }
                    ASHMEM_GET_PIN_STATUS => {
                        let mut intervals = state.unpinned.intersection(lo..hi);
                        return match intervals.next() {
                            Some(_) => Ok(ASHMEM_IS_UNPINNED.into()),
                            None => Ok(ASHMEM_IS_PINNED.into()),
                        };
                    }
                    _ => unreachable!(),
                }
            }
            ASHMEM_PURGE_ALL_CACHES => {
                let mut state = self.state.lock();
                let memory = self.memory.get().ok_or_else(|| errno!(EINVAL))?;

                if state.unpinned.is_empty() {
                    return Ok(ASHMEM_IS_PINNED.into());
                }
                for (range, is_purged) in state.unpinned.iter_mut() {
                    let (lo, hi) = (range.start as u64, range.end as u64);
                    *is_purged = true;
                    memory.op_range(zx::VmoOp::ZERO, lo, hi - lo).unwrap_or(());
                }
                return Ok(ASHMEM_IS_UNPINNED.into());
            }
            ASHMEM_GET_FILE_ID => {
                let state = self.state.lock();
                current_task.write_object(arg.into(), &(state.id))?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(file, locked, current_task, request, arg),
        }
    }
}
