// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::memory::MemoryObject;
use crate::mm::{DesiredAddress, MappingName, MappingOptions, ProtectionFlags};
use crate::task::CurrentTask;
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle,
    FileObject, FileOps, FileWriteGuardRef, NamespaceNode,
};
use fuchsia_zircon as zx;
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    errno, error, io_cqring_offsets, io_sqring_offsets, io_uring_cqe, io_uring_params,
    io_uring_sqe, IORING_FEAT_SINGLE_MMAP, IORING_OFF_CQ_RING, IORING_OFF_SQES, IORING_OFF_SQ_RING,
    IORING_SETUP_CQSIZE,
};
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

// See https://github.com/google/gvisor/blob/master/pkg/abi/linux/iouring.go#L47
const IORING_MAX_ENTRIES: u32 = 1 << 15; // 32768
const IORING_MAX_CQ_ENTRIES: u32 = 2 * IORING_MAX_ENTRIES;

type RingIndex = u32;

/// The control header at the start of the shared buffer.
///
/// This structure is not declared in the Linux UAPI. Instead, userspace learns about its structure
/// from the SQ and CQ offsets returned by `io_uring_setup()`.
///
/// We determined this structure by running `io_uring_setup()` and observing the placement of each
/// field. The total size of the structure is 64 bytes, which we determined by looking at the
/// offset of the cqes offset. It's likely that many of the bytes at the end of this structure are
/// just padding for alignment.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, IntoBytes, FromBytes, KnownLayout, Immutable)]
struct ControlHeader {
    sq_head: u32,
    sq_tail: u32,
    cq_head: u32,
    cq_tail: u32,
    sq_ring_mask: u32,
    cq_ring_mask: u32,
    sq_ring_entries: u32,
    cq_ring_entries: u32,
    sq_dropped: u32,
    sq_flags: u32,
    cq_flags: u32,
    cq_overflow: u32,
    _padding: [u8; 16],
}

// From params.cq_off.cqes reported by sys_io_uring_setup.
static_assertions::const_assert_eq!(std::mem::size_of::<ControlHeader>(), 64);

pub struct IoUringFileObject {
    ring_buffer: Arc<MemoryObject>,
    sq_entries: Arc<MemoryObject>,
}

impl IoUringFileObject {
    pub fn new_file(
        current_task: &CurrentTask,
        entries: u32,
        params: &mut io_uring_params,
    ) -> Result<FileHandle, Errno> {
        if entries > IORING_MAX_ENTRIES {
            return error!(EINVAL);
        }

        let sq_entries = entries.next_power_of_two();
        let cq_entries = if params.flags & IORING_SETUP_CQSIZE != 0 {
            let cq_entries = params.cq_entries;
            if cq_entries > IORING_MAX_CQ_ENTRIES {
                return error!(EINVAL);
            }
            cq_entries
        } else {
            sq_entries * 2
        };

        let cqes_offset = std::mem::size_of::<ControlHeader>();
        let array_offset =
            cqes_offset + (cq_entries as usize) * std::mem::size_of::<io_uring_cqe>();
        let ring_buffer_size =
            array_offset + (sq_entries as usize) * std::mem::size_of::<RingIndex>();
        let sq_entries_size = (sq_entries as usize) * std::mem::size_of::<io_uring_sqe>();

        let ring_buffer_vmo =
            zx::Vmo::create(ring_buffer_size as u64).map_err(|_| errno!(ENOMEM))?;
        let sq_entries_vmo = zx::Vmo::create(sq_entries_size as u64).map_err(|_| errno!(ENOMEM))?;

        let header = ControlHeader {
            sq_ring_mask: sq_entries - 1,
            cq_ring_mask: cq_entries - 1,
            sq_ring_entries: sq_entries,
            cq_ring_entries: cq_entries,
            ..Default::default()
        };
        ring_buffer_vmo.write(header.as_bytes(), 0).map_err(|_| errno!(ENOMEM))?;

        params.sq_entries = sq_entries;
        params.cq_entries = cq_entries;
        params.features = IORING_FEAT_SINGLE_MMAP;
        params.sq_off = io_sqring_offsets {
            head: std::mem::offset_of!(ControlHeader, sq_head) as u32,
            tail: std::mem::offset_of!(ControlHeader, sq_tail) as u32,
            ring_mask: std::mem::offset_of!(ControlHeader, sq_ring_mask) as u32,
            ring_entries: std::mem::offset_of!(ControlHeader, sq_ring_entries) as u32,
            flags: std::mem::offset_of!(ControlHeader, sq_flags) as u32,
            dropped: std::mem::offset_of!(ControlHeader, sq_dropped) as u32,
            array: array_offset as u32,
            ..Default::default()
        };
        params.cq_off = io_cqring_offsets {
            head: std::mem::offset_of!(ControlHeader, cq_head) as u32,
            tail: std::mem::offset_of!(ControlHeader, cq_tail) as u32,
            ring_mask: std::mem::offset_of!(ControlHeader, cq_ring_mask) as u32,
            ring_entries: std::mem::offset_of!(ControlHeader, cq_ring_entries) as u32,
            overflow: std::mem::offset_of!(ControlHeader, cq_overflow) as u32,
            cqes: cqes_offset as u32,
            flags: std::mem::offset_of!(ControlHeader, cq_flags) as u32,
            ..Default::default()
        };

        let object = Box::new(IoUringFileObject {
            ring_buffer: Arc::new(ring_buffer_vmo.into()),
            sq_entries: Arc::new(sq_entries_vmo.into()),
        });
        Ok(Anon::new_file(current_task, object, OpenFlags::RDWR))
    }
}

impl FileOps for IoUringFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
    fileops_impl_dataless!();

    fn mmap(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        if !options.contains(MappingOptions::SHARED) {
            return error!(EINVAL);
        }
        let magic_offset: u32 = memory_offset.try_into().map_err(|_| errno!(EINVAL))?;
        let memory = match magic_offset {
            IORING_OFF_SQ_RING | IORING_OFF_CQ_RING => self.ring_buffer.clone(),
            IORING_OFF_SQES => self.sq_entries.clone(),
            _ => return error!(EINVAL),
        };
        current_task.mm().map_memory(
            addr,
            memory,
            0,
            length,
            prot_flags,
            options,
            MappingName::File(filename.into_active()),
            FileWriteGuardRef(None),
        )
    }
}
