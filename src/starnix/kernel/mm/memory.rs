// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{PAGE_SIZE, ZX_VM_SPECIFIC_OVERWRITE};

use starnix_logging::{impossible_error, set_zx_name};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::mem::MaybeUninit;
use zerocopy::FromBytes;
use zx::{AsHandleRef, HandleBased, Koid};

#[derive(Debug, PartialEq, Eq)]
pub enum MemoryObject {
    Vmo(zx::Vmo),
    /// The memory object is a bpf ring buffer. The layout it represents is:
    /// |Page1 - Page2 - Page3 .. PageN - Page3 .. PageN| where the vmo is
    /// |Page1 - Page2 - Page3 .. PageN|
    RingBuf(zx::Vmo),
}

impl From<zx::Vmo> for MemoryObject {
    fn from(vmo: zx::Vmo) -> Self {
        Self::Vmo(vmo)
    }
}

impl MemoryObject {
    pub fn as_vmo(&self) -> Option<&zx::Vmo> {
        match self {
            Self::Vmo(vmo) => Some(&vmo),
            Self::RingBuf(_) => None,
        }
    }

    pub fn into_vmo(self) -> Option<zx::Vmo> {
        match self {
            Self::Vmo(vmo) => Some(vmo),
            Self::RingBuf(_) => None,
        }
    }

    pub fn get_content_size(&self) -> u64 {
        match self {
            Self::Vmo(vmo) => vmo.get_content_size().map_err(impossible_error).unwrap(),
            Self::RingBuf(_) => self.get_size(),
        }
    }

    pub fn set_content_size(&self, size: u64) {
        match self {
            Self::Vmo(vmo) => vmo.set_content_size(&size).map_err(impossible_error).unwrap(),
            Self::RingBuf(_) => {}
        }
    }

    pub fn get_size(&self) -> u64 {
        match self {
            Self::Vmo(vmo) => vmo.get_size().map_err(impossible_error).unwrap(),
            Self::RingBuf(vmo) => {
                let base_size = vmo.get_size().map_err(impossible_error).unwrap();
                (base_size - *PAGE_SIZE) * 2
            }
        }
    }

    pub fn set_size(&self, size: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.set_size(size),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn create_child(
        &self,
        option: zx::VmoChildOptions,
        offset: u64,
        size: u64,
    ) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.create_child(option, offset, size).map(Self::from),
            Self::RingBuf(vmo) => vmo.create_child(option, offset, size).map(Self::RingBuf),
        }
    }

    pub fn duplicate_handle(&self, rights: zx::Rights) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.duplicate_handle(rights).map(Self::from),
            Self::RingBuf(vmo) => vmo.duplicate_handle(rights).map(Self::RingBuf),
        }
    }

    pub fn read(&self, data: &mut [u8], offset: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read(data, offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn read_to_array<T: FromBytes, const N: usize>(
        &self,
        offset: u64,
    ) -> Result<[T; N], zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_to_array(offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn read_to_vec(&self, offset: u64, length: u64) -> Result<Vec<u8>, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_to_vec(offset, length),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn read_uninit<'a>(
        &self,
        data: &'a mut [MaybeUninit<u8>],
        offset: u64,
    ) -> Result<&'a mut [u8], zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_uninit(data, offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    /// Reads from the memory.
    ///
    /// # Safety
    ///
    /// Callers must guarantee that the buffer is valid to write to.
    pub unsafe fn read_raw(
        &self,
        buffer: *mut u8,
        buffer_length: usize,
        offset: u64,
    ) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_raw(buffer, buffer_length, offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn write(&self, data: &[u8], offset: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.write(data, offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn basic_info(&self) -> zx::HandleBasicInfo {
        match self {
            Self::Vmo(vmo) | Self::RingBuf(vmo) => {
                vmo.basic_info().map_err(impossible_error).unwrap()
            }
        }
    }

    pub fn get_koid(&self) -> Koid {
        self.basic_info().koid
    }

    pub fn info(&self) -> Result<zx::VmoInfo, Errno> {
        match self {
            Self::Vmo(vmo) | Self::RingBuf(vmo) => vmo.info().map_err(|_| errno!(EIO)),
        }
    }

    pub fn set_zx_name(&self, name: &[u8]) {
        match self {
            Self::Vmo(vmo) | Self::RingBuf(vmo) => set_zx_name(vmo, name),
        }
    }

    pub fn with_zx_name(self, name: &[u8]) -> Self {
        self.set_zx_name(name);
        self
    }

    pub fn op_range(
        &self,
        op: zx::VmoOp,
        mut offset: u64,
        mut size: u64,
    ) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.op_range(op, offset, size),
            Self::RingBuf(vmo) => {
                let vmo_size = vmo.get_size().map_err(impossible_error).unwrap();
                let data_size = vmo_size - (2 * *PAGE_SIZE);
                let memory_size = vmo_size + data_size;
                if offset + size > memory_size {
                    return Err(zx::Status::OUT_OF_RANGE);
                }
                // If `offset` is greater than `vmo_size`, the operation is equivalent to the one
                // done on the first part of the memory range.
                if offset >= vmo_size {
                    offset -= data_size;
                }
                // If the operation spill over the end if the vmo, it must be done on the start of
                // the data part of the vmo.
                if offset + size > vmo_size {
                    vmo.op_range(op, 2 * *PAGE_SIZE, offset + size - vmo_size)?;
                    size = vmo_size - offset;
                }
                vmo.op_range(op, offset, size)
            }
        }
    }

    pub fn replace_as_executable(self, vmex: &zx::Resource) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.replace_as_executable(vmex).map(Self::from),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }

    pub fn map_in_vmar(
        &self,
        vmar: &zx::Vmar,
        vmar_offset: usize,
        mut memory_offset: u64,
        len: usize,
        flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmar.map(vmar_offset, vmo, memory_offset, len, flags),
            Self::RingBuf(vmo) => {
                let vmo_size = vmo.get_size().map_err(impossible_error).unwrap();
                let data_size = vmo_size - (2 * *PAGE_SIZE);
                let memory_size = vmo_size + data_size;
                if memory_offset.checked_add(len as u64).ok_or_else(|| zx::Status::OUT_OF_RANGE)?
                    > memory_size
                {
                    return Err(zx::Status::OUT_OF_RANGE);
                }
                // If `memory_offset` is greater than `vmo_size`, the operation is equivalent to
                // the one done on the first part of the memory range.
                if memory_offset >= vmo_size {
                    memory_offset -= data_size;
                }
                // Map the vmo for the full length. This ensures the kernel will choose a range
                // that can accommodate the full length so that the second mapping will not erase
                // another mapping.
                let result = vmar.map(
                    vmar_offset,
                    vmo,
                    memory_offset,
                    len,
                    flags | zx::VmarFlags::ALLOW_FAULTS,
                )?;
                // The maximal amount of data that can have been mapped from the vmo with the
                // previous operation.
                let max_mapped_len = (vmo_size - memory_offset) as usize;
                // If more data is needed, the data part of the vmo must be mapped again, replacing
                // the part of the previous mapping that contained no data.
                if len > max_mapped_len {
                    let vmar_info = vmar.info().map_err(|_| errno!(EIO))?;
                    let base_address = vmar_info.base;
                    // The request should map the data part of the vmo a second time
                    let second_mapping_address = vmar
                        .map(
                            result + max_mapped_len - base_address,
                            vmo,
                            2 * *PAGE_SIZE,
                            len - max_mapped_len,
                            flags | ZX_VM_SPECIFIC_OVERWRITE,
                        )
                        .expect("Mapping should not fail as the space has been reserved");
                    debug_assert_eq!(second_mapping_address, result + max_mapped_len);
                }
                Ok(result)
            }
        }
    }

    pub fn memmove(
        &self,
        options: zx::TransferDataOptions,
        dst_offset: u64,
        src_offset: u64,
        size: u64,
    ) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.transfer_data(options, dst_offset, size, vmo, src_offset),
            Self::RingBuf(_) => Err(zx::Status::NOT_SUPPORTED),
        }
    }
}
