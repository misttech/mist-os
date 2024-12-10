// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::mm::memory::MemoryObject;
use crate::mm::{
    read_to_object_as_bytes, DesiredAddress, MappingName, MappingOptions, ProtectionFlags,
};
use crate::task::CurrentTask;
use crate::vfs::socket::syscalls::{sys_recvfrom, sys_recvmsg, sys_sendmsg, sys_sendto};
use crate::vfs::syscalls::{
    sys_pread64, sys_preadv2, sys_pwrite64, sys_pwritev2, sys_read, sys_write,
};
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FdNumber,
    FileHandle, FileObject, FileOps, FileWriteGuardRef, NamespaceNode,
};
use starnix_logging::set_zx_name;
use starnix_sync::{FileOpsCore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::user_buffer::UserBuffers;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::Access;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::user_value::UserValue;
use starnix_uapi::{
    errno, error, io_cqring_offsets, io_sqring_offsets, io_uring_cqe, io_uring_op,
    io_uring_op_IORING_OP_ACCEPT, io_uring_op_IORING_OP_ASYNC_CANCEL, io_uring_op_IORING_OP_CLOSE,
    io_uring_op_IORING_OP_CONNECT, io_uring_op_IORING_OP_EPOLL_CTL, io_uring_op_IORING_OP_FADVISE,
    io_uring_op_IORING_OP_FALLOCATE, io_uring_op_IORING_OP_FILES_UPDATE,
    io_uring_op_IORING_OP_FSYNC, io_uring_op_IORING_OP_LINK_TIMEOUT, io_uring_op_IORING_OP_MADVISE,
    io_uring_op_IORING_OP_NOP, io_uring_op_IORING_OP_OPENAT, io_uring_op_IORING_OP_OPENAT2,
    io_uring_op_IORING_OP_POLL_ADD, io_uring_op_IORING_OP_POLL_REMOVE, io_uring_op_IORING_OP_READ,
    io_uring_op_IORING_OP_READV, io_uring_op_IORING_OP_READ_FIXED, io_uring_op_IORING_OP_RECV,
    io_uring_op_IORING_OP_RECVMSG, io_uring_op_IORING_OP_SEND, io_uring_op_IORING_OP_SENDMSG,
    io_uring_op_IORING_OP_STATX, io_uring_op_IORING_OP_SYNC_FILE_RANGE,
    io_uring_op_IORING_OP_TIMEOUT, io_uring_op_IORING_OP_TIMEOUT_REMOVE,
    io_uring_op_IORING_OP_WRITE, io_uring_op_IORING_OP_WRITEV, io_uring_op_IORING_OP_WRITE_FIXED,
    io_uring_params, io_uring_sqe, off_t, socklen_t, IORING_FEAT_SINGLE_MMAP, IORING_OFF_CQ_RING,
    IORING_OFF_SQES, IORING_OFF_SQ_RING, IORING_SETUP_CQSIZE,
};
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

// See https://github.com/google/gvisor/blob/master/pkg/abi/linux/iouring.go#L47
pub const IORING_MAX_ENTRIES: u32 = 1 << 15; // 32768
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
    /// The index of the first element in the submission queue.
    ///
    /// These values use the full range of u32, wrapping around on overflow. To find the entry in
    /// the ring buffer, you need to take this index modulo `sq_ring_entries` or, equivalently,
    /// mask this value with `sq_ring_mask`.
    sq_head: u32,

    /// The index of the first element beyond the end of the submission queue.
    ///
    /// The number of items in the queue is defined to be `sq_tail` - `sq_head`, which means the
    /// queue is empty if the head and tail are equal.
    sq_tail: u32,

    /// The index of the first element in the completion queue.
    ///
    /// These values use the full range of u32, wrapping around on overflow. To find the entry in
    /// the ring buffer, you need to take this index modulo `cq_ring_entries` or, equivalently,
    /// mask this value with `cq_ring_mask`.
    cq_head: u32,

    /// The index of the first element beyond the end of the completion queue.
    ///
    /// The number of items in the queue is defined to be `cq_tail` - `cq_head`, which means the
    /// queue is empty if the head and tail are equal.
    cq_tail: u32,

    /// The mask to apply to map `sq_head` and `sq_tail` into the ring buffer.
    sq_ring_mask: u32,

    /// The mask to apply to map `cq_head` and `cq_tail` into the ring buffer.
    cq_ring_mask: u32,

    /// The number of entries in the submission queue.
    sq_ring_entries: u32,

    /// The number of entries in the completion queue.
    cq_ring_entries: u32,

    /// The number of submission queue entries that were dropped for being malformed.
    sq_dropped: u32,

    sq_flags: u32,
    cq_flags: u32,

    /// The number of completion queue entries that were not placed in the completion queue because
    /// there were no slots available in the ring buffer.
    cq_overflow: u32,

    _padding: [u8; 16],
}

const RING_ALIGNMENT: usize = 64;

// From params.cq_off.cqes reported by sys_io_uring_setup.
static_assertions::const_assert_eq!(std::mem::size_of::<ControlHeader>(), RING_ALIGNMENT);

/// An entry in the submission queue.
///
/// We cannot use the bindgen type generated for `io_uring_sqe` directly because that type contains
/// unions. Instead, we redefine the type here and assert that the layout matches the one that
/// defined by bindgen.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, IntoBytes, FromBytes, KnownLayout, Immutable)]
struct SqEntry {
    opcode: u8,
    flags: u8,
    ioprio: u16,
    raw_fd: i32,
    field0: u64,
    field1: u64,
    len: u32,
    op_flags: u32,
    user_data: u64,
    buf_index_or_group: u16,
    personality: u16,
    field2: u32,
    field3: [u64; 2usize],
}

static_assertions::assert_eq_size!(SqEntry, io_uring_sqe);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, opcode),
    std::mem::offset_of!(io_uring_sqe, opcode)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, flags),
    std::mem::offset_of!(io_uring_sqe, flags)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, ioprio),
    std::mem::offset_of!(io_uring_sqe, ioprio)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, raw_fd),
    std::mem::offset_of!(io_uring_sqe, fd)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, field0),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_1)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, field1),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_2)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, len),
    std::mem::offset_of!(io_uring_sqe, len)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, op_flags),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_3)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, user_data),
    std::mem::offset_of!(io_uring_sqe, user_data)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, buf_index_or_group),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_4)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, personality),
    std::mem::offset_of!(io_uring_sqe, personality)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, field2),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_5)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(SqEntry, field3),
    std::mem::offset_of!(io_uring_sqe, __bindgen_anon_6)
);

impl SqEntry {
    fn complete(&self, result: Result<SyscallResult, Errno>) -> CqEntry {
        let res = match result {
            Ok(return_value) => return_value.value() as i32,
            Err(errno) => errno.return_value() as i32,
        };
        CqEntry { user_data: self.user_data, res, flags: 0 }
    }

    fn fd(&self) -> FdNumber {
        FdNumber::from_raw(self.raw_fd)
    }

    fn iovec_addr(&self) -> UserAddress {
        self.field1.into()
    }

    fn iovec_count(&self) -> UserValue<i32> {
        (self.len as i32).into()
    }

    fn address(&self) -> UserAddress {
        self.field1.into()
    }

    fn length(&self) -> usize {
        self.len as usize
    }

    fn offset(&self) -> off_t {
        self.field0 as off_t
    }

    fn buf_index(&self) -> usize {
        self.buf_index_or_group as usize
    }
}

/// An entry in the completion queue.
///
/// We cannot use the bindgen type generated for `io_uring_cqe` directly because that type contains
/// a variable length array. Instead, we redefine the type here and assert that the layout matches
/// the one that defined by bindgen.
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, IntoBytes, FromBytes, KnownLayout, Immutable)]
struct CqEntry {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

static_assertions::assert_eq_size!(CqEntry, io_uring_cqe);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(CqEntry, user_data),
    std::mem::offset_of!(io_uring_cqe, user_data)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(CqEntry, res),
    std::mem::offset_of!(io_uring_cqe, res)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(CqEntry, flags),
    std::mem::offset_of!(io_uring_cqe, flags)
);

const CQES_OFFSET: usize = std::mem::size_of::<ControlHeader>();

#[inline]
fn align_ring_field(offset: usize) -> usize {
    offset.next_multiple_of(RING_ALIGNMENT)
}
struct IoUringMetadata {
    /// The number of entries in the submission queue.
    sq_entries: u32,

    /// The number of entries in the completion queue.
    cq_entries: u32,
}

impl IoUringMetadata {
    /// The offset of the compleition queue entry with the given index.
    ///
    /// The offset is from the start of the `ring_buffer` VMO.
    fn cq_entry_offset(&self, index: u32) -> u64 {
        let index = index % self.cq_entries;
        (CQES_OFFSET + index as usize * std::mem::size_of::<io_uring_cqe>()) as u64
    }

    /// The offset of first completion queue entry in the `ring_buffer` VMO.
    fn cqes_offset(&self) -> usize {
        CQES_OFFSET
    }

    /// The offset of submission queue indirection array in the `ring_buffer` VMO.
    fn array_offset(&self) -> usize {
        CQES_OFFSET
            + align_ring_field(self.cq_entries as usize * std::mem::size_of::<io_uring_cqe>())
    }

    /// The offset of submission queue indirection array entry with the given index in the
    /// `ring_buffer` VMO.
    fn array_entry_offset(&self, index: u32) -> u64 {
        let index = index % self.sq_entries;
        (self.array_offset() + index as usize * std::mem::size_of::<RingIndex>()) as u64
    }

    /// The number of bytes in the `ring_buffer` VMO.
    fn ring_buffer_size(&self) -> usize {
        self.array_offset() + self.sq_entries as usize * std::mem::size_of::<RingIndex>()
    }

    /// The offset of the submission queue entry with the given index in the `sq_entries` VMO.
    ///
    /// This index is the actual index of the submission queue entry, after indirecting through the
    /// indirecton array.
    fn sq_entry_offset(&self, index: u32) -> u64 {
        let index = index % self.sq_entries;
        (index as usize * std::mem::size_of::<io_uring_sqe>()) as u64
    }

    /// The number of bytes in the `sq_entries` VMO.
    fn sq_entries_size(&self) -> usize {
        self.sq_entries as usize * std::mem::size_of::<io_uring_sqe>()
    }
}

#[repr(u32)]
enum Op {
    Accept = io_uring_op_IORING_OP_ACCEPT,
    AsyncCancel = io_uring_op_IORING_OP_ASYNC_CANCEL,
    Close = io_uring_op_IORING_OP_CLOSE,
    Connect = io_uring_op_IORING_OP_CONNECT,
    EpollCtl = io_uring_op_IORING_OP_EPOLL_CTL,
    FAdvise = io_uring_op_IORING_OP_FADVISE,
    FAllocate = io_uring_op_IORING_OP_FALLOCATE,
    FilesUpdate = io_uring_op_IORING_OP_FILES_UPDATE,
    FSync = io_uring_op_IORING_OP_FSYNC,
    LinkTimeout = io_uring_op_IORING_OP_LINK_TIMEOUT,
    MAdvise = io_uring_op_IORING_OP_MADVISE,
    NOP = io_uring_op_IORING_OP_NOP,
    OpenAt = io_uring_op_IORING_OP_OPENAT,
    OpenAt2 = io_uring_op_IORING_OP_OPENAT2,
    PollAdd = io_uring_op_IORING_OP_POLL_ADD,
    PollRemove = io_uring_op_IORING_OP_POLL_REMOVE,
    Read = io_uring_op_IORING_OP_READ,
    ReadV = io_uring_op_IORING_OP_READV,
    ReadFixed = io_uring_op_IORING_OP_READ_FIXED,
    Recv = io_uring_op_IORING_OP_RECV,
    RecvMsg = io_uring_op_IORING_OP_RECVMSG,
    Send = io_uring_op_IORING_OP_SEND,
    SendMsg = io_uring_op_IORING_OP_SENDMSG,
    StatX = io_uring_op_IORING_OP_STATX,
    SyncFileRange = io_uring_op_IORING_OP_SYNC_FILE_RANGE,
    Timeout = io_uring_op_IORING_OP_TIMEOUT,
    TimeoutRemove = io_uring_op_IORING_OP_TIMEOUT_REMOVE,
    Write = io_uring_op_IORING_OP_WRITE,
    WriteV = io_uring_op_IORING_OP_WRITEV,
    WriteFixed = io_uring_op_IORING_OP_WRITE_FIXED,
}

impl Op {
    fn from_code(opcode: io_uring_op) -> Result<Op, Errno> {
        match opcode {
            io_uring_op_IORING_OP_ACCEPT => Ok(Self::Accept),
            io_uring_op_IORING_OP_ASYNC_CANCEL => Ok(Self::AsyncCancel),
            io_uring_op_IORING_OP_CLOSE => Ok(Self::Close),
            io_uring_op_IORING_OP_CONNECT => Ok(Self::Connect),
            io_uring_op_IORING_OP_EPOLL_CTL => Ok(Self::EpollCtl),
            io_uring_op_IORING_OP_FADVISE => Ok(Self::FAdvise),
            io_uring_op_IORING_OP_FALLOCATE => Ok(Self::FAllocate),
            io_uring_op_IORING_OP_FILES_UPDATE => Ok(Self::FilesUpdate),
            io_uring_op_IORING_OP_FSYNC => Ok(Self::FSync),
            io_uring_op_IORING_OP_LINK_TIMEOUT => Ok(Self::LinkTimeout),
            io_uring_op_IORING_OP_MADVISE => Ok(Self::MAdvise),
            io_uring_op_IORING_OP_NOP => Ok(Self::NOP),
            io_uring_op_IORING_OP_OPENAT => Ok(Self::OpenAt),
            io_uring_op_IORING_OP_OPENAT2 => Ok(Self::OpenAt2),
            io_uring_op_IORING_OP_POLL_ADD => Ok(Self::PollAdd),
            io_uring_op_IORING_OP_POLL_REMOVE => Ok(Self::PollRemove),
            io_uring_op_IORING_OP_READ => Ok(Self::Read),
            io_uring_op_IORING_OP_READV => Ok(Self::ReadV),
            io_uring_op_IORING_OP_READ_FIXED => Ok(Self::ReadFixed),
            io_uring_op_IORING_OP_RECV => Ok(Self::Recv),
            io_uring_op_IORING_OP_RECVMSG => Ok(Self::RecvMsg),
            io_uring_op_IORING_OP_SEND => Ok(Self::Send),
            io_uring_op_IORING_OP_SENDMSG => Ok(Self::SendMsg),
            io_uring_op_IORING_OP_STATX => Ok(Self::StatX),
            io_uring_op_IORING_OP_SYNC_FILE_RANGE => Ok(Self::SyncFileRange),
            io_uring_op_IORING_OP_TIMEOUT => Ok(Self::Timeout),
            io_uring_op_IORING_OP_TIMEOUT_REMOVE => Ok(Self::TimeoutRemove),
            io_uring_op_IORING_OP_WRITE => Ok(Self::Write),
            io_uring_op_IORING_OP_WRITEV => Ok(Self::WriteV),
            io_uring_op_IORING_OP_WRITE_FIXED => Ok(Self::WriteFixed),
            _ => error!(EINVAL),
        }
    }
}

// Currently, we read and write the memory shared with userspace via the VMOs. In the future, we
// will likely want to map the memory for these VMOs into the kernel address space so that we can
// access their contents more efficiently and so that we can perform the appropriate atomic
// operations.

// TODO(https://fxbug.dev/297431387): Map `ring_buffer` and `sq_entries` into kernel memory so that
// this operation becomes memcpy.
fn read_object<T: FromBytes>(memory_object: &MemoryObject, offset: u64) -> Result<T, Errno> {
    // SAFETY: read_uninit returns an error if not all the bytes were read.
    unsafe {
        read_to_object_as_bytes(|buf| {
            memory_object.read_uninit(buf, offset).map_err(|_| errno!(EFAULT))?;
            Ok(())
        })
    }
}

// TODO(https://fxbug.dev/297431387): Map `ring_buffer` and `sq_entries` into kernel memory so that
// this operation becomes memcpy.
fn write_object<T: IntoBytes + Immutable>(
    memory_object: &MemoryObject,
    offset: u64,
    value: &T,
) -> Result<(), Errno> {
    memory_object.write(value.as_bytes(), offset).map_err(|_| errno!(EFAULT))
}

/// The memory the IoUring shares with userspace.
struct IoUringQueue {
    /// Metadata about the layout of this memory.
    metadata: IoUringMetadata,

    /// The primary ring buffer.
    ///
    /// The ring buffer's memory layout is as follows:
    ///
    ///   ControlHeader
    ///   N completion queue entries
    ///   An array of u32 values used to indirect indices to the submission queue entries
    ///
    /// The ControlHeader is a fixed size, which means the completion queue entries always start
    /// at the same offset in this VMO.
    ring_buffer: Arc<MemoryObject>,

    /// A separate VMO for the submission queue entries.
    ///
    /// This entries are not necessarily populated in order. Instead, userspace uses the array of
    /// submission queue indices in the `ring_buffer` in order. That array gives the indices of
    /// the actual submission queue entries.
    ///
    /// IoUring uses this index indirection scheme because submission queue entries do not always
    /// complete in the same order they were submitted.
    sq_entries: Arc<MemoryObject>,
}

impl IoUringQueue {
    fn new(metadata: IoUringMetadata) -> Result<Self, Errno> {
        let ring_buffer =
            zx::Vmo::create(metadata.ring_buffer_size() as u64).map_err(|_| errno!(ENOMEM))?;
        set_zx_name(&ring_buffer, b"io_uring:ring");
        let sq_entries =
            zx::Vmo::create(metadata.sq_entries_size() as u64).map_err(|_| errno!(ENOMEM))?;
        set_zx_name(&sq_entries, b"io_uring:sqes");

        Ok(Self {
            metadata,
            ring_buffer: Arc::new(ring_buffer.into()),
            sq_entries: Arc::new(sq_entries.into()),
        })
    }

    fn write_header(&self, header: ControlHeader) -> Result<(), Errno> {
        write_object(&self.ring_buffer, 0, &header).map_err(|_| errno!(ENOMEM))
    }

    fn read_sq_head(&self) -> Result<u32, Errno> {
        read_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, sq_head) as u64)
    }

    fn write_sq_head(&self, value: u32) -> Result<(), Errno> {
        write_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, sq_head) as u64, &value)
    }

    fn read_sq_tail(&self) -> Result<u32, Errno> {
        // TODO(https://fxbug.dev/297431387): Reading the tail field should be atomic with ordering
        // acquire once we map these buffers into kernel memory.
        read_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, sq_tail) as u64)
    }

    fn read_cq_head(&self) -> Result<u32, Errno> {
        // TODO(https://fxbug.dev/297431387): Reading the head field should be atomic with ordering
        // acquire once we map these buffers into kernel memory.
        read_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, cq_head) as u64)
    }

    fn read_cq_tail(&self) -> Result<u32, Errno> {
        read_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, cq_tail) as u64)
    }

    fn write_cq_tail(&self, value: u32) -> Result<(), Errno> {
        // TODO(https://fxbug.dev/297431387): Writing the tail field should be atomic with ordering
        // release once we map these buffers into kernel memory.
        write_object(&self.ring_buffer, std::mem::offset_of!(ControlHeader, cq_tail) as u64, &value)
    }

    fn read_array_entry(&self, index: u32) -> Result<u32, Errno> {
        read_object(&self.ring_buffer, self.metadata.array_entry_offset(index))
    }

    fn read_sq_entry(&self, index: u32) -> Result<SqEntry, Errno> {
        let sqe_index = self.read_array_entry(index)?;
        read_object(&self.sq_entries, self.metadata.sq_entry_offset(sqe_index))
    }

    fn write_cq_entry(&self, index: u32, entry: &CqEntry) -> Result<(), Errno> {
        write_object(&self.ring_buffer, self.metadata.cq_entry_offset(index), entry)
    }

    fn increment_overflow(&self) -> Result<(), Errno> {
        // TODO(https://fxbug.dev/297431387): Incrementing the overflow count should be an atomic
        // operation.
        let offset = std::mem::offset_of!(ControlHeader, cq_overflow) as u64;
        let mut overflow: u32 = read_object(&self.ring_buffer, offset)?;
        overflow = overflow.saturating_add(1);
        write_object(&self.ring_buffer, offset, &overflow)
    }

    /// Pop an entry off the submission queue and update the head to let userspace queue more
    /// entries.
    ///
    /// Returns `None` if the submission queue is empty.
    fn pop_sq_entry(&self) -> Result<Option<SqEntry>, Errno> {
        let tail = self.read_sq_tail()?;
        let head = self.read_sq_head()?;
        if head != tail {
            let sq_entry = self.read_sq_entry(head)?;
            self.write_sq_head(head.wrapping_add(1))?;
            Ok(Some(sq_entry))
        } else {
            Ok(None)
        }
    }

    /// Push an entry onto the completion queue and update the tail to let userspace know a new
    /// entry is available.
    ///
    /// If there is no room in the completion queue, this function will increment the overflow
    /// counter.
    fn push_cq_entry(&self, entry: &CqEntry) -> Result<(), Errno> {
        let head = self.read_cq_head()?;
        let tail = self.read_cq_tail()?;
        // Check that the offset for the tail location doesn't collide with the head of the queue.
        // This can happen because the entries are stored in a ring buffer.
        if head != tail
            && self.metadata.cq_entry_offset(tail) == self.metadata.cq_entry_offset(head)
        {
            self.increment_overflow()?;
        } else {
            self.write_cq_entry(tail, entry)?;
            self.write_cq_tail(tail.wrapping_add(1))?;
        }
        Ok(())
    }
}

pub struct IoUringFileObject {
    queue: IoUringQueue,
    registered_buffers: Mutex<UserBuffers>,
}

impl IoUringFileObject {
    pub fn new_file(
        current_task: &CurrentTask,
        entries: u32,
        params: &mut io_uring_params,
    ) -> Result<FileHandle, Errno> {
        let sq_entries = entries.next_power_of_two();
        let cq_entries = if params.flags & IORING_SETUP_CQSIZE != 0 {
            UserValue::from_raw(params.cq_entries)
                .validate(sq_entries..IORING_MAX_CQ_ENTRIES)
                .ok_or_else(|| errno!(EINVAL))?
                .next_power_of_two()
        } else {
            // This operation cannot overflow because sq_entries is capped at IORING_MAX_ENTRIES,
            // which is only 15 bits.
            sq_entries * 2
        };

        let queue =
            IoUringQueue::new(IoUringMetadata { sq_entries: sq_entries, cq_entries: cq_entries })?;

        queue.write_header(ControlHeader {
            sq_ring_mask: sq_entries - 1,
            cq_ring_mask: cq_entries - 1,
            sq_ring_entries: sq_entries,
            cq_ring_entries: cq_entries,
            ..Default::default()
        })?;

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
            array: queue.metadata.array_offset() as u32,
            ..Default::default()
        };
        params.cq_off = io_cqring_offsets {
            head: std::mem::offset_of!(ControlHeader, cq_head) as u32,
            tail: std::mem::offset_of!(ControlHeader, cq_tail) as u32,
            ring_mask: std::mem::offset_of!(ControlHeader, cq_ring_mask) as u32,
            ring_entries: std::mem::offset_of!(ControlHeader, cq_ring_entries) as u32,
            overflow: std::mem::offset_of!(ControlHeader, cq_overflow) as u32,
            cqes: queue.metadata.cqes_offset() as u32,
            flags: std::mem::offset_of!(ControlHeader, cq_flags) as u32,
            ..Default::default()
        };

        let object = Box::new(IoUringFileObject { queue, registered_buffers: Default::default() });
        Ok(Anon::new_file(current_task, object, OpenFlags::RDWR))
    }

    pub fn register_buffers(&self, buffers: UserBuffers) {
        // The docs for io_uring_register imply that the kernel should actually map this memory
        // into its own address space when these buffers are registered. That's probably observable
        // if the client changes the mappings for these addresses between the time they are
        // registered and they are used. For now, we just store the addresses.
        *self.registered_buffers.lock() = buffers;
    }

    pub fn unregister_buffers(&self) {
        self.registered_buffers.lock().clear();
    }

    pub fn enter(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        to_submit: u32,
        _min_complete: u32,
        _flags: u32,
    ) -> Result<u32, Errno> {
        let mut submitted = 0;
        while let Some(sq_entry) = self.queue.pop_sq_entry()? {
            submitted += 1;
            // We currently act as if every SqEntry has IOSQE_IO_DRAIN.
            let result = self.execute(locked, current_task, &sq_entry);
            let cq_entry = sq_entry.complete(result);
            self.queue.push_cq_entry(&cq_entry)?;
            if submitted >= to_submit {
                break;
            }
        }
        Ok(submitted)
    }

    fn has_registered_buffers(&self) -> bool {
        !self.registered_buffers.lock().is_empty()
    }

    fn check_buffer(&self, entry: &SqEntry) -> Result<(), Errno> {
        let index = entry.buf_index();
        let buffers = self.registered_buffers.lock();
        if buffers.is_empty() {
            return error!(EFAULT);
        }
        let buffer = buffers.get(index).ok_or_else(|| errno!(EINVAL))?;
        if !buffer.contains(entry.address(), entry.length()) {
            error!(EFAULT)
        } else {
            Ok(())
        }
    }

    fn execute(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        entry: &SqEntry,
    ) -> Result<SyscallResult, Errno> {
        match Op::from_code(entry.opcode as io_uring_op)? {
            Op::NOP => Ok(SUCCESS),
            Op::ReadV => {
                if entry.ioprio != 0 || entry.buf_index() != 0 {
                    return error!(EINVAL);
                }
                sys_preadv2(
                    locked,
                    current_task,
                    entry.fd(),
                    entry.iovec_addr(),
                    entry.iovec_count(),
                    entry.offset(),
                    SyscallArg::default(),
                    entry.op_flags,
                )
                .map(Into::into)
            }
            Op::WriteV => {
                if entry.ioprio != 0 || entry.buf_index() != 0 {
                    return error!(EINVAL);
                }
                sys_pwritev2(
                    locked,
                    current_task,
                    entry.fd(),
                    entry.iovec_addr(),
                    entry.iovec_count(),
                    entry.offset(),
                    SyscallArg::default(),
                    entry.op_flags,
                )
                .map(Into::into)
            }
            Op::ReadFixed => {
                if entry.ioprio != 0 {
                    return error!(EINVAL);
                }
                // TODO(https://fxbug.dev/297431387): We're supposed to make a kernel mapping
                // when the buffers are registered and we should be performing this operation using
                // those kernel mappings rather than using the userspace mappings.
                self.check_buffer(entry)?;
                do_read(locked, current_task, entry)
            }
            Op::WriteFixed => {
                if entry.ioprio != 0 {
                    return error!(EINVAL);
                }
                // TODO(https://fxbug.dev/297431387): We're supposed to make a kernel mapping
                // when the buffers are registered and we should be performing this operation using
                // those kernel mappings rather than using the userspace mappings.
                self.check_buffer(entry)?;
                do_write(locked, current_task, entry)
            }
            Op::Read => {
                if self.has_registered_buffers() {
                    return error!(EINVAL);
                }
                do_read(locked, current_task, entry)
            }
            Op::Write => {
                if self.has_registered_buffers() {
                    return error!(EINVAL);
                }
                do_write(locked, current_task, entry)
            }
            Op::SendMsg => sys_sendmsg(
                locked,
                current_task,
                entry.fd(),
                entry.address().into(),
                entry.op_flags,
            )
            .map(Into::into),
            Op::RecvMsg => sys_recvmsg(
                locked,
                current_task,
                entry.fd(),
                entry.address().into(),
                entry.op_flags,
            )
            .map(Into::into),
            Op::Send => sys_sendto(
                locked,
                current_task,
                entry.fd(),
                entry.address(),
                entry.length(),
                entry.op_flags,
                UserAddress::default(),
                socklen_t::default(),
            )
            .map(Into::into),
            Op::Recv => sys_recvfrom(
                locked,
                current_task,
                entry.fd(),
                entry.address(),
                entry.length(),
                entry.op_flags,
                UserAddress::default(),
                UserRef::default(),
            )
            .map(Into::into),
            Op::FSync
            | Op::PollAdd
            | Op::PollRemove
            | Op::SyncFileRange
            | Op::Timeout
            | Op::TimeoutRemove
            | Op::Accept
            | Op::AsyncCancel
            | Op::LinkTimeout
            | Op::Connect
            | Op::FAllocate
            | Op::OpenAt
            | Op::Close
            | Op::FilesUpdate
            | Op::StatX
            | Op::FAdvise
            | Op::MAdvise
            | Op::OpenAt2
            | Op::EpollCtl => error!(EOPNOTSUPP),
        }
    }
}

fn do_read(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    entry: &SqEntry,
) -> Result<SyscallResult, Errno> {
    let offset = entry.offset();
    if offset == -1 {
        sys_read(locked, current_task, entry.fd(), entry.address(), entry.length()).map(Into::into)
    } else {
        sys_pread64(locked, current_task, entry.fd(), entry.address(), entry.length(), offset)
            .map(Into::into)
    }
}

fn do_write(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    entry: &SqEntry,
) -> Result<SyscallResult, Errno> {
    let offset = entry.offset();
    if offset == -1 {
        sys_write(locked, current_task, entry.fd(), entry.address(), entry.length()).map(Into::into)
    } else {
        sys_pwrite64(
            locked,
            current_task,
            entry.fd(),
            entry.address(),
            entry.length(),
            entry.offset(),
        )
        .map(Into::into)
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
            IORING_OFF_SQ_RING | IORING_OFF_CQ_RING => self.queue.ring_buffer.clone(),
            IORING_OFF_SQES => self.queue.sq_entries.clone(),
            _ => return error!(EINVAL),
        };
        current_task.mm().ok_or_else(|| errno!(EINVAL))?.map_memory(
            addr,
            memory,
            0,
            length,
            prot_flags,
            Access::rwx(),
            options,
            MappingName::File(filename.into_active()),
            FileWriteGuardRef(None),
        )
    }
}
