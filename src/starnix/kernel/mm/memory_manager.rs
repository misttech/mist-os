// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::memory::MemoryObject;
use crate::mm::{
    FutexTable, InflightVmsplicedPayloads, PrivateFutexKey, VmsplicePayload,
    VmsplicePayloadSegment, VMEX_RESOURCE,
};
use crate::signals::{SignalDetail, SignalInfo};
use crate::task::{CurrentTask, ExceptionResult, PageFaultExceptionReport, Task};
use crate::vfs::aio::AioContext;
use crate::vfs::{
    ActiveNamespaceNode, DynamicFile, DynamicFileBuf, FileWriteGuardRef, FsNodeOps, FsStr,
    FsString, NamespaceNode, SequenceFileSource,
};
use anyhow::{anyhow, Error};
use bitflags::bitflags;
use fuchsia_inspect_contrib::{profile_duration, ProfileDuration};
use starnix_types::arch::ArchWidth;

use once_cell::sync::Lazy;
use rand::{thread_rng, Rng};
use range_map::RangeMap;
use smallvec::SmallVec;
use starnix_logging::{
    impossible_error, log_warn, trace_duration, track_stub, CATEGORY_STARNIX_MM,
};
use starnix_sync::{LockBefore, Locked, MmDumpable, OrderedMutex, RwLock};
use starnix_types::futex_address::FutexAddress;
use starnix_types::math::round_up_to_system_page_size;
use starnix_types::ownership::WeakRef;
use starnix_types::user_buffer::{UserBuffer, UserBuffers, UserBuffers32};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::Access;
use starnix_uapi::range_ext::RangeExt;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::restricted_aspace::{
    RESTRICTED_ASPACE_BASE, RESTRICTED_ASPACE_HIGHEST_ADDRESS, RESTRICTED_ASPACE_RANGE,
    RESTRICTED_ASPACE_SIZE,
};
use starnix_uapi::signals::{SIGBUS, SIGSEGV};
use starnix_uapi::user_address::{UserAddress, UserCString, UserRef};
use starnix_uapi::user_value::UserValue;
use starnix_uapi::{
    errno, error, MADV_DOFORK, MADV_DONTFORK, MADV_DONTNEED, MADV_KEEPONFORK, MADV_NOHUGEPAGE,
    MADV_NORMAL, MADV_WILLNEED, MADV_WIPEONFORK, MREMAP_DONTUNMAP, MREMAP_FIXED, MREMAP_MAYMOVE,
    PROT_EXEC, PROT_GROWSDOWN, PROT_READ, PROT_WRITE, SI_KERNEL, UIO_MAXIOV,
};
use static_assertions::const_assert_eq;
use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;
use syncio::zxio::zxio_default_maybe_faultable_copy;
use usercopy::slice_to_maybe_uninit_mut;
use zerocopy::{FromBytes, Immutable, IntoBytes};
use zx::VmarInfo;

pub const ZX_VM_SPECIFIC_OVERWRITE: zx::VmarFlags =
    zx::VmarFlags::from_bits_retain(zx::VmarFlagsExtended::SPECIFIC_OVERWRITE.bits());

// We do not create shared processes in unit tests.
pub(crate) const UNIFIED_ASPACES_ENABLED: bool =
    cfg!(not(test)) && cfg!(feature = "unified_aspace");

/// Initializes the usercopy utilities.
///
/// It is useful to explicitly call this so that the usercopy is initialized
/// at a known instant. For example, Starnix may want to make sure the usercopy
/// thread created to support user copying is associated to the Starnix process
/// and not a restricted-mode process.
pub fn init_usercopy() {
    // This call lazily initializes the `Usercopy` instance.
    let _ = usercopy();
}

const GUARD_PAGE_COUNT_FOR_GROWSDOWN_MAPPINGS: usize = 256;

#[cfg(target_arch = "x86_64")]
const ASLR_RANDOM_BITS: usize = 27;

#[cfg(target_arch = "aarch64")]
const ASLR_RANDOM_BITS: usize = 28;

#[cfg(target_arch = "riscv64")]
const ASLR_RANDOM_BITS: usize = 18;

/// Number of bits of entropy for processes running in 32 bits mode.
const ASLR_32_RANDOM_BITS: usize = 8;

// The biggest we expect stack to be; increase as needed
// TODO(https://fxbug.dev/322874791): Once setting RLIMIT_STACK is implemented, we should use it.
const MAX_STACK_SIZE: usize = 512 * 1024 * 1024;

fn usercopy() -> Option<&'static usercopy::Usercopy> {
    static USERCOPY: Lazy<Option<usercopy::Usercopy>> = Lazy::new(|| {
        // We do not create shared processes in unit tests.
        if UNIFIED_ASPACES_ENABLED {
            // ASUMPTION: All Starnix managed Linux processes have the same
            // restricted mode address range.
            Some(usercopy::Usercopy::new(RESTRICTED_ASPACE_RANGE).unwrap())
        } else {
            None
        }
    });

    Lazy::force(&USERCOPY).as_ref()
}

/// Provides an implementation for zxio's `zxio_maybe_faultable_copy` that supports
/// catching faults.
///
/// See zxio's `zxio_maybe_faultable_copy` documentation for more details.
///
/// # Safety
///
/// Only one of `src`/`dest` may be an address to a buffer owned by user/restricted-mode
/// (`ret_dest` indicates whether the user-owned buffer is `dest` when `true`).
/// The other must be a valid Starnix/normal-mode buffer that will never cause a fault
/// when the first `count` bytes are read/written.
#[no_mangle]
pub unsafe fn zxio_maybe_faultable_copy_impl(
    dest: *mut u8,
    src: *const u8,
    count: usize,
    ret_dest: bool,
) -> bool {
    if let Some(usercopy) = usercopy() {
        let ret = usercopy.raw_hermetic_copy(dest, src, count, ret_dest);
        ret == count
    } else {
        zxio_default_maybe_faultable_copy(dest, src, count, ret_dest)
    }
}

pub static PAGE_SIZE: Lazy<u64> = Lazy::new(|| zx::system_get_page_size() as u64);

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MappingOptions: u32 {
      const SHARED      = 1 << 0;
      const ANONYMOUS   = 1 << 1;
      const LOWER_32BIT = 1 << 2;
      const GROWSDOWN   = 1 << 3;
      const ELF_BINARY  = 1 << 4;
      const DONTFORK    = 1 << 5;
      const WIPEONFORK  = 1 << 6;
      const DONT_SPLIT  = 1 << 7;
      const DONT_EXPAND = 1 << 8;
      const POPULATE    = 1 << 9;
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ProtectionFlags: u32 {
      const READ = PROT_READ;
      const WRITE = PROT_WRITE;
      const EXEC = PROT_EXEC;
      const GROWSDOWN = PROT_GROWSDOWN;
    }
}

impl ProtectionFlags {
    pub const ACCESS_FLAGS: Self =
        Self::from_bits_truncate(Self::READ.bits() | Self::WRITE.bits() | Self::EXEC.bits());

    pub fn to_vmar_flags(self) -> zx::VmarFlags {
        let mut vmar_flags = zx::VmarFlags::empty();
        if self.contains(ProtectionFlags::READ) {
            vmar_flags |= zx::VmarFlags::PERM_READ;
        }
        if self.contains(ProtectionFlags::WRITE) {
            vmar_flags |= zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE;
        }
        if self.contains(ProtectionFlags::EXEC) {
            vmar_flags |= zx::VmarFlags::PERM_EXECUTE | zx::VmarFlags::PERM_READ_IF_XOM_UNSUPPORTED;
        }
        vmar_flags
    }

    pub fn from_vmar_flags(vmar_flags: zx::VmarFlags) -> ProtectionFlags {
        let mut prot_flags = ProtectionFlags::empty();
        if vmar_flags.contains(zx::VmarFlags::PERM_READ) {
            prot_flags |= ProtectionFlags::READ;
        }
        if vmar_flags.contains(zx::VmarFlags::PERM_WRITE) {
            prot_flags |= ProtectionFlags::WRITE;
        }
        if vmar_flags.contains(zx::VmarFlags::PERM_EXECUTE) {
            prot_flags |= ProtectionFlags::EXEC;
        }
        prot_flags
    }

    pub fn from_access_bits(prot: u32) -> Option<Self> {
        if let Some(flags) = ProtectionFlags::from_bits(prot) {
            if flags.contains(Self::ACCESS_FLAGS.complement()) {
                None
            } else {
                Some(flags)
            }
        } else {
            None
        }
    }

    pub fn access_flags(&self) -> Self {
        *self & Self::ACCESS_FLAGS
    }

    pub fn to_access(&self) -> Access {
        let mut access = Access::empty();
        if self.contains(ProtectionFlags::READ) {
            access |= Access::READ;
        }
        if self.contains(ProtectionFlags::WRITE) {
            access |= Access::WRITE;
        }
        if self.contains(ProtectionFlags::EXEC) {
            access |= Access::EXEC;
        }
        access
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[rustfmt::skip]  // Preserve column alignment.
    struct MappingFlags: u32 {
        const READ        = 1 <<  0;  // PROT_READ
        const WRITE       = 1 <<  1;  // PROT_WRITE
        const EXEC        = 1 <<  2;  // PROT_EXEC
        const SHARED      = 1 <<  3;
        const ANONYMOUS   = 1 <<  4;
        const LOWER_32BIT = 1 <<  5;
        const GROWSDOWN   = 1 <<  6;
        const ELF_BINARY  = 1 <<  7;
        const DONTFORK    = 1 <<  8;
        const WIPEONFORK  = 1 <<  9;
        const DONT_SPLIT  = 1 << 10;
        const DONT_EXPAND = 1 << 11;
    }
}

// The low three bits of MappingFlags match ProtectionFlags.
const_assert_eq!(MappingFlags::READ.bits(), PROT_READ);
const_assert_eq!(MappingFlags::WRITE.bits(), PROT_WRITE);
const_assert_eq!(MappingFlags::EXEC.bits(), PROT_EXEC);

// The next bits of MappingFlags match MappingOptions, shifted up.
const_assert_eq!(MappingFlags::SHARED.bits(), MappingOptions::SHARED.bits() << 3);
const_assert_eq!(MappingFlags::ANONYMOUS.bits(), MappingOptions::ANONYMOUS.bits() << 3);
const_assert_eq!(MappingFlags::LOWER_32BIT.bits(), MappingOptions::LOWER_32BIT.bits() << 3);
const_assert_eq!(MappingFlags::GROWSDOWN.bits(), MappingOptions::GROWSDOWN.bits() << 3);
const_assert_eq!(MappingFlags::ELF_BINARY.bits(), MappingOptions::ELF_BINARY.bits() << 3);
const_assert_eq!(MappingFlags::DONTFORK.bits(), MappingOptions::DONTFORK.bits() << 3);
const_assert_eq!(MappingFlags::WIPEONFORK.bits(), MappingOptions::WIPEONFORK.bits() << 3);
const_assert_eq!(MappingFlags::DONT_SPLIT.bits(), MappingOptions::DONT_SPLIT.bits() << 3);
const_assert_eq!(MappingFlags::DONT_EXPAND.bits(), MappingOptions::DONT_EXPAND.bits() << 3);

impl MappingFlags {
    fn access_flags(&self) -> ProtectionFlags {
        ProtectionFlags::from_bits_truncate(self.bits() & ProtectionFlags::ACCESS_FLAGS.bits())
    }

    fn with_access_flags(&self, prot_flags: ProtectionFlags) -> Self {
        let mapping_flags =
            *self & (MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXEC).complement();
        mapping_flags | Self::from_bits_truncate(prot_flags.access_flags().bits())
    }

    #[cfg(any(feature = "alternate_anon_allocs", test))]
    fn options(&self) -> MappingOptions {
        MappingOptions::from_bits_truncate(self.bits() >> 3)
    }

    fn from_access_flags_and_options(prot_flags: ProtectionFlags, options: MappingOptions) -> Self {
        Self::from_bits_truncate(prot_flags.access_flags().bits())
            | Self::from_bits_truncate(options.bits() << 3)
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MremapFlags: u32 {
        const MAYMOVE = MREMAP_MAYMOVE;
        const FIXED = MREMAP_FIXED;
        const DONTUNMAP = MREMAP_DONTUNMAP;
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum MappingName {
    /// No name.
    None,

    /// This mapping is the initial stack.
    Stack,

    /// This mapping is the heap.
    Heap,

    /// This mapping is the vdso.
    Vdso,

    /// This mapping is the vvar.
    Vvar,

    /// The file backing this mapping.
    File(ActiveNamespaceNode),

    /// The name associated with the mapping. Set by prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
    /// An empty name is distinct from an unnamed mapping. Mappings are initially created with no
    /// name and can be reset to the unnamed state by passing NULL to
    /// prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
    Vma(FsString),

    /// The name associated with the mapping of an ashmem region.  Set by ioctl(fd, ASHMEM_SET_NAME, ...).
    /// By default "dev/ashmem".
    Ashmem(FsString),

    /// This mapping is a context for asynchronous I/O.
    AioContext(Arc<AioContext>),
}

#[derive(Debug, Eq, PartialEq, Clone)]
struct MappingBackingMemory {
    /// The base address of this mapping.
    ///
    /// Keep in mind that the mapping might be trimmed in the RangeMap if the
    /// part of the mapping is unmapped, which means the base might extend
    /// before the currently valid portion of the mapping.
    base: UserAddress,

    /// The memory object that contains the memory used in this mapping.
    memory: Arc<MemoryObject>,

    /// The offset in the memory object that corresponds to the base address.
    memory_offset: u64,
}

impl MappingBackingMemory {
    /// Reads exactly `bytes.len()` bytes of memory from `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        profile_duration!("MappingReadMemory");
        self.memory.read_uninit(bytes, self.address_to_offset(addr)).map_err(|_| errno!(EFAULT))
    }

    /// Writes the provided bytes to `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write to the memory object.
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<(), Errno> {
        self.memory.write(bytes, self.address_to_offset(addr)).map_err(|_| errno!(EFAULT))
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.memory
            .op_range(zx::VmoOp::ZERO, self.address_to_offset(addr), length as u64)
            .map_err(|_| errno!(EFAULT))?;
        Ok(length)
    }

    /// Converts a `UserAddress` to an offset in this mapping's memory object.
    fn address_to_offset(&self, addr: UserAddress) -> u64 {
        (addr.ptr() - self.base.ptr()) as u64 + self.memory_offset
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
enum MappingBacking {
    Memory(MappingBackingMemory),

    #[cfg(feature = "alternate_anon_allocs")]
    PrivateAnonymous,
}

#[derive(Debug, Eq, PartialEq, Clone)]
struct Mapping {
    /// Object backing this mapping.
    backing: MappingBacking,

    /// The flags used by the mapping, including protection.
    flags: MappingFlags,

    /// The maximum amount of access allowed to this mapping.
    max_access: Access,

    /// The name for this mapping.
    ///
    /// This may be a reference to the filesystem node backing this mapping or a userspace-assigned name.
    /// The existence of this field is orthogonal to whether this mapping is anonymous - mappings of the
    /// file '/dev/zero' are treated as anonymous mappings and anonymous mappings may have a name assigned.
    ///
    /// Because of this exception, avoid using this field to check if a mapping is anonymous.
    /// Instead, check if `options` bitfield contains `MappingOptions::ANONYMOUS`.
    name: MappingName,

    /// Lock guard held to prevent this file from being written while it's being executed.
    file_write_guard: FileWriteGuardRef,
}

impl Mapping {
    fn new(
        base: UserAddress,
        memory: Arc<MemoryObject>,
        memory_offset: u64,
        flags: MappingFlags,
        max_access: Access,
        file_write_guard: FileWriteGuardRef,
    ) -> Mapping {
        Self::with_name(
            base,
            memory,
            memory_offset,
            flags,
            max_access,
            MappingName::None,
            file_write_guard,
        )
    }

    fn with_name(
        base: UserAddress,
        memory: Arc<MemoryObject>,
        memory_offset: u64,
        flags: MappingFlags,
        max_access: Access,
        name: MappingName,
        file_write_guard: FileWriteGuardRef,
    ) -> Mapping {
        Mapping {
            backing: MappingBacking::Memory(MappingBackingMemory { base, memory, memory_offset }),
            flags,
            max_access,
            name,
            file_write_guard,
        }
    }

    #[cfg(feature = "alternate_anon_allocs")]
    fn new_private_anonymous(flags: MappingFlags, name: MappingName) -> Mapping {
        Mapping {
            backing: MappingBacking::PrivateAnonymous,
            flags,
            max_access: Access::rwx(),
            name,
            file_write_guard: FileWriteGuardRef(None),
        }
    }

    fn inflate_to_include_guard_pages(&self, range: &Range<UserAddress>) -> Range<UserAddress> {
        let start = if self.flags.contains(MappingFlags::GROWSDOWN) {
            range
                .start
                .saturating_sub(*PAGE_SIZE as usize * GUARD_PAGE_COUNT_FOR_GROWSDOWN_MAPPINGS)
        } else {
            range.start
        };
        start..range.end
    }

    /// Converts a `UserAddress` to an offset in this mapping's memory object.
    fn address_to_offset(&self, addr: UserAddress) -> u64 {
        match &self.backing {
            MappingBacking::Memory(backing) => backing.address_to_offset(addr),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                // For private, anonymous allocations the virtual address is the offset in the backing memory object.
                addr.ptr() as u64
            }
        }
    }

    fn can_read(&self) -> bool {
        self.flags.contains(MappingFlags::READ)
    }

    fn can_write(&self) -> bool {
        self.flags.contains(MappingFlags::WRITE)
    }

    fn can_exec(&self) -> bool {
        self.flags.contains(MappingFlags::EXEC)
    }

    fn private_anonymous(&self) -> bool {
        #[cfg(feature = "alternate_anon_allocs")]
        if let MappingBacking::PrivateAnonymous = &self.backing {
            return true;
        }
        !self.flags.contains(MappingFlags::SHARED) && self.flags.contains(MappingFlags::ANONYMOUS)
    }

    fn vm_flags(&self) -> String {
        let mut string = String::default();
        // From <https://man7.org/linux/man-pages/man5/proc_pid_smaps.5.html>:
        //
        // rd   -   readable
        if self.flags.contains(MappingFlags::READ) {
            string.push_str("rd ");
        }
        // wr   -   writable
        if self.flags.contains(MappingFlags::WRITE) {
            string.push_str("wr ");
        }
        // ex   -   executable
        if self.flags.contains(MappingFlags::EXEC) {
            string.push_str("ex ");
        }
        // sh   -   shared
        if self.flags.contains(MappingFlags::SHARED) && self.max_access.contains(Access::WRITE) {
            string.push_str("sh ");
        }
        // mr   -   may read
        if self.max_access.contains(Access::READ) {
            string.push_str("mr ");
        }
        // mw   -   may write
        if self.max_access.contains(Access::WRITE) {
            string.push_str("mw ");
        }
        // me   -   may execute
        if self.max_access.contains(Access::EXEC) {
            string.push_str("me ");
        }
        // ms   -   may share
        if self.flags.contains(MappingFlags::SHARED) {
            string.push_str("ms ");
        }
        // gd   -   stack segment grows down
        if self.flags.contains(MappingFlags::GROWSDOWN) {
            string.push_str("gd ");
        }
        // pf   -   pure PFN range
        // dw   -   disabled write to the mapped file
        // lo   -   pages are locked in memory
        // io   -   memory mapped I/O area
        // sr   -   sequential read advise provided
        // rr   -   random read advise provided
        // dc   -   do not copy area on fork
        if self.flags.contains(MappingFlags::DONTFORK) {
            string.push_str("dc ");
        }
        // de   -   do not expand area on remapping
        if self.flags.contains(MappingFlags::DONT_EXPAND) {
            string.push_str("de ");
        }
        // ac   -   area is accountable
        string.push_str("ac ");
        // nr   -   swap space is not reserved for the area
        // ht   -   area uses huge tlb pages
        // sf   -   perform synchronous page faults (since Linux 4.15)
        // nl   -   non-linear mapping (removed in Linux 4.0)
        // ar   -   architecture specific flag
        // wf   -   wipe on fork (since Linux 4.14)
        if self.flags.contains(MappingFlags::WIPEONFORK) {
            string.push_str("wf ");
        }
        // dd   -   do not include area into core dump
        // sd   -   soft-dirty flag (since Linux 3.13)
        // mm   -   mixed map area
        // hg   -   huge page advise flag
        // nh   -   no-huge page advise flag
        // mg   -   mergeable advise flag
        // um   -   userfaultfd missing pages tracking (since Linux 4.3)
        // uw   -   userfaultfd wprotect pages tracking (since Linux 4.3)
        string
    }
}

const PROGRAM_BREAK_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Eq, PartialEq)]
struct ProgramBreak {
    // These base address at which the data segment is mapped.
    base: UserAddress,

    // The current program break.
    //
    // The addresses from [base, current.round_up(*PAGE_SIZE)) are mapped into the
    // client address space from the underlying |memory|.
    current: UserAddress,
}

/// The policy about whether the address space can be dumped.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DumpPolicy {
    /// The address space cannot be dumped.
    ///
    /// Corresponds to SUID_DUMP_DISABLE.
    Disable,

    /// The address space can be dumped.
    ///
    /// Corresponds to SUID_DUMP_USER.
    User,
}

pub struct MemoryManagerState {
    /// The VMAR in which userspace mappings occur.
    ///
    /// We map userspace memory in this child VMAR so that we can destroy the
    /// entire VMAR during exec.
    /// For 32-bit tasks, we limit the user_vmar to correspond to the available memory.
    user_vmar: zx::Vmar,

    /// Cached VmarInfo for user_vmar.
    user_vmar_info: zx::VmarInfo,

    /// The memory mappings currently used by this address space.
    ///
    /// The mappings record which object backs each address.
    mappings: RangeMap<UserAddress, Mapping>,

    /// Memory object backing private, anonymous memory allocations in this address space.
    #[cfg(feature = "alternate_anon_allocs")]
    private_anonymous: PrivateAnonymousMemoryManager,

    forkable_state: MemoryManagerForkableState,
}

// 64k under the 4GB
const LOWER_4GB_LIMIT: UserAddress = UserAddress::const_from(0xffff_0000);

#[cfg(feature = "alternate_anon_allocs")]
struct PrivateAnonymousMemoryManager {
    /// Memory object backing private, anonymous memory allocations in this address space.
    backing: Arc<MemoryObject>,
}

#[cfg(feature = "alternate_anon_allocs")]
impl PrivateAnonymousMemoryManager {
    fn new(backing_size: u64) -> Self {
        let backing = Arc::new(MemoryObject::from(
            zx::Vmo::create(backing_size).unwrap().replace_as_executable(&VMEX_RESOURCE).unwrap(),
        ));
        Self { backing }
    }

    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.backing.read_uninit(bytes, addr.ptr() as u64).map_err(|_| errno!(EFAULT))
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<(), Errno> {
        self.backing.write(bytes, addr.ptr() as u64).map_err(|_| errno!(EFAULT))
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.backing
            .op_range(zx::VmoOp::ZERO, addr.ptr() as u64, length as u64)
            .map_err(|_| errno!(EFAULT))?;
        Ok(length)
    }

    fn move_pages(
        &self,
        source: &std::ops::Range<UserAddress>,
        dest: UserAddress,
    ) -> Result<(), Errno> {
        let length = source.end - source.start;
        let dest_memory_offset = dest.ptr() as u64;
        let source_memory_offset = source.start.ptr() as u64;
        self.backing
            .memmove(
                zx::TransferDataOptions::empty(),
                dest_memory_offset,
                source_memory_offset,
                length.try_into().unwrap(),
            )
            .map_err(impossible_error)?;
        Ok(())
    }

    fn snapshot(&self, backing_size: u64) -> Result<Self, Errno> {
        Ok(Self {
            backing: Arc::new(
                self.backing
                    .create_child(zx::VmoChildOptions::SNAPSHOT, 0, backing_size)
                    .map_err(impossible_error)?
                    .replace_as_executable(&VMEX_RESOURCE)
                    .map_err(impossible_error)?,
            ),
        })
    }
}

#[derive(Default, Clone)]
pub struct MemoryManagerForkableState {
    /// State for the brk and sbrk syscalls.
    brk: Option<ProgramBreak>,

    /// The namespace node that represents the executable associated with this task.
    executable_node: Option<NamespaceNode>,

    pub stack_size: usize,
    pub stack_start: UserAddress,
    pub auxv_start: UserAddress,
    pub auxv_end: UserAddress,
    pub argv_start: UserAddress,
    pub argv_end: UserAddress,
    pub environ_start: UserAddress,
    pub environ_end: UserAddress,

    /// vDSO location
    pub vdso_base: UserAddress,

    /// Randomized regions:
    pub mmap_top: UserAddress,
    pub stack_origin: UserAddress,
    pub brk_origin: UserAddress,
}

impl Deref for MemoryManagerState {
    type Target = MemoryManagerForkableState;
    fn deref(&self) -> &Self::Target {
        &self.forkable_state
    }
}

impl DerefMut for MemoryManagerState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.forkable_state
    }
}

fn map_in_vmar(
    vmar: &zx::Vmar,
    vmar_info: &zx::VmarInfo,
    addr: SelectedAddress,
    memory: &MemoryObject,
    memory_offset: u64,
    length: usize,
    flags: MappingFlags,
    populate: bool,
) -> Result<UserAddress, Errno> {
    profile_duration!("MapInVmar");
    let mut profile = ProfileDuration::enter("MapInVmarArgs");

    let base_addr = UserAddress::from_ptr(vmar_info.base);
    let (vmar_offset, vmar_extra_flags) = match addr {
        SelectedAddress::Fixed(addr) => (addr - base_addr, zx::VmarFlags::SPECIFIC),
        SelectedAddress::FixedOverwrite(addr) => (addr - base_addr, ZX_VM_SPECIFIC_OVERWRITE),
    };

    if populate {
        profile_duration!("MmapPopulate");
        let op = if flags.contains(MappingFlags::WRITE) {
            // Requires ZX_RIGHT_WRITEABLE which we should expect when the mapping is writeable.
            zx::VmoOp::COMMIT
        } else {
            // When we don't expect to have ZX_RIGHT_WRITEABLE, fall back to a VMO op that doesn't
            // need it.
            zx::VmoOp::PREFETCH
        };
        trace_duration!(CATEGORY_STARNIX_MM, c"MmapCommitPages");
        let _ = memory.op_range(op, memory_offset, length as u64);
        // "The mmap() call doesn't fail if the mapping cannot be populated."
    }

    let vmar_maybe_map_range = if populate && !vmar_extra_flags.contains(ZX_VM_SPECIFIC_OVERWRITE) {
        zx::VmarFlags::MAP_RANGE
    } else {
        zx::VmarFlags::empty()
    };
    let vmar_flags = flags.access_flags().to_vmar_flags()
        | zx::VmarFlags::ALLOW_FAULTS
        | vmar_extra_flags
        | vmar_maybe_map_range;

    profile.pivot("VmarMapSyscall");
    let map_result = memory.map_in_vmar(vmar, vmar_offset, memory_offset, length, vmar_flags);
    let mapped_addr = map_result.map_err(MemoryManager::get_errno_for_map_err)?;

    Ok(UserAddress::from_ptr(mapped_addr))
}

impl MemoryManagerState {
    /// Returns occupied address ranges that intersect with the given range.
    ///
    /// An address range is "occupied" if (a) there is already a mapping in that range or (b) there
    /// is a GROWSDOWN mapping <= 256 pages above that range. The 256 pages below a GROWSDOWN
    /// mapping is the "guard region." The memory manager avoids mapping memory in the guard region
    /// in some circumstances to preserve space for the GROWSDOWN mapping to grow down.
    fn get_occupied_address_ranges<'a>(
        &'a self,
        subrange: &'a Range<UserAddress>,
    ) -> impl Iterator<Item = Range<UserAddress>> + 'a {
        let query_range = subrange.start
            ..(subrange
                .end
                .saturating_add(*PAGE_SIZE as usize * GUARD_PAGE_COUNT_FOR_GROWSDOWN_MAPPINGS));
        self.mappings.intersection(query_range).filter_map(|(range, mapping)| {
            let occupied_range = mapping.inflate_to_include_guard_pages(range);
            if occupied_range.start < subrange.end && subrange.start < occupied_range.end {
                Some(occupied_range)
            } else {
                None
            }
        })
    }

    fn count_possible_placements(
        &self,
        length: usize,
        subrange: &Range<UserAddress>,
    ) -> Option<usize> {
        let mut occupied_ranges = self.get_occupied_address_ranges(subrange);
        let mut possible_placements = 0;
        // If the allocation is placed at the first available address, every page that is left
        // before the next mapping or the end of subrange is +1 potential placement.
        let mut first_fill_end = subrange.start.checked_add(length)?;
        while first_fill_end <= subrange.end {
            let Some(mapping) = occupied_ranges.next() else {
                possible_placements += (subrange.end - first_fill_end) / (*PAGE_SIZE as usize) + 1;
                break;
            };
            if mapping.start >= first_fill_end {
                possible_placements += (mapping.start - first_fill_end) / (*PAGE_SIZE as usize) + 1;
            }
            first_fill_end = mapping.end.checked_add(length)?;
        }
        Some(possible_placements)
    }

    fn pick_placement(
        &self,
        length: usize,
        mut chosen_placement_idx: usize,
        subrange: &Range<UserAddress>,
    ) -> Option<UserAddress> {
        let mut candidate =
            Range { start: subrange.start, end: subrange.start.checked_add(length)? };
        let mut occupied_ranges = self.get_occupied_address_ranges(subrange);
        loop {
            let Some(mapping) = occupied_ranges.next() else {
                // No more mappings: treat the rest of the index as an offset.
                let res =
                    candidate.start.checked_add(chosen_placement_idx * *PAGE_SIZE as usize)?;
                debug_assert!(res.checked_add(length)? <= subrange.end);
                return Some(res);
            };
            if mapping.start < candidate.end {
                // doesn't fit, skip
                candidate = Range { start: mapping.end, end: mapping.end.checked_add(length)? };
                continue;
            }
            let unused_space =
                (mapping.start.ptr() - candidate.end.ptr()) / (*PAGE_SIZE as usize) + 1;
            if unused_space > chosen_placement_idx {
                // Chosen placement is within the range; treat the rest of the index as an offset.
                let res =
                    candidate.start.checked_add(chosen_placement_idx * *PAGE_SIZE as usize)?;
                return Some(res);
            }

            // chosen address is further up, skip
            chosen_placement_idx -= unused_space;
            candidate = Range { start: mapping.end, end: mapping.end.checked_add(length)? };
        }
    }

    fn find_random_unused_range(
        &self,
        length: usize,
        subrange: &Range<UserAddress>,
    ) -> Option<UserAddress> {
        let possible_placements = self.count_possible_placements(length, subrange)?;
        if possible_placements == 0 {
            return None;
        }
        let chosen_placement_idx = thread_rng().gen_range(0..possible_placements);
        self.pick_placement(length, chosen_placement_idx, subrange)
    }

    // Find the first unused range of addresses that fits a mapping of `length` bytes, searching
    // from `mmap_top` downwards.
    pub fn find_next_unused_range(&self, length: usize) -> Option<UserAddress> {
        // Iterate over existing mappings within range, in descending order
        let mut map_iter = self.mappings.iter_ending_at(&self.mmap_top);
        // Currently considered range. Will be moved downwards if it intersects the current mapping.
        let mut candidate = Range { start: self.mmap_top.checked_sub(length)?, end: self.mmap_top };

        loop {
            // Is there a next mapping? If not, the candidate is already good.
            let Some((occupied_range, mapping)) = map_iter.next_back() else {
                return Some(candidate.start);
            };
            let occupied_range = mapping.inflate_to_include_guard_pages(occupied_range);
            // If it doesn't overlap, the gap is big enough to fit.
            if occupied_range.end <= candidate.start {
                return Some(candidate.start);
            }
            // If there was a mapping in the way, the next range to consider will be `length` bytes
            // below.
            candidate = Range {
                start: occupied_range.start.checked_sub(length)?,
                end: occupied_range.start,
            };
        }
    }

    // Accept the hint if the range is unused and does not wrap around.
    fn is_hint_acceptable(&self, hint_addr: UserAddress, length: usize) -> bool {
        let Some(hint_end) = hint_addr.checked_add(length) else {
            return false;
        };
        self.get_occupied_address_ranges(&(hint_addr..hint_end)).next().is_none()
    }

    fn select_address(
        &self,
        addr: DesiredAddress,
        length: usize,
        flags: MappingFlags,
    ) -> Result<SelectedAddress, Errno> {
        let adjusted_length = round_up_to_system_page_size(length)?;

        let find_address = || -> Result<SelectedAddress, Errno> {
            profile_duration!("FindAddressForMmap");
            let new_addr = if flags.contains(MappingFlags::LOWER_32BIT) {
                // MAP_32BIT specifies that the memory allocated will
                // be within the first 2 GB of the process address space.
                self.find_random_unused_range(
                    adjusted_length,
                    &(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
                        ..UserAddress::from_ptr(0x80000000)),
                )
                .ok_or_else(|| errno!(ENOMEM))?
            } else {
                self.find_next_unused_range(adjusted_length).ok_or_else(|| errno!(ENOMEM))?
            };

            Ok(SelectedAddress::Fixed(new_addr))
        };

        Ok(match addr {
            DesiredAddress::Any => find_address()?,
            DesiredAddress::Hint(hint_addr) => {
                // Round down to page size
                let hint_addr =
                    UserAddress::from_ptr(hint_addr.ptr() - hint_addr.ptr() % *PAGE_SIZE as usize);
                if self.is_hint_acceptable(hint_addr, adjusted_length) {
                    SelectedAddress::Fixed(hint_addr)
                } else {
                    find_address()?
                }
            }
            DesiredAddress::Fixed(addr) => SelectedAddress::Fixed(addr),
            DesiredAddress::FixedOverwrite(addr) => SelectedAddress::FixedOverwrite(addr),
        })
    }

    // Map the memory without updating `self.mappings`.
    fn map_in_user_vmar(
        &self,
        addr: SelectedAddress,
        memory: &MemoryObject,
        memory_offset: u64,
        length: usize,
        flags: MappingFlags,
        populate: bool,
    ) -> Result<UserAddress, Errno> {
        map_in_vmar(
            &self.user_vmar,
            &self.user_vmar_info,
            addr,
            memory,
            memory_offset,
            length,
            flags,
            populate,
        )
    }

    fn validate_addr(&self, addr: DesiredAddress, length: usize) -> Result<(), Errno> {
        if let DesiredAddress::FixedOverwrite(addr) = addr {
            if self.check_has_unauthorized_splits(addr, length) {
                return error!(ENOMEM);
            }
        }
        Ok(())
    }

    fn map_memory(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: DesiredAddress,
        memory: Arc<MemoryObject>,
        memory_offset: u64,
        length: usize,
        flags: MappingFlags,
        max_access: Access,
        populate: bool,
        name: MappingName,
        file_write_guard: FileWriteGuardRef,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        self.validate_addr(addr, length)?;

        let mapped_addr = self.map_in_user_vmar(
            self.select_address(addr, length, flags)?,
            &memory,
            memory_offset,
            length,
            flags,
            populate,
        )?;

        #[cfg(any(test, debug_assertions))]
        {
            // Take the lock on directory entry while holding the one on the mm state to ensure any
            // wrong ordering will trigger the tracing-mutex at the right call site.
            if let MappingName::File(name) = &name {
                let _l1 = name.entry.parent();
            }
        }

        profile_duration!("FinishMapping");
        let end = (mapped_addr + length).round_up(*PAGE_SIZE)?;

        if let DesiredAddress::FixedOverwrite(addr) = addr {
            assert_eq!(addr, mapped_addr);
            self.update_after_unmap(mm, addr, end - addr, released_mappings)?;
        }

        let mut mapping =
            Mapping::new(mapped_addr, memory, memory_offset, flags, max_access, file_write_guard);
        mapping.name = name;
        self.mappings.insert(mapped_addr..end, mapping);

        Ok(mapped_addr)
    }

    #[cfg(feature = "alternate_anon_allocs")]
    fn map_private_anonymous(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: DesiredAddress,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        name: MappingName,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        self.validate_addr(addr, length)?;

        let flags = MappingFlags::from_access_flags_and_options(prot_flags, options);
        let selected_addr = self.select_address(addr, length, flags)?;
        let backing_memory_offset = selected_addr.addr().ptr();

        let mapped_addr = self.map_in_user_vmar(
            selected_addr,
            &self.private_anonymous.backing,
            backing_memory_offset as u64,
            length,
            flags,
            false,
        )?;

        let end = (mapped_addr + length).round_up(*PAGE_SIZE)?;
        if let DesiredAddress::FixedOverwrite(addr) = addr {
            assert_eq!(addr, mapped_addr);
            self.update_after_unmap(mm, addr, end - addr, released_mappings)?;
        }

        let mapping = Mapping::new_private_anonymous(flags, name);
        self.mappings.insert(mapped_addr..end, mapping);

        Ok(mapped_addr)
    }

    fn map_anonymous(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: DesiredAddress,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        name: MappingName,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        #[cfg(feature = "alternate_anon_allocs")]
        if !options.contains(MappingOptions::SHARED) {
            return self.map_private_anonymous(
                mm,
                addr,
                length,
                prot_flags,
                options,
                name,
                released_mappings,
            );
        }
        let memory = create_anonymous_mapping_memory(length as u64)?;
        let flags = MappingFlags::from_access_flags_and_options(prot_flags, options);
        self.map_memory(
            mm,
            addr,
            memory,
            0,
            length,
            flags,
            Access::rwx(),
            options.contains(MappingOptions::POPULATE),
            name,
            FileWriteGuardRef(None),
            released_mappings,
        )
    }

    fn remap(
        &mut self,
        _current_task: &CurrentTask,
        mm: &Arc<MemoryManager>,
        old_addr: UserAddress,
        old_length: usize,
        new_length: usize,
        flags: MremapFlags,
        new_addr: UserAddress,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        // MREMAP_FIXED moves a mapping, which requires MREMAP_MAYMOVE.
        if flags.contains(MremapFlags::FIXED) && !flags.contains(MremapFlags::MAYMOVE) {
            return error!(EINVAL);
        }

        // MREMAP_DONTUNMAP is always a move to a specific address,
        // which requires MREMAP_FIXED. There is no resizing allowed either.
        if flags.contains(MremapFlags::DONTUNMAP)
            && (!flags.contains(MremapFlags::FIXED) || old_length != new_length)
        {
            return error!(EINVAL);
        }

        // In-place copies are invalid.
        if !flags.contains(MremapFlags::MAYMOVE) && old_length == 0 {
            return error!(ENOMEM);
        }

        if flags.contains(MremapFlags::DONTUNMAP) {
            track_stub!(TODO("https://fxbug.dev/297372077"), "MREMAP_DONTUNMAP");
            return error!(EOPNOTSUPP);
        }

        if new_length == 0 {
            return error!(EINVAL);
        }

        // Make sure old_addr is page-aligned.
        if !old_addr.is_aligned(*PAGE_SIZE) {
            return error!(EINVAL);
        }

        let old_length = round_up_to_system_page_size(old_length)?;
        let new_length = round_up_to_system_page_size(new_length)?;

        if self.check_has_unauthorized_splits(old_addr, old_length) {
            return error!(EINVAL);
        }

        if self.check_has_unauthorized_splits(new_addr, new_length) {
            return error!(EINVAL);
        }

        if !flags.contains(MremapFlags::FIXED) && old_length != 0 {
            // We are not requested to remap to a specific address, so first we see if we can remap
            // in-place. In-place copies (old_length == 0) are not allowed.
            if let Some(new_addr) =
                self.try_remap_in_place(mm, old_addr, old_length, new_length, released_mappings)?
            {
                return Ok(new_addr);
            }
        }

        // There is no space to grow in place, or there is an explicit request to move.
        if flags.contains(MremapFlags::MAYMOVE) {
            let dst_address =
                if flags.contains(MremapFlags::FIXED) { Some(new_addr) } else { None };
            self.remap_move(mm, old_addr, old_length, dst_address, new_length, released_mappings)
        } else {
            error!(ENOMEM)
        }
    }

    /// Attempts to grow or shrink the mapping in-place. Returns `Ok(Some(addr))` if the remap was
    /// successful. Returns `Ok(None)` if there was no space to grow.
    fn try_remap_in_place(
        &mut self,
        mm: &Arc<MemoryManager>,
        old_addr: UserAddress,
        old_length: usize,
        new_length: usize,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<Option<UserAddress>, Errno> {
        let old_range = old_addr..old_addr.checked_add(old_length).ok_or_else(|| errno!(EINVAL))?;
        let new_range_in_place =
            old_addr..old_addr.checked_add(new_length).ok_or_else(|| errno!(EINVAL))?;

        if new_length <= old_length {
            // Shrink the mapping in-place, which should always succeed.
            // This is done by unmapping the extraneous region.
            if new_length != old_length {
                self.unmap(mm, new_range_in_place.end, old_length - new_length, released_mappings)?;
            }
            return Ok(Some(old_addr));
        }

        if self.mappings.intersection(old_range.end..new_range_in_place.end).next().is_some() {
            // There is some mapping in the growth range prevening an in-place growth.
            return Ok(None);
        }

        // There is space to grow in-place. The old range must be one contiguous mapping.
        let (original_range, mapping) =
            self.mappings.get(&old_addr).ok_or_else(|| errno!(EINVAL))?;

        if old_range.end > original_range.end {
            return error!(EFAULT);
        }
        let original_range = original_range.clone();
        let original_mapping = mapping.clone();

        // Compute the new length of the entire mapping once it has grown.
        let final_length = (original_range.end - original_range.start) + (new_length - old_length);

        let private_anonymous = original_mapping.private_anonymous();

        // As a special case for private, anonymous mappings, allocate more space in the
        // memory object. FD-backed mappings have their backing memory handled by the file system.
        match original_mapping.backing {
            MappingBacking::Memory(backing) => {
                if private_anonymous {
                    let new_memory_size = backing
                        .memory_offset
                        .checked_add(final_length as u64)
                        .ok_or_else(|| errno!(EINVAL))?;
                    backing
                        .memory
                        .set_size(new_memory_size)
                        .map_err(MemoryManager::get_errno_for_map_err)?;
                    // Zero-out the pages that were added when growing. This is not necessary, but ensures
                    // correctness of our COW implementation. Ignore any errors.
                    let original_length = original_range.end - original_range.start;
                    let _ = backing.memory.op_range(
                        zx::VmoOp::ZERO,
                        backing.memory_offset + original_length as u64,
                        (final_length - original_length) as u64,
                    );
                }

                // Re-map the original range, which may include pages before the requested range.
                Ok(Some(self.map_memory(
                    mm,
                    DesiredAddress::FixedOverwrite(original_range.start),
                    backing.memory,
                    backing.memory_offset,
                    final_length,
                    original_mapping.flags,
                    original_mapping.max_access,
                    false,
                    original_mapping.name,
                    original_mapping.file_write_guard,
                    released_mappings,
                )?))
            }
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                let growth_start = original_range.end;
                let growth_length = new_length - old_length;
                // Map new pages to back the growth.
                self.map_in_user_vmar(
                    SelectedAddress::FixedOverwrite(growth_start),
                    &self.private_anonymous.backing,
                    growth_start.ptr() as u64,
                    growth_length,
                    original_mapping.flags,
                    false,
                )?;
                // Overwrite the mapping entry with the new larger size.
                self.mappings.insert(
                    original_range.start..original_range.start + final_length,
                    original_mapping.clone(),
                );
                Ok(Some(original_range.start))
            }
        }
    }

    /// Grows or shrinks the mapping while moving it to a new destination.
    fn remap_move(
        &mut self,
        mm: &Arc<MemoryManager>,
        src_addr: UserAddress,
        src_length: usize,
        dst_addr: Option<UserAddress>,
        dst_length: usize,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        let src_range = src_addr..src_addr.checked_add(src_length).ok_or_else(|| errno!(EINVAL))?;
        let (original_range, src_mapping) =
            self.mappings.get(&src_addr).ok_or_else(|| errno!(EINVAL))?;
        let original_range = original_range.clone();
        let src_mapping = src_mapping.clone();

        if src_length == 0 && !src_mapping.flags.contains(MappingFlags::SHARED) {
            // src_length == 0 means that the mapping is to be copied. This behavior is only valid
            // with MAP_SHARED mappings.
            return error!(EINVAL);
        }

        // If the destination range is smaller than the source range, we must first shrink
        // the source range in place. This must be done now and visible to processes, even if
        // a later failure causes the remap operation to fail.
        if src_length != 0 && src_length > dst_length {
            self.unmap(mm, src_addr + dst_length, src_length - dst_length, released_mappings)?;
        }

        let dst_addr_for_map = match dst_addr {
            None => DesiredAddress::Any,
            Some(dst_addr) => {
                // The mapping is being moved to a specific address.
                let dst_range =
                    dst_addr..(dst_addr.checked_add(dst_length).ok_or_else(|| errno!(EINVAL))?);
                if !src_range.intersect(&dst_range).is_empty() {
                    return error!(EINVAL);
                }

                // The destination range must be unmapped. This must be done now and visible to
                // processes, even if a later failure causes the remap operation to fail.
                self.unmap(mm, dst_addr, dst_length, released_mappings)?;

                DesiredAddress::Fixed(dst_addr)
            }
        };

        // According to gVisor's aio_test, Linux checks for DONT_EXPAND after unmapping the dst
        // range.
        if dst_length > src_length && src_mapping.flags.contains(MappingFlags::DONT_EXPAND) {
            return error!(EFAULT);
        }

        if src_range.end > original_range.end {
            // The source range is not one contiguous mapping. This check must be done only after
            // the source range is shrunk and the destination unmapped.
            return error!(EFAULT);
        }

        let offset_into_original_range = (src_addr - original_range.start) as u64;
        let private_anonymous = src_mapping.private_anonymous();
        let (dst_memory_offset, memory) = match src_mapping.backing {
            MappingBacking::Memory(backing) => {
                if private_anonymous {
                    // This mapping is a private, anonymous mapping. Create a COW child memory object that covers
                    // the pages being moved and map that into the destination.
                    let child_memory = backing
                        .memory
                        .create_child(
                            zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::RESIZABLE,
                            backing.memory_offset + offset_into_original_range,
                            dst_length as u64,
                        )
                        .map_err(MemoryManager::get_errno_for_map_err)?;
                    if dst_length > src_length {
                        // The mapping has grown. Zero-out the pages that were "added" when growing the
                        // mapping. These pages might be pointing inside the parent memory object, in which case
                        // we want to zero them out to make them look like new pages. Since this is a COW
                        // child memory object, this will simply allocate new pages.
                        // This is not necessary, but ensures correctness of our COW implementation.
                        // Ignore any errors.
                        let _ = child_memory.op_range(
                            zx::VmoOp::ZERO,
                            src_length as u64,
                            (dst_length - src_length) as u64,
                        );
                    }
                    (0, Arc::new(MemoryObject::from(child_memory)))
                } else {
                    // This mapping is backed by an FD, just map the range of the memory object covering the moved
                    // pages. If the memory object already had COW semantics, this preserves them.
                    (backing.memory_offset + offset_into_original_range, backing.memory)
                }
            }
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                let dst_addr =
                    self.select_address(dst_addr_for_map, dst_length, src_mapping.flags)?.addr();

                let length_to_move = std::cmp::min(dst_length, src_length) as u64;

                if dst_addr != src_addr {
                    let range_to_move = src_range.start..src_range.start + length_to_move;
                    // Move the previously mapped pages into their new location.
                    self.private_anonymous.move_pages(&range_to_move, dst_addr)?;
                }

                self.map_in_user_vmar(
                    SelectedAddress::FixedOverwrite(dst_addr),
                    &self.private_anonymous.backing,
                    dst_addr.ptr() as u64,
                    dst_length,
                    src_mapping.flags,
                    false,
                )?;

                if dst_length > src_length {
                    // The mapping has grown, map new pages in to cover the growth.
                    let growth_start_addr = dst_addr + length_to_move;
                    let growth_length = dst_length - src_length;

                    self.map_private_anonymous(
                        mm,
                        DesiredAddress::FixedOverwrite(growth_start_addr),
                        growth_length,
                        src_mapping.flags.access_flags(),
                        src_mapping.flags.options(),
                        src_mapping.name.clone(),
                        released_mappings,
                    )?;
                }

                self.mappings.insert(
                    dst_addr..dst_addr + dst_length,
                    Mapping::new_private_anonymous(src_mapping.flags, src_mapping.name.clone()),
                );

                if dst_addr != src_addr && src_length != 0 {
                    self.unmap(mm, src_addr, src_length, released_mappings)?;
                }

                return Ok(dst_addr);
            }
        };

        let new_address = self.map_memory(
            mm,
            dst_addr_for_map,
            memory,
            dst_memory_offset,
            dst_length,
            src_mapping.flags,
            src_mapping.max_access,
            false,
            src_mapping.name,
            src_mapping.file_write_guard,
            released_mappings,
        )?;

        if src_length != 0 {
            // Only unmap the source range if this is not a copy. It was checked earlier that
            // this mapping is MAP_SHARED.
            self.unmap(mm, src_addr, src_length, released_mappings)?;
        }

        Ok(new_address)
    }

    // Checks if an operation may be performed over the target mapping that may
    // result in a split mapping.
    //
    // An operation may be forbidden if the target mapping only partially covers
    // an existing mapping with the `MappingOptions::DONT_SPLIT` flag set.
    fn check_has_unauthorized_splits(&self, addr: UserAddress, length: usize) -> bool {
        let query_range = addr..addr.saturating_add(length);
        let mut intersection = self.mappings.intersection(query_range.clone());

        // A mapping is not OK if it disallows splitting and the target range
        // does not fully cover the mapping range.
        let check_if_mapping_has_unauthorized_split =
            |mapping: Option<(&Range<UserAddress>, &Mapping)>| {
                mapping.is_some_and(|(mapping_range, mapping)| {
                    mapping.flags.contains(MappingFlags::DONT_SPLIT)
                        && (mapping_range.start < query_range.start
                            || query_range.end < mapping_range.end)
                })
            };

        // We only check the first and last mappings in the range because naturally,
        // the mappings in the middle are fully covered by the target mapping and
        // won't be split.
        check_if_mapping_has_unauthorized_split(intersection.next())
            || check_if_mapping_has_unauthorized_split(intersection.last())
    }

    /// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
    fn unmap(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: UserAddress,
        length: usize,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<(), Errno> {
        if !addr.is_aligned(*PAGE_SIZE) {
            return error!(EINVAL);
        }
        let length = round_up_to_system_page_size(length)?;
        if length == 0 {
            return error!(EINVAL);
        }

        if self.check_has_unauthorized_splits(addr, length) {
            return error!(EINVAL);
        }

        // Unmap the range, including the the tail of any range that would have been split. This
        // operation is safe because we're operating on another process.
        match unsafe { self.user_vmar.unmap(addr.ptr(), length) } {
            Ok(_) => (),
            Err(zx::Status::NOT_FOUND) => (),
            Err(zx::Status::INVALID_ARGS) => return error!(EINVAL),
            Err(status) => {
                impossible_error(status);
            }
        };

        self.update_after_unmap(mm, addr, length, released_mappings)?;

        Ok(())
    }

    // Updates `self.mappings` after the specified range was unmaped.
    //
    // The range to unmap can span multiple mappings, and can split mappings if
    // the range start or end falls in the middle of a mapping.
    //
    // For example, with this set of mappings and unmap range `R`:
    //
    //   [  A  ][ B ] [    C    ]     <- mappings
    //      |-------------|           <- unmap range R
    //
    // Assuming the mappings are all MAP_ANONYMOUS:
    // - the pages of A, B, and C that fall in range R are unmapped; the memory object backing B is dropped.
    // - the memory object backing A is shrunk.
    // - a COW child memory object is created from C, which is mapped in the range of C that falls outside R.
    //
    // File-backed mappings don't need to have their memory object modified.
    //
    // Unmapped mappings are placed in `released_mappings`.
    fn update_after_unmap(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: UserAddress,
        length: usize,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<(), Errno> {
        profile_duration!("UpdateAfterUnmap");
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
        let unmap_range = addr..end_addr;

        #[cfg(feature = "alternate_anon_allocs")]
        {
            for (range, mapping) in self.mappings.intersection(&unmap_range) {
                // Deallocate any pages in the private, anonymous backing that are now unreachable.
                if let MappingBacking::PrivateAnonymous = mapping.backing {
                    let unmapped_range = &unmap_range.intersect(range);

                    mm.inflight_vmspliced_payloads
                        .handle_unmapping(&self.private_anonymous.backing, unmapped_range)?;

                    self.private_anonymous
                        .zero(unmapped_range.start, unmapped_range.end - unmapped_range.start)?;
                }
            }
            released_mappings.extend(self.mappings.remove(&unmap_range));
            return Ok(());
        }

        #[cfg(not(feature = "alternate_anon_allocs"))]
        {
            // Find the private, anonymous mapping that will get its tail cut off by this unmap call.
            let truncated_head = match self.mappings.get(&addr) {
                Some((range, mapping)) if range.start != addr && mapping.private_anonymous() => {
                    Some((range.start..addr, mapping.clone()))
                }
                _ => None,
            };

            // Find the private, anonymous mapping that will get its head cut off by this unmap call.
            // A mapping that starts exactly at `end_addr` is excluded since it is not affected by
            // the unmapping.
            let truncated_tail = match self.mappings.get(&end_addr) {
                Some((range, mapping))
                    if range.start != end_addr && mapping.private_anonymous() =>
                {
                    Some((end_addr..range.end, mapping.clone()))
                }
                _ => None,
            };

            // Remove the original range of mappings from our map.
            released_mappings.extend(self.mappings.remove(&(addr..end_addr)));

            if let Some((range, mut mapping)) = truncated_tail {
                let MappingBacking::Memory(backing) = &mut mapping.backing;
                mm.inflight_vmspliced_payloads
                    .handle_unmapping(&backing.memory, &unmap_range.intersect(&range))?;

                // Create and map a child COW memory object mapping that represents the truncated tail.
                let memory_info = backing.memory.basic_info();
                let child_memory_offset =
                    (range.start - backing.base) as u64 + backing.memory_offset;
                let child_length = range.end - range.start;
                let mut child_memory = backing
                    .memory
                    .create_child(
                        zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::RESIZABLE,
                        child_memory_offset,
                        child_length as u64,
                    )
                    .map_err(MemoryManager::get_errno_for_map_err)?;
                if memory_info.rights.contains(zx::Rights::EXECUTE) {
                    child_memory = child_memory
                        .replace_as_executable(&VMEX_RESOURCE)
                        .map_err(impossible_error)?;
                }

                // Update the mapping.
                backing.memory = Arc::new(child_memory);
                backing.base = range.start;
                backing.memory_offset = 0;

                self.map_in_user_vmar(
                    SelectedAddress::FixedOverwrite(range.start),
                    &backing.memory,
                    0,
                    child_length,
                    mapping.flags,
                    false,
                )?;

                // Replace the mapping with a new one that contains updated memory object handle.
                self.mappings.insert(range, mapping);
            }

            if let Some((range, mapping)) = truncated_head {
                let MappingBacking::Memory(backing) = &mapping.backing;
                mm.inflight_vmspliced_payloads
                    .handle_unmapping(&backing.memory, &unmap_range.intersect(&range))?;

                // Resize the memory object of the head mapping, whose tail was cut off.
                let new_mapping_size = (range.end - range.start) as u64;
                let new_memory_size = backing.memory_offset + new_mapping_size;
                backing
                    .memory
                    .set_size(new_memory_size)
                    .map_err(MemoryManager::get_errno_for_map_err)?;
            }

            Ok(())
        }
    }

    fn protect(
        &mut self,
        addr: UserAddress,
        length: usize,
        prot_flags: ProtectionFlags,
    ) -> Result<(), Errno> {
        profile_duration!("Protect");
        let vmar_flags = prot_flags.to_vmar_flags();
        let page_size = *PAGE_SIZE;
        let end = addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?.round_up(page_size)?;

        if self.check_has_unauthorized_splits(addr, length) {
            return error!(EINVAL);
        }

        let prot_range = if prot_flags.contains(ProtectionFlags::GROWSDOWN) {
            let mut start = addr;
            let Some((range, mapping)) = self.mappings.get(&start) else {
                return error!(EINVAL);
            };
            // Ensure that the mapping has GROWSDOWN if PROT_GROWSDOWN was specified.
            if !mapping.flags.contains(MappingFlags::GROWSDOWN) {
                return error!(EINVAL);
            }
            let access_flags = mapping.flags.access_flags();
            // From <https://man7.org/linux/man-pages/man2/mprotect.2.html>:
            //
            //   PROT_GROWSDOWN
            //     Apply the protection mode down to the beginning of a
            //     mapping that grows downward (which should be a stack
            //     segment or a segment mapped with the MAP_GROWSDOWN flag
            //     set).
            start = range.start;
            while let Some((range, mapping)) =
                self.mappings.get(&start.saturating_sub(page_size as usize))
            {
                if !mapping.flags.contains(MappingFlags::GROWSDOWN)
                    || mapping.flags.access_flags() != access_flags
                {
                    break;
                }
                start = range.start;
            }
            start..end
        } else {
            addr..end
        };

        let addr = prot_range.start;
        let length = prot_range.end - prot_range.start;

        // TODO: We should check the max_access flags on all the mappings in this range.
        //       There are cases where max_access is more restrictive than the Zircon rights
        //       we hold on the underlying VMOs.

        // Make one call to mprotect to update all the zircon protections.
        // SAFETY: This is safe because the vmar belongs to a different process.
        unsafe { self.user_vmar.protect(addr.ptr(), length, vmar_flags) }.map_err(|s| match s {
            zx::Status::INVALID_ARGS => errno!(EINVAL),
            zx::Status::NOT_FOUND => {
                track_stub!(
                    TODO("https://fxbug.dev/322875024"),
                    "mprotect: succeed and update prot after NOT_FOUND"
                );
                errno!(EINVAL)
            }
            zx::Status::ACCESS_DENIED => errno!(EACCES),
            _ => impossible_error(s),
        })?;

        // Update the flags on each mapping in the range.
        let mut updates = vec![];
        for (range, mapping) in self.mappings.intersection(prot_range.clone()) {
            let range = range.intersect(&prot_range);
            let mut mapping = mapping.clone();
            mapping.flags = mapping.flags.with_access_flags(prot_flags);
            updates.push((range, mapping));
        }
        // Use a separate loop to avoid mutating the mappings structure while iterating over it.
        for (range, mapping) in updates {
            self.mappings.insert(range, mapping);
        }
        Ok(())
    }

    fn madvise(
        &mut self,
        _current_task: &CurrentTask,
        addr: UserAddress,
        length: usize,
        advice: u32,
    ) -> Result<(), Errno> {
        profile_duration!("Madvise");
        if !addr.is_aligned(*PAGE_SIZE) {
            return error!(EINVAL);
        }

        let end_addr =
            addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?.round_up(*PAGE_SIZE)?;
        if end_addr > self.max_address() {
            return error!(EFAULT);
        }

        if advice == MADV_NORMAL {
            return Ok(());
        }

        let mut updates = vec![];
        let range_for_op = addr..end_addr;
        for (range, mapping) in self.mappings.intersection(&range_for_op) {
            let range_to_zero = range.intersect(&range_for_op);
            if range_to_zero.is_empty() {
                continue;
            }
            let start = mapping.address_to_offset(range_to_zero.start);
            let end = mapping.address_to_offset(range_to_zero.end);
            if advice == MADV_DONTFORK
                || advice == MADV_DOFORK
                || advice == MADV_WIPEONFORK
                || advice == MADV_KEEPONFORK
            {
                // WIPEONFORK is only supported on private anonymous mappings per madvise(2).
                // KEEPONFORK can be specified on ranges that cover other sorts of mappings. It should
                // have no effect on mappings that are not private and anonymous as such mappings cannot
                // have the WIPEONFORK option set.
                if advice == MADV_WIPEONFORK && !mapping.private_anonymous() {
                    return error!(EINVAL);
                }
                let new_flags = match advice {
                    MADV_DONTFORK => mapping.flags | MappingFlags::DONTFORK,
                    MADV_DOFORK => mapping.flags & MappingFlags::DONTFORK.complement(),
                    MADV_WIPEONFORK => mapping.flags | MappingFlags::WIPEONFORK,
                    MADV_KEEPONFORK => mapping.flags & MappingFlags::WIPEONFORK.complement(),
                    _ => mapping.flags,
                };
                let new_mapping = match &mapping.backing {
                    MappingBacking::Memory(backing) => Mapping::with_name(
                        range_to_zero.start,
                        backing.memory.clone(),
                        backing.memory_offset + start,
                        new_flags,
                        mapping.max_access,
                        mapping.name.clone(),
                        mapping.file_write_guard.clone(),
                    ),
                    #[cfg(feature = "alternate_anon_allocs")]
                    MappingBacking::PrivateAnonymous => {
                        Mapping::new_private_anonymous(new_flags, mapping.name.clone())
                    }
                };
                updates.push((range_to_zero, new_mapping));
            } else {
                if mapping.flags.contains(MappingFlags::SHARED) {
                    continue;
                }
                let op = match advice {
                    MADV_DONTNEED if !mapping.flags.contains(MappingFlags::ANONYMOUS) => {
                        // Note, we cannot simply implemented MADV_DONTNEED with
                        // zx::VmoOp::DONT_NEED because they have different
                        // semantics.
                        track_stub!(
                            TODO("https://fxbug.dev/322874496"),
                            "MADV_DONTNEED with file-backed mapping"
                        );
                        return error!(EINVAL);
                    }
                    MADV_DONTNEED => zx::VmoOp::ZERO,
                    MADV_WILLNEED => {
                        if mapping.flags.contains(MappingFlags::WRITE) {
                            zx::VmoOp::COMMIT
                        } else {
                            zx::VmoOp::PREFETCH
                        }
                    }
                    MADV_NOHUGEPAGE => return Ok(()),
                    advice => {
                        track_stub!(TODO("https://fxbug.dev/322874202"), "madvise", advice);
                        return error!(EINVAL);
                    }
                };

                let memory = match &mapping.backing {
                    MappingBacking::Memory(backing) => &backing.memory,
                    #[cfg(feature = "alternate_anon_allocs")]
                    MappingBacking::PrivateAnonymous => &self.private_anonymous.backing,
                };
                memory.op_range(op, start, end - start).map_err(|s| match s {
                    zx::Status::OUT_OF_RANGE => errno!(EINVAL),
                    zx::Status::NO_MEMORY => errno!(ENOMEM),
                    zx::Status::INVALID_ARGS => errno!(EINVAL),
                    zx::Status::ACCESS_DENIED => errno!(EACCES),
                    _ => impossible_error(s),
                })?;
            }
        }
        // Use a separate loop to avoid mutating the mappings structure while iterating over it.
        for (range, mapping) in updates {
            self.mappings.insert(range, mapping);
        }
        Ok(())
    }

    fn max_address(&self) -> UserAddress {
        UserAddress::from_ptr(self.user_vmar_info.base + self.user_vmar_info.len)
    }

    fn user_address_to_vmar_offset(&self, addr: UserAddress) -> Result<usize, ()> {
        if !(self.user_vmar_info.base..self.user_vmar_info.base + self.user_vmar_info.len)
            .contains(&addr.ptr())
        {
            return Err(());
        }
        Ok((addr - self.user_vmar_info.base).ptr())
    }

    fn get_mappings_for_vmsplice(
        &self,
        mm: &Arc<MemoryManager>,
        buffers: &UserBuffers,
    ) -> Result<Vec<Arc<VmsplicePayload>>, Errno> {
        let mut vmsplice_mappings = Vec::new();

        for UserBuffer { mut address, length } in buffers.iter().copied() {
            let mappings = self.get_contiguous_mappings_at(address, length)?;
            for (mapping, length) in mappings {
                let vmsplice_payload = match &mapping.backing {
                    MappingBacking::Memory(m) => VmsplicePayloadSegment {
                        addr_offset: address,
                        length,
                        memory: m.memory.clone(),
                        memory_offset: m.address_to_offset(address),
                    },
                    #[cfg(feature = "alternate_anon_allocs")]
                    MappingBacking::PrivateAnonymous => VmsplicePayloadSegment {
                        addr_offset: address,
                        length,
                        memory: self.private_anonymous.backing.clone(),
                        memory_offset: address.ptr() as u64,
                    },
                };
                vmsplice_mappings.push(VmsplicePayload::new(Arc::downgrade(mm), vmsplice_payload));

                address += length;
            }
        }

        Ok(vmsplice_mappings)
    }

    /// Returns all the mappings starting at `addr`, and continuing until either `length` bytes have
    /// been covered or an unmapped page is reached.
    ///
    /// Mappings are returned in ascending order along with the number of bytes that intersect the
    /// requested range. The returned mappings are guaranteed to be contiguous and the total length
    /// corresponds to the number of contiguous mapped bytes starting from `addr`, i.e.:
    /// - 0 (empty iterator) if `addr` is not mapped.
    /// - exactly `length` if the requested range is fully mapped.
    /// - the offset of the first unmapped page (between 0 and `length`) if the requested range is
    ///   only partially mapped.
    ///
    /// Returns EFAULT if the requested range overflows or extends past the end of the vmar.
    fn get_contiguous_mappings_at(
        &self,
        addr: UserAddress,
        length: usize,
    ) -> Result<impl Iterator<Item = (&Mapping, usize)>, Errno> {
        profile_duration!("GetContiguousMappings");
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EFAULT))?;
        if end_addr > self.max_address() {
            return error!(EFAULT);
        }

        // Iterate over all contiguous mappings intersecting the requested range.
        let mut mappings = self.mappings.intersection(addr..end_addr);
        let mut prev_range_end = None;
        let mut offset = 0;
        let result = std::iter::from_fn(move || {
            profile_duration!("NextContiguousMapping");
            if offset != length {
                if let Some((range, mapping)) = mappings.next() {
                    return match prev_range_end {
                        // If this is the first mapping that we are considering, it may not actually
                        // contain `addr` at all.
                        None if range.start > addr => None,

                        // Subsequent mappings may not be contiguous.
                        Some(prev_range_end) if range.start != prev_range_end => None,

                        // This mapping can be returned.
                        _ => {
                            let mapping_length = std::cmp::min(length, range.end - addr) - offset;
                            offset += mapping_length;
                            prev_range_end = Some(range.end);
                            Some((mapping, mapping_length))
                        }
                    };
                }
            }

            None
        });

        Ok(result)
    }

    /// Finds the next mapping at or above the given address if that mapping has the
    /// MappingFlags::GROWSDOWN flag.
    ///
    /// If such a mapping exists, this function returns the address at which that mapping starts
    /// and the mapping itself.
    fn next_mapping_if_growsdown(&self, addr: UserAddress) -> Option<(UserAddress, &Mapping)> {
        match self.mappings.iter_starting_at(&addr).next() {
            Some((range, mapping)) => {
                if range.contains(&addr) {
                    // |addr| is already contained within a mapping, nothing to grow.
                    None
                } else if !mapping.flags.contains(MappingFlags::GROWSDOWN) {
                    None
                } else {
                    Some((range.start, mapping))
                }
            }
            None => None,
        }
    }

    /// Determines if an access at a given address could be covered by extending a growsdown mapping and
    /// extends it if possible. Returns true if the given address is covered by a mapping.
    pub fn extend_growsdown_mapping_to_address(
        &mut self,
        addr: UserAddress,
        is_write: bool,
    ) -> Result<bool, Error> {
        profile_duration!("ExtendGrowsDown");
        let Some((mapping_low_addr, mapping_to_grow)) = self.next_mapping_if_growsdown(addr) else {
            return Ok(false);
        };
        if is_write && !mapping_to_grow.can_write() {
            // Don't grow a read-only GROWSDOWN mapping for a write fault, it won't work.
            return Ok(false);
        }
        let low_addr = addr - (addr.ptr() as u64 % *PAGE_SIZE);
        let high_addr = mapping_low_addr;
        let length = high_addr
            .ptr()
            .checked_sub(low_addr.ptr())
            .ok_or_else(|| anyhow!("Invalid growth range"))?;
        let memory =
            Arc::new(MemoryObject::from(zx::Vmo::create(length as u64).map_err(|s| match s {
                zx::Status::NO_MEMORY | zx::Status::OUT_OF_RANGE => {
                    anyhow!("Could not allocate VMO for mapping growth")
                }
                _ => anyhow!("Unexpected error creating VMO: {s}"),
            })?));
        let vmar_flags =
            mapping_to_grow.flags.access_flags().to_vmar_flags() | zx::VmarFlags::SPECIFIC;
        let mapping = Mapping::new(
            low_addr,
            memory.clone(),
            0,
            mapping_to_grow.flags,
            mapping_to_grow.max_access,
            FileWriteGuardRef(None),
        );
        let vmar_offset = self
            .user_address_to_vmar_offset(low_addr)
            .map_err(|_| anyhow!("Address outside of user range"))?;
        let mapped_address = memory
            .map_in_vmar(&self.user_vmar, vmar_offset, 0, length, vmar_flags)
            .map_err(MemoryManager::get_errno_for_map_err)?;
        if mapped_address != low_addr.ptr() {
            return Err(anyhow!("Could not map extension of mapping to desired location."));
        }
        self.mappings.insert(low_addr..high_addr, mapping);
        Ok(true)
    }

    /// Reads exactly `bytes.len()` bytes of memory.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            self.read_mapping_memory(
                addr + bytes_read,
                mapping,
                &mut bytes[bytes_read..next_offset],
            )?;
            bytes_read = next_offset;
        }

        if bytes_read != bytes.len() {
            error!(EFAULT)
        } else {
            // SAFETY: The created slice is properly aligned/sized since it
            // is a subset of the `bytes` slice. Note that `MaybeUninit<T>` has
            // the same layout as `T`. Also note that `bytes_read` bytes have
            // been properly initialized.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut u8, bytes_read)
            };
            Ok(bytes)
        }
    }

    /// Reads exactly `bytes.len()` bytes of memory from `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    fn read_mapping_memory<'a>(
        &self,
        addr: UserAddress,
        mapping: &Mapping,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        profile_duration!("MappingReadMemory");
        if !mapping.can_read() {
            return error!(EFAULT);
        }
        match &mapping.backing {
            MappingBacking::Memory(backing) => backing.read_memory(addr, bytes),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => self.private_anonymous.read_memory(addr, bytes),
        }
    }

    /// Reads bytes starting at `addr`, continuing until either `bytes.len()` bytes have been read
    /// or no more bytes can be read.
    ///
    /// This is used, for example, to read null-terminated strings where the exact length is not
    /// known, only the maximum length is.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            if self
                .read_mapping_memory(
                    addr + bytes_read,
                    mapping,
                    &mut bytes[bytes_read..next_offset],
                )
                .is_err()
            {
                break;
            }
            bytes_read = next_offset;
        }

        // If at least one byte was requested but we got none, it means that `addr` was invalid.
        if !bytes.is_empty() && bytes_read == 0 {
            error!(EFAULT)
        } else {
            // SAFETY: The created slice is properly aligned/sized since it
            // is a subset of the `bytes` slice. Note that `MaybeUninit<T>` has
            // the same layout as `T`. Also note that `bytes_read` bytes have
            // been properly initialized.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut u8, bytes_read)
            };
            Ok(bytes)
        }
    }

    /// Like `read_memory_partial` but only returns the bytes up to and including
    /// a null (zero) byte.
    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        let read_bytes = self.read_memory_partial(addr, bytes)?;
        let max_len = memchr::memchr(b'\0', read_bytes)
            .map_or_else(|| read_bytes.len(), |null_index| null_index + 1);
        Ok(&mut read_bytes[..max_len])
    }

    /// Writes the provided bytes.
    ///
    /// In case of success, the number of bytes written will always be `bytes.len()`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write.
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        profile_duration!("WriteMemory");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            self.write_mapping_memory(
                addr + bytes_written,
                mapping,
                &bytes[bytes_written..next_offset],
            )?;
            bytes_written = next_offset;
        }

        if bytes_written != bytes.len() {
            error!(EFAULT)
        } else {
            Ok(bytes.len())
        }
    }

    /// Writes the provided bytes to `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write to the memory object.
    fn write_mapping_memory(
        &self,
        addr: UserAddress,
        mapping: &Mapping,
        bytes: &[u8],
    ) -> Result<(), Errno> {
        profile_duration!("MappingWriteMemory");
        if !mapping.can_write() {
            return error!(EFAULT);
        }
        match &mapping.backing {
            MappingBacking::Memory(backing) => backing.write_memory(addr, bytes),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => self.private_anonymous.write_memory(addr, bytes),
        }
    }

    /// Writes bytes starting at `addr`, continuing until either `bytes.len()` bytes have been
    /// written or no more bytes can be written.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to write from.
    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        profile_duration!("WriteMemoryPartial");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            if self
                .write_mapping_memory(
                    addr + bytes_written,
                    mapping,
                    &bytes[bytes_written..next_offset],
                )
                .is_err()
            {
                break;
            }
            bytes_written = next_offset;
        }

        if !bytes.is_empty() && bytes_written == 0 {
            error!(EFAULT)
        } else {
            Ok(bytes.len())
        }
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        profile_duration!("Zero");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, length)? {
            let next_offset = bytes_written + len;
            if self.zero_mapping(addr + bytes_written, mapping, len).is_err() {
                break;
            }
            bytes_written = next_offset;
        }

        if length != bytes_written {
            error!(EFAULT)
        } else {
            Ok(length)
        }
    }

    fn zero_mapping(
        &self,
        addr: UserAddress,
        mapping: &Mapping,
        length: usize,
    ) -> Result<usize, Errno> {
        profile_duration!("MappingZeroMemory");
        if !mapping.can_write() {
            return error!(EFAULT);
        }

        match &mapping.backing {
            MappingBacking::Memory(backing) => backing.zero(addr, length),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => self.private_anonymous.zero(addr, length),
        }
    }

    fn get_aio_context(&self, addr: UserAddress) -> Option<(Range<UserAddress>, Arc<AioContext>)> {
        let Some((range, mapping)) = self.mappings.get(&addr) else {
            return None;
        };
        let MappingName::AioContext(ref aio_context) = mapping.name else {
            return None;
        };
        if !mapping.can_read() {
            return None;
        }
        Some((range.clone(), aio_context.clone()))
    }
}

fn create_user_vmar(vmar: &zx::Vmar, vmar_info: &zx::VmarInfo) -> Result<zx::Vmar, zx::Status> {
    let (vmar, ptr) = vmar.allocate(
        0,
        vmar_info.len,
        zx::VmarFlags::SPECIFIC
            | zx::VmarFlags::CAN_MAP_SPECIFIC
            | zx::VmarFlags::CAN_MAP_READ
            | zx::VmarFlags::CAN_MAP_WRITE
            | zx::VmarFlags::CAN_MAP_EXECUTE,
    )?;
    assert_eq!(ptr, vmar_info.base);
    Ok(vmar)
}

pub trait MemoryAccessor {
    /// Reads exactly `bytes.len()` bytes of memory from `addr` into `bytes`.
    ///
    /// In case of success, the number of bytes read will always be `bytes.len()`.
    ///
    /// Consider using `MemoryAccessorExt::read_memory_to_*` methods if you do not require control
    /// over the allocation.
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno>;

    /// Reads bytes starting at `addr`, continuing until either a null byte is read, `bytes.len()`
    /// bytes have been read or no more bytes can be read from the target.
    ///
    /// This is used, for example, to read null-terminated strings where the exact length is not
    /// known, only the maximum length is.
    ///
    /// Returns the bytes that have been read to on success.
    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno>;

    /// Reads bytes starting at `addr`, continuing until either `bytes.len()` bytes have been read
    /// or no more bytes can be read from the target.
    ///
    /// This is used, for example, to read null-terminated strings where the exact length is not
    /// known, only the maximum length is.
    ///
    /// Consider using `MemoryAccessorExt::read_memory_partial_to_*` methods if you do not require
    /// control over the allocation.
    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno>;

    /// Writes the provided bytes to `addr`.
    ///
    /// In case of success, the number of bytes written will always be `bytes.len()`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write from.
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno>;

    /// Writes bytes starting at `addr`, continuing until either `bytes.len()` bytes have been
    /// written or no more bytes can be written.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write from.
    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno>;

    /// Writes zeros starting at `addr` and continuing for `length` bytes.
    ///
    /// Returns the number of bytes that were zeroed.
    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno>;
}

pub trait TaskMemoryAccessor: MemoryAccessor {
    /// Returns the maximum valid address for this memory accessor.
    fn maximum_valid_address(&self) -> UserAddress;
}

/// A memory manager for another thread.
///
/// When accessing memory through this object, we use less efficient codepaths that work across
/// address spaces.
pub struct RemoteMemoryManager {
    mm: Arc<MemoryManager>,
}

impl RemoteMemoryManager {
    fn new(mm: Arc<MemoryManager>) -> Self {
        Self { mm }
    }
}

// If we just have a MemoryManager, we cannot assume that its address space is current, which means
// we need to use the slower "syscall" mechanism to access its memory.
impl MemoryAccessor for RemoteMemoryManager {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm.syscall_read_memory(addr, bytes)
    }

    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm.syscall_read_memory_partial_until_null_byte(addr, bytes)
    }

    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm.syscall_read_memory_partial(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.syscall_write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.syscall_write_memory_partial(addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.mm.syscall_zero(addr, length)
    }
}

impl TaskMemoryAccessor for RemoteMemoryManager {
    fn maximum_valid_address(&self) -> UserAddress {
        self.mm.maximum_valid_user_address
    }
}

// TODO(https://fxbug.dev/42079727): replace this with MaybeUninit::as_bytes_mut.
#[inline]
fn object_as_mut_bytes<T: FromBytes + Sized>(
    object: &mut MaybeUninit<T>,
) -> &mut [MaybeUninit<u8>] {
    // SAFETY: T is FromBytes, which means that any bit pattern is valid. Interpreting
    // MaybeUninit<T> as [MaybeUninit<u8>] is safe because T's alignment requirements
    // are larger than u8.
    unsafe {
        std::slice::from_raw_parts_mut(
            object.as_mut_ptr() as *mut MaybeUninit<u8>,
            std::mem::size_of::<T>(),
        )
    }
}

// TODO(https://fxbug.dev/42079727): replace this with MaybeUninit::slice_as_bytes_mut.
#[inline]
fn slice_as_mut_bytes<T: FromBytes + Sized>(
    slice: &mut [MaybeUninit<T>],
) -> &mut [MaybeUninit<u8>] {
    // SAFETY: T is FromBytes, which means that any bit pattern is valid. Interpreting T as u8
    // is safe because T's alignment requirements are larger than u8.
    unsafe {
        std::slice::from_raw_parts_mut(
            slice.as_mut_ptr() as *mut MaybeUninit<u8>,
            slice.len() * std::mem::size_of::<T>(),
        )
    }
}

/// Holds the number of _elements_ read by the callback to [`read_to_vec`].
///
/// Used to make it clear to callers that the callback should return the number
/// of elements read and not the number of bytes read.
pub struct NumberOfElementsRead(pub usize);

/// Performs a read into a `Vec` using the provided read function.
///
/// The read function returns the number of elements of type `T` read.
///
/// # Safety
///
/// The read function must only return `Ok(n)` if at least one element was read and `n` holds
/// the number of elements of type `T` read starting from the beginning of the slice.
#[inline]
pub unsafe fn read_to_vec<T: FromBytes, E>(
    max_len: usize,
    read_fn: impl FnOnce(&mut [MaybeUninit<T>]) -> Result<NumberOfElementsRead, E>,
) -> Result<Vec<T>, E> {
    let mut buffer = Vec::with_capacity(max_len);
    // We can't just pass `spare_capacity_mut` because `Vec::with_capacity`
    // returns a `Vec` with _at least_ the requested capacity.
    let NumberOfElementsRead(read_elements) = read_fn(&mut buffer.spare_capacity_mut()[..max_len])?;
    debug_assert!(read_elements <= max_len, "read_elements={read_elements}, max_len={max_len}");
    // SAFETY: The new length is equal to the number of elements successfully
    // initialized (since `read_fn` returned successfully).
    unsafe { buffer.set_len(read_elements) }
    Ok(buffer)
}

/// Performs a read into an array using the provided read function.
///
/// The read function returns `Ok(())` if the buffer was fully read to.
///
/// # Safety
///
/// The read function must only return `Ok(())` if all the bytes were read to.
#[inline]
pub unsafe fn read_to_array<T: FromBytes, E, const N: usize>(
    read_fn: impl FnOnce(&mut [MaybeUninit<T>]) -> Result<(), E>,
) -> Result<[T; N], E> {
    // TODO(https://fxbug.dev/129314): replace with MaybeUninit::uninit_array.
    let buffer: MaybeUninit<[MaybeUninit<T>; N]> = MaybeUninit::uninit();
    // SAFETY: We are converting from an uninitialized array to an array
    // of uninitialized elements which is the same. See
    // https://doc.rust-lang.org/std/mem/union.MaybeUninit.html#initializing-an-array-element-by-element.
    let mut buffer = unsafe { buffer.assume_init() };
    read_fn(&mut buffer)?;
    // SAFETY: This is safe because we have initialized all the elements in
    // the array (since `read_fn` returned successfully).
    //
    // TODO(https://fxbug.deb/129309): replace with MaybeUninit::array_assume_init.
    let buffer = buffer.map(|a| unsafe { a.assume_init() });
    Ok(buffer)
}

/// Performs a read into an object using the provided read function.
///
/// The read function returns `Ok(())` if the buffer was fully read to.
///
/// # Safety
///
/// The read function must only return `Ok(())` if all the bytes were read to.
#[inline]
pub unsafe fn read_to_object_as_bytes<T: FromBytes, E>(
    read_fn: impl FnOnce(&mut [MaybeUninit<u8>]) -> Result<(), E>,
) -> Result<T, E> {
    let mut object = MaybeUninit::uninit();
    read_fn(object_as_mut_bytes(&mut object))?;
    // SAFETY: The call to `read_fn` succeeded so we know that `object`
    // has been initialized.
    let object = unsafe { object.assume_init() };
    Ok(object)
}

pub trait MemoryAccessorExt: MemoryAccessor {
    /// Reads exactly `bytes.len()` bytes of memory from `addr` into `bytes`.
    ///
    /// In case of success, the number of bytes read will always be `bytes.len()`.
    ///
    /// Consider using `MemoryAccessorExt::read_memory_to_*` methods if you do not require control
    /// over the allocation.
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        let bytes_len = bytes.len();
        self.read_memory(addr, slice_to_maybe_uninit_mut(bytes))
            .map(|bytes_read| debug_assert_eq!(bytes_read.len(), bytes_len))
    }

    /// Read exactly `len` bytes of memory, returning them as a a Vec.
    fn read_memory_to_vec(&self, addr: UserAddress, len: usize) -> Result<Vec<u8>, Errno> {
        // SAFETY: `self.read_memory` only returns `Ok` if all bytes were read to.
        unsafe {
            read_to_vec::<u8, _>(len, |buf| {
                self.read_memory(addr, buf).map(|bytes_read| {
                    debug_assert_eq!(bytes_read.len(), len);
                    NumberOfElementsRead(len)
                })
            })
        }
    }

    /// Read up to `max_len` bytes from `addr`, returning them as a Vec.
    fn read_memory_partial_to_vec(
        &self,
        addr: UserAddress,
        max_len: usize,
    ) -> Result<Vec<u8>, Errno> {
        // SAFETY: `self.read_memory_partial` returns the bytes read.
        unsafe {
            read_to_vec::<u8, _>(max_len, |buf| {
                self.read_memory_partial(addr, buf)
                    .map(|bytes_read| NumberOfElementsRead(bytes_read.len()))
            })
        }
    }

    /// Read exactly `N` bytes from `addr`, returning them as an array.
    fn read_memory_to_array<const N: usize>(&self, addr: UserAddress) -> Result<[u8; N], Errno> {
        // SAFETY: `self.read_memory` only returns `Ok` if all bytes were read to.
        unsafe {
            read_to_array(|buf| {
                self.read_memory(addr, buf).map(|bytes_read| debug_assert_eq!(bytes_read.len(), N))
            })
        }
    }

    /// Read the contents of `buffer`, returning them as a Vec.
    fn read_buffer(&self, buffer: &UserBuffer) -> Result<Vec<u8>, Errno> {
        self.read_memory_to_vec(buffer.address, buffer.length)
    }

    /// Read an instance of T from `user`.
    fn read_object<T: FromBytes>(&self, user: UserRef<T>) -> Result<T, Errno> {
        // SAFETY: `self.read_memory` only returns `Ok` if all bytes were read to.
        unsafe {
            read_to_object_as_bytes(|buf| {
                self.read_memory(user.addr(), buf)
                    .map(|bytes_read| debug_assert_eq!(bytes_read.len(), std::mem::size_of::<T>()))
            })
        }
    }

    /// Reads the first `partial` bytes of an object, leaving any remainder 0-filled.
    ///
    /// This is used for reading size-versioned structures where the user can specify an older
    /// version of the structure with a smaller size.
    ///
    /// Returns EINVAL if the input size is larger than the object (assuming the input size is from
    /// the user who has specified something we don't support).
    fn read_object_partial<T: FromBytes>(
        &self,
        user: UserRef<T>,
        partial_size: usize,
    ) -> Result<T, Errno> {
        let full_size = std::mem::size_of::<T>();
        if partial_size > full_size {
            return error!(EINVAL);
        }

        // This implementation involves an extra memcpy compared to read_object but avoids unsafe
        // code. This isn't currently called very often.
        let mut object = MaybeUninit::uninit();
        let (to_read, to_zero) = object_as_mut_bytes(&mut object).split_at_mut(partial_size);
        self.read_memory(user.addr(), to_read)?;

        // Zero pad out to the correct size.
        to_zero.fill(MaybeUninit::new(0));

        // SAFETY: `T` implements `FromBytes` so any bit pattern is valid and all
        // bytes of `object` have been initialized.
        Ok(unsafe { object.assume_init() })
    }

    /// Read exactly `objects.len()` objects into `objects` from `user`.
    fn read_objects<'a, T: FromBytes>(
        &self,
        user: UserRef<T>,
        objects: &'a mut [MaybeUninit<T>],
    ) -> Result<&'a mut [T], Errno> {
        let objects_len = objects.len();
        self.read_memory(user.addr(), slice_as_mut_bytes(objects)).map(|bytes_read| {
            debug_assert_eq!(bytes_read.len(), objects_len * std::mem::size_of::<T>());
            // SAFETY: `T` implements `FromBytes` and all bytes have been initialized.
            unsafe {
                std::slice::from_raw_parts_mut(bytes_read.as_mut_ptr() as *mut T, objects_len)
            }
        })
    }

    /// Read exactly `objects.len()` objects into `objects` from `user`.
    fn read_objects_to_slice<T: FromBytes>(
        &self,
        user: UserRef<T>,
        objects: &mut [T],
    ) -> Result<(), Errno> {
        let objects_len = objects.len();
        self.read_objects(user, slice_to_maybe_uninit_mut(objects))
            .map(|objects_read| debug_assert_eq!(objects_read.len(), objects_len))
    }

    /// Read exactly `len` objects from `user`, returning them as a Vec.
    fn read_objects_to_vec<T: Clone + FromBytes>(
        &self,
        user: UserRef<T>,
        len: usize,
    ) -> Result<Vec<T>, Errno> {
        // SAFETY: `self.read_objects` only returns `Ok` if all bytes were read to.
        unsafe {
            read_to_vec::<T, _>(len, |buf| {
                self.read_objects(user, buf).map(|objects_read| {
                    debug_assert_eq!(objects_read.len(), len);
                    NumberOfElementsRead(len)
                })
            })
        }
    }

    /// Read exactly `len` objects from `user`, returning them as a SmallVec.
    fn read_objects_to_smallvec<T: Clone + FromBytes, const N: usize>(
        &self,
        user: UserRef<T>,
        len: usize,
    ) -> Result<SmallVec<[T; N]>, Errno> {
        if len > N {
            Ok(SmallVec::<[T; N]>::from_vec(self.read_objects_to_vec(user, len)?))
        } else {
            // TODO(https://github.com/rust-lang/rust/issues/96097) use MaybeUninit::uninit_array
            // SAFETY: We are converting from an uninitialized array to an array of uninitialized
            // elements which is the same. See
            // https://doc.rust-lang.org/std/mem/union.MaybeUninit.html#initializing-an-array-element-by-element.
            let mut buffer: [MaybeUninit<T>; N] = unsafe { MaybeUninit::uninit().assume_init() };

            self.read_objects(user, &mut buffer[..len])?;

            // TODO(https://github.com/rust-lang/rust/issues/96097) use MaybeUninit::transpose
            // SAFETY: MaybeUninit<[T; N]> and [MaybeUninit<T>; N] have the same layout.
            let buffer: MaybeUninit<[T; N]> = unsafe { std::mem::transmute_copy(&buffer) };

            // SAFETY: `read_objects` guarantees that the first `len` entries are initialized.
            Ok(unsafe { SmallVec::from_buf_and_len_unchecked(buffer, len) })
        }
    }

    /// Read exactly `N` objects from `user`, returning them as an array.
    fn read_objects_to_array<T: Copy + FromBytes, const N: usize>(
        &self,
        user: UserRef<T>,
    ) -> Result<[T; N], Errno> {
        // SAFETY: `self.read_objects` only returns `Ok` if all bytes were read to.
        unsafe {
            read_to_array(|buf| {
                self.read_objects(user, buf).map(|objects_read| {
                    debug_assert_eq!(objects_read.len(), N);
                })
            })
        }
    }

    /// Read exactly `iovec_count` `UserBuffer`s from `iovec_addr`.
    ///
    /// Fails if `iovec_count` is greater than `UIO_MAXIOV`.
    fn read_iovec(
        &self,
        iovec_addr: UserAddress,
        iovec_count: UserValue<i32>,
    ) -> Result<UserBuffers, Errno> {
        let iovec_count: usize = iovec_count.try_into().map_err(|_| errno!(EINVAL))?;
        if iovec_count > UIO_MAXIOV as usize {
            return error!(EINVAL);
        }

        self.read_objects_to_smallvec(iovec_addr.into(), iovec_count)
    }

    /// Read exactly `iovec_count` `UserBuffer`s from `iovec_addr`.
    ///
    /// Fails if `iovec_count` is greater than `UIO_MAXIOV`.
    // TODO(https://fxbug.dev/380427162) Should this be rolled into the
    // above call with just a 32bit flag?
    fn read_iovec32(
        &self,
        iovec_addr: UserAddress,
        iovec_count: UserValue<i32>,
    ) -> Result<UserBuffers, Errno> {
        let iovec_count: usize = iovec_count.try_into().map_err(|_| errno!(EINVAL))?;
        if iovec_count > UIO_MAXIOV as usize {
            return error!(EINVAL);
        }
        let ub32s: UserBuffers32 = self.read_objects_to_smallvec(iovec_addr.into(), iovec_count)?;
        Ok(ub32s.iter().map(|&ub32| ub32.into()).collect())
    }

    /// Read up to `max_size` bytes from `string`, stopping at the first discovered null byte and
    /// returning the results as a Vec.
    fn read_c_string_to_vec(
        &self,
        string: UserCString,
        max_size: usize,
    ) -> Result<FsString, Errno> {
        let chunk_size = std::cmp::min(*PAGE_SIZE as usize, max_size);

        let mut buf = Vec::with_capacity(chunk_size);
        let mut index = 0;
        loop {
            // This operation should never overflow: we should fail to read before that.
            let addr = string.addr().checked_add(index).ok_or_else(|| errno!(EFAULT))?;
            let read = self.read_memory_partial_until_null_byte(
                addr,
                &mut buf.spare_capacity_mut()[index..][..chunk_size],
            )?;
            let read_len = read.len();

            // Check if the last byte read is the null byte.
            if read.last() == Some(&0) {
                let null_index = index + read_len - 1;
                // SAFETY: Bytes until `null_index` have been initialized.
                unsafe { buf.set_len(null_index) }
                if buf.len() > max_size {
                    return error!(ENAMETOOLONG);
                }

                return Ok(buf.into());
            }
            index += read_len;

            if read_len < chunk_size || index >= max_size {
                // There's no more for us to read.
                return error!(ENAMETOOLONG);
            }

            // Trigger a capacity increase.
            buf.reserve(index + chunk_size);
        }
    }

    /// Read `len` bytes from `start` and parse the region as null-delimited CStrings, for example
    /// how `argv` is stored.
    ///
    /// There can be an arbitrary number of null bytes in between `start` and `end`.
    fn read_nul_delimited_c_string_list(
        &self,
        start: UserAddress,
        len: usize,
    ) -> Result<Vec<FsString>, Errno> {
        let buf = self.read_memory_to_vec(start, len)?;
        let mut buf = &buf[..];

        let mut list = vec![];
        while !buf.is_empty() {
            let len_consumed = match CStr::from_bytes_until_nul(buf) {
                Ok(segment) => {
                    // Return the string without the null to match our other APIs, but advance the
                    // "cursor" of the buf variable past the null byte.
                    list.push(segment.to_bytes().into());
                    segment.to_bytes_with_nul().len()
                }
                Err(_) => {
                    // If we didn't find a null byte, then the whole rest of the buffer is the
                    // last string.
                    list.push(buf.into());
                    buf.len()
                }
            };
            buf = &buf[len_consumed..];
        }

        Ok(list)
    }

    /// Read up to `buffer.len()` bytes from `string`, stopping at the first discovered null byte
    /// and returning the result as a slice that ends before that null.
    ///
    /// Consider using `read_c_string_to_vec` if you do not require control over the allocation.
    fn read_c_string<'a>(
        &self,
        string: UserCString,
        buffer: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a FsStr, Errno> {
        let buffer = self.read_memory_partial_until_null_byte(string.addr(), buffer)?;
        // Make sure the last element holds the null byte.
        if let Some((null_byte, buffer)) = buffer.split_last() {
            if null_byte == &0 {
                return Ok(buffer.into());
            }
        }

        error!(ENAMETOOLONG)
    }

    fn write_object<T: IntoBytes + Immutable>(
        &self,
        user: UserRef<T>,
        object: &T,
    ) -> Result<usize, Errno> {
        self.write_memory(user.addr(), object.as_bytes())
    }

    fn write_objects<T: IntoBytes + Immutable>(
        &self,
        user: UserRef<T>,
        objects: &[T],
    ) -> Result<usize, Errno> {
        self.write_memory(user.addr(), objects.as_bytes())
    }
}

impl MemoryManager {
    pub fn get_mappings_for_vmsplice(
        self: &Arc<MemoryManager>,
        buffers: &UserBuffers,
    ) -> Result<Vec<Arc<VmsplicePayload>>, Errno> {
        self.state.read().get_mappings_for_vmsplice(self, buffers)
    }

    pub fn has_same_address_space(&self, other: &Self) -> bool {
        self.root_vmar == other.root_vmar
    }

    pub fn unified_read_memory<'a>(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyRead");
            let (read_bytes, unread_bytes) = usercopy.copyin(addr.ptr(), bytes);
            if unread_bytes.is_empty() {
                Ok(read_bytes)
            } else {
                error!(EFAULT)
            }
        } else {
            self.syscall_read_memory(addr, bytes)
        }
    }

    pub fn syscall_read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.state.read().read_memory(addr, bytes)
    }

    pub fn unified_read_memory_partial_until_null_byte<'a>(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyReadPartialUntilNull");
            let (read_bytes, unread_bytes) = usercopy.copyin_until_null_byte(addr.ptr(), bytes);
            if read_bytes.is_empty() && !unread_bytes.is_empty() {
                error!(EFAULT)
            } else {
                Ok(read_bytes)
            }
        } else {
            self.syscall_read_memory_partial_until_null_byte(addr, bytes)
        }
    }

    pub fn syscall_read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.state.read().read_memory_partial_until_null_byte(addr, bytes)
    }

    pub fn unified_read_memory_partial<'a>(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyReadPartial");
            let (read_bytes, unread_bytes) = usercopy.copyin(addr.ptr(), bytes);
            if read_bytes.is_empty() && !unread_bytes.is_empty() {
                error!(EFAULT)
            } else {
                Ok(read_bytes)
            }
        } else {
            self.syscall_read_memory_partial(addr, bytes)
        }
    }

    pub fn syscall_read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.state.read().read_memory_partial(addr, bytes)
    }

    pub fn unified_write_memory(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        bytes: &[u8],
    ) -> Result<usize, Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyWrite");
            let num_copied = usercopy.copyout(bytes, addr.ptr());
            if num_copied != bytes.len() {
                error!(
                    EFAULT,
                    format!("expected {:?} bytes, copied {:?} bytes", bytes.len(), num_copied)
                )
            } else {
                Ok(num_copied)
            }
        } else {
            self.syscall_write_memory(addr, bytes)
        }
    }

    pub fn syscall_write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.state.read().write_memory(addr, bytes)
    }

    pub fn unified_write_memory_partial(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        bytes: &[u8],
    ) -> Result<usize, Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyWritePartial");
            let num_copied = usercopy.copyout(bytes, addr.ptr());
            if num_copied == 0 && !bytes.is_empty() {
                error!(EFAULT)
            } else {
                Ok(num_copied)
            }
        } else {
            self.syscall_write_memory_partial(addr, bytes)
        }
    }

    pub fn syscall_write_memory_partial(
        &self,
        addr: UserAddress,
        bytes: &[u8],
    ) -> Result<usize, Errno> {
        self.state.read().write_memory_partial(addr, bytes)
    }

    pub fn unified_zero(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        length: usize,
    ) -> Result<usize, Errno> {
        debug_assert!(self.has_same_address_space(current_task.mm()));

        {
            let page_size = *PAGE_SIZE as usize;
            // Get the page boundary immediately following `addr` if `addr` is
            // not page aligned.
            let next_page_boundary = round_up_to_system_page_size(addr.ptr())?;
            // The number of bytes needed to zero at least a full page (not just
            // a pages worth of bytes) starting at `addr`.
            let length_with_atleast_one_full_page = page_size + (next_page_boundary - addr.ptr());
            // If at least one full page is being zeroed, go through the memory object since Zircon
            // can swap the mapped pages with the zero page which should be cheaper than zeroing
            // out a pages worth of bytes manually.
            //
            // If we are not zeroing out a full page, then go through usercopy
            // if unified aspaces is enabled.
            if length >= length_with_atleast_one_full_page {
                return self.syscall_zero(addr, length);
            }
        }

        if let Some(usercopy) = usercopy() {
            profile_duration!("UsercopyZero");
            if usercopy.zero(addr.ptr(), length) == length {
                Ok(length)
            } else {
                error!(EFAULT)
            }
        } else {
            self.syscall_zero(addr, length)
        }
    }

    pub fn syscall_zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.state.read().zero(addr, length)
    }

    /// Obtain a reference to this memory manager that can be used from another thread.
    pub fn as_remote(self: &Arc<Self>) -> RemoteMemoryManager {
        RemoteMemoryManager::new(self.clone())
    }
}

impl MemoryAccessorExt for dyn MemoryAccessor + '_ {}
impl<T: MemoryAccessor> MemoryAccessorExt for T {}

pub struct MemoryManager {
    /// The root VMAR for the child process.
    ///
    /// Instead of mapping memory directly in this VMAR, we map the memory in
    /// `state.user_vmar`.
    root_vmar: zx::Vmar,

    /// The base address of the root_vmar.
    pub base_addr: UserAddress,

    /// The futexes in this address space.
    pub futex: FutexTable<PrivateFutexKey>,

    /// Mutable state for the memory manager.
    pub state: RwLock<MemoryManagerState>,

    /// Whether this address space is dumpable.
    pub dumpable: OrderedMutex<DumpPolicy, MmDumpable>,

    /// Maximum valid user address for this vmar.
    pub maximum_valid_user_address: UserAddress,

    /// In-flight payloads enqueued to a pipe as a consequence of a `vmsplice(2)`
    /// operation.
    ///
    /// For details on why we need to keep track of in-flight vmspliced payloads,
    /// see [`VmsplicePayload`].
    ///
    /// For details on why this isn't under the `RwLock` protected `MemoryManagerState`,
    /// See [`InflightVmsplicedPayloads::payloads`].
    pub inflight_vmspliced_payloads: InflightVmsplicedPayloads,
}

impl MemoryManager {
    pub fn new(root_vmar: zx::Vmar) -> Result<Self, zx::Status> {
        let info = root_vmar.info()?;
        let user_vmar = create_user_vmar(&root_vmar, &info)?;
        let user_vmar_info = user_vmar.info()?;

        debug_assert_eq!(RESTRICTED_ASPACE_BASE, user_vmar_info.base);
        debug_assert_eq!(RESTRICTED_ASPACE_SIZE, user_vmar_info.len);

        Ok(Self::from_vmar(root_vmar, user_vmar, user_vmar_info))
    }

    pub fn new_empty() -> Self {
        let root_vmar = zx::Vmar::from(zx::Handle::invalid());
        let user_vmar = zx::Vmar::from(zx::Handle::invalid());
        Self::from_vmar(root_vmar, user_vmar, Default::default())
    }

    fn from_vmar(root_vmar: zx::Vmar, user_vmar: zx::Vmar, user_vmar_info: zx::VmarInfo) -> Self {
        #[cfg(feature = "alternate_anon_allocs")]
        // The private anonymous backing memory object extend from the user address 0 up to the
        // highest mappable address. The pages below `user_vmar_info.base` are never mapped, but
        // including them in the memory object makes the math for mapping address to memory object
        // offsets simpler.
        let backing_size = (user_vmar_info.base + user_vmar_info.len) as u64;

        MemoryManager {
            root_vmar,
            base_addr: UserAddress::from_ptr(user_vmar_info.base),
            futex: FutexTable::<PrivateFutexKey>::default(),
            state: RwLock::new(MemoryManagerState {
                user_vmar,
                user_vmar_info,
                mappings: RangeMap::new(),
                #[cfg(feature = "alternate_anon_allocs")]
                private_anonymous: PrivateAnonymousMemoryManager::new(backing_size),
                forkable_state: Default::default(),
            }),
            // TODO(security): Reset to DISABLE, or the value in the fs.suid_dumpable sysctl, under
            // certain conditions as specified in the prctl(2) man page.
            dumpable: OrderedMutex::new(DumpPolicy::User),
            maximum_valid_user_address: UserAddress::from_ptr(
                user_vmar_info.base + user_vmar_info.len,
            ),
            inflight_vmspliced_payloads: Default::default(),
        }
    }

    pub fn set_brk(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        addr: UserAddress,
    ) -> Result<UserAddress, Errno> {
        let rlimit_data = std::cmp::min(
            PROGRAM_BREAK_LIMIT,
            current_task.thread_group.get_rlimit(Resource::DATA),
        );

        let mut released_mappings = vec![];
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();

        let brk = match state.brk.clone() {
            None => {
                let memory = Arc::new(MemoryObject::from(
                    zx::Vmo::create(PROGRAM_BREAK_LIMIT).map_err(|_| errno!(ENOMEM))?,
                ));
                memory.set_zx_name(b"starnix-brk");

                let brk = ProgramBreak { base: state.brk_origin, current: state.brk_origin };
                state.brk = Some(brk.clone());
                brk
            }
            Some(brk) => brk,
        };

        if addr < brk.base || addr > brk.base + rlimit_data {
            // The requested program break is out-of-range. We're supposed to simply
            // return the current program break.
            return Ok(brk.current);
        }

        let old_end = brk.current.round_up(*PAGE_SIZE).unwrap();
        let new_end = addr.round_up(*PAGE_SIZE).unwrap();

        match new_end.cmp(&old_end) {
            std::cmp::Ordering::Less => {
                // Shrinking the program break removes any mapped pages in the
                // affected range, regardless of whether they were actually program
                // break pages, or other mappings.
                let delta = old_end - new_end;

                if state.unmap(self, new_end, delta, &mut released_mappings).is_err() {
                    return Ok(brk.current);
                }
            }
            std::cmp::Ordering::Greater => {
                let range = old_end..new_end;
                let delta = new_end - old_end;

                // Check for mappings over the program break region.
                if state.mappings.intersection(&range).next().is_some() {
                    return Ok(brk.current);
                }

                // TODO(b/310255065): Call `map_anonymous()` directly once
                // `alternate_anon_allocs` is always on.
                if !Self::extend_brk(
                    &mut state,
                    self,
                    old_end,
                    delta,
                    brk.base,
                    &mut released_mappings,
                ) {
                    return Ok(brk.current);
                }
            }
            _ => {}
        };

        // Any required updates to the program break succeeded, so update internal state.
        let mut new_brk = brk;
        new_brk.current = addr;
        state.brk = Some(new_brk);
        Ok(addr)
    }

    fn extend_brk(
        state: &mut MemoryManagerState,
        mm: &Arc<MemoryManager>,
        old_end: UserAddress,
        delta: usize,
        brk_base: UserAddress,
        released_mappings: &mut Vec<Mapping>,
    ) -> bool {
        #[cfg(not(feature = "alternate_anon_allocs"))]
        {
            // If there was previously at least one page of program break then we can
            // extend that mapping, rather than making a new allocation.
            let existing = if old_end > brk_base {
                let last_page = old_end - *PAGE_SIZE;
                state.mappings.get(&last_page).filter(|(_, m)| m.name == MappingName::Heap)
            } else {
                None
            };

            if let Some((range, _)) = existing {
                let range_start = range.start;
                let old_length = range.end - range.start;
                return state
                    .try_remap_in_place(
                        mm,
                        range_start,
                        old_length,
                        old_length + delta,
                        released_mappings,
                    )
                    .unwrap_or_default()
                    .is_some();
            }
        }

        #[cfg(feature = "alternate_anon_allocs")]
        {
            let _ = brk_base;
        }

        // Otherwise, allocating fresh anonymous pages is good-enough.
        state
            .map_anonymous(
                mm,
                DesiredAddress::FixedOverwrite(old_end),
                delta,
                ProtectionFlags::READ | ProtectionFlags::WRITE,
                MappingOptions::ANONYMOUS,
                MappingName::Heap,
                released_mappings,
            )
            .is_ok()
    }

    pub fn snapshot_to<L>(
        &self,
        locked: &mut Locked<'_, L>,
        target: &Arc<MemoryManager>,
    ) -> Result<(), Errno>
    where
        L: LockBefore<MmDumpable>,
    {
        // TODO(https://fxbug.dev/42074633): When SNAPSHOT (or equivalent) is supported on pager-backed VMOs
        // we can remove the hack below (which also won't be performant). For now, as a workaround,
        // we use SNAPSHOT_AT_LEAST_ON_WRITE on both the child and the parent.

        struct MemoryInfo {
            memory: Arc<MemoryObject>,
            size: u64,

            // Indicates whether or not the memory object needs to be replaced on the parent as
            // well.
            needs_snapshot_on_parent: bool,
        }

        // Clones the `memory` and returns the `MemoryInfo` with the clone.
        fn clone_memory(
            memory: &Arc<MemoryObject>,
            rights: zx::Rights,
        ) -> Result<MemoryInfo, Errno> {
            let memory_info = memory.info()?;
            let pager_backed = memory_info.flags.contains(zx::VmoInfoFlags::PAGER_BACKED);
            Ok(if pager_backed && !rights.contains(zx::Rights::WRITE) {
                MemoryInfo {
                    memory: memory.clone(),
                    size: memory_info.size_bytes,
                    needs_snapshot_on_parent: false,
                }
            } else {
                let mut cloned_memory = memory
                    .create_child(
                        if pager_backed {
                            zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE
                        } else {
                            zx::VmoChildOptions::SNAPSHOT
                        } | zx::VmoChildOptions::RESIZABLE,
                        0,
                        memory_info.size_bytes,
                    )
                    .map_err(MemoryManager::get_errno_for_map_err)?;
                if rights.contains(zx::Rights::EXECUTE) {
                    cloned_memory = cloned_memory
                        .replace_as_executable(&VMEX_RESOURCE)
                        .map_err(impossible_error)?;
                }
                MemoryInfo {
                    memory: Arc::new(cloned_memory),
                    size: memory_info.size_bytes,
                    needs_snapshot_on_parent: pager_backed,
                }
            })
        }

        fn snapshot_memory(
            memory: &MemoryObject,
            size: u64,
            rights: zx::Rights,
        ) -> Result<Arc<MemoryObject>, Errno> {
            let mut cloned_memory = memory
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE
                        | zx::VmoChildOptions::RESIZABLE,
                    0,
                    size,
                )
                .map_err(MemoryManager::get_errno_for_map_err)?;

            if rights.contains(zx::Rights::EXECUTE) {
                cloned_memory = cloned_memory
                    .replace_as_executable(&VMEX_RESOURCE)
                    .map_err(impossible_error)?;
            }
            Ok(Arc::new(cloned_memory))
        }

        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let state: &mut MemoryManagerState = &mut self.state.write();
        let mut target_state = target.state.write();
        let mut child_memorys = HashMap::<zx::Koid, MemoryInfo>::new();
        let mut replaced_memorys = HashMap::<zx::Koid, Arc<MemoryObject>>::new();

        #[cfg(feature = "alternate_anon_allocs")]
        {
            let backing_size = (state.user_vmar_info.base + state.user_vmar_info.len) as u64;
            target_state.private_anonymous = state.private_anonymous.snapshot(backing_size)?;
        }

        for (range, mapping) in state.mappings.iter_mut() {
            if mapping.flags.contains(MappingFlags::DONTFORK) {
                continue;
            }
            match &mut mapping.backing {
                MappingBacking::Memory(backing) => {
                    let memory_offset = backing.memory_offset + (range.start - backing.base) as u64;
                    let length = range.end - range.start;

                    let target_memory = if mapping.flags.contains(MappingFlags::SHARED)
                        || mapping.name == MappingName::Vvar
                    {
                        // Note that the Vvar is a special mapping that behaves like a shared mapping but
                        // is private to each process.
                        backing.memory.clone()
                    } else if mapping.flags.contains(MappingFlags::WIPEONFORK) {
                        create_anonymous_mapping_memory(length as u64)?
                    } else {
                        let basic_info = backing.memory.basic_info();

                        let MemoryInfo { memory, size: memory_size, needs_snapshot_on_parent } =
                            child_memorys
                                .entry(basic_info.koid)
                                .or_insert(clone_memory(&backing.memory, basic_info.rights)?);

                        if *needs_snapshot_on_parent {
                            let replaced_memory =
                                replaced_memorys.entry(basic_info.koid).or_insert(snapshot_memory(
                                    &backing.memory,
                                    *memory_size,
                                    basic_info.rights,
                                )?);
                            map_in_vmar(
                                &state.user_vmar,
                                &state.user_vmar_info,
                                SelectedAddress::FixedOverwrite(range.start),
                                replaced_memory,
                                memory_offset,
                                length,
                                mapping.flags,
                                false,
                            )?;

                            backing.memory = replaced_memory.clone();
                        }
                        memory.clone()
                    };

                    let mut released_mappings = vec![];
                    target_state.map_memory(
                        target,
                        DesiredAddress::Fixed(range.start),
                        target_memory,
                        memory_offset,
                        length,
                        mapping.flags,
                        mapping.max_access,
                        false,
                        mapping.name.clone(),
                        FileWriteGuardRef(None),
                        &mut released_mappings,
                    )?;
                    assert!(released_mappings.is_empty());
                }
                #[cfg(feature = "alternate_anon_allocs")]
                MappingBacking::PrivateAnonymous => {
                    let length = range.end - range.start;
                    if mapping.flags.contains(MappingFlags::WIPEONFORK) {
                        target_state
                            .private_anonymous
                            .zero(range.start, length)
                            .map_err(|_| errno!(ENOMEM))?;
                    }

                    let target_memory_offset = range.start.ptr() as u64;
                    target_state.map_in_user_vmar(
                        SelectedAddress::FixedOverwrite(range.start),
                        &target_state.private_anonymous.backing,
                        target_memory_offset,
                        length,
                        mapping.flags,
                        false,
                    )?;
                    target_state.mappings.insert(
                        range.clone(),
                        Mapping::new_private_anonymous(mapping.flags, mapping.name.clone()),
                    );
                }
            };
        }

        target_state.forkable_state = state.forkable_state.clone();

        let self_dumpable = *self.dumpable.lock(locked);
        *target.dumpable.lock(locked) = self_dumpable;

        Ok(())
    }

    pub fn exec(&self, exe_node: NamespaceNode, arch_width: ArchWidth) -> Result<(), zx::Status> {
        // The previous mapping should be dropped only after the lock to state is released to
        // prevent lock order inversion.
        let _old_mappings = {
            let mut state = self.state.write();
            let mut info = self.root_vmar.info()?;
            // SAFETY: This operation is safe because the VMAR is for another process.
            unsafe { state.user_vmar.destroy()? }
            if arch_width.is_arch32() {
                info.len = (LOWER_4GB_LIMIT.ptr() - info.base) as usize;
            } else {
                info.len = RESTRICTED_ASPACE_HIGHEST_ADDRESS - info.base;
            }
            state.user_vmar = create_user_vmar(&self.root_vmar, &info)?;
            state.user_vmar_info = state.user_vmar.info()?;
            state.brk = None;
            state.executable_node = Some(exe_node);

            std::mem::replace(&mut state.mappings, RangeMap::new())
        };
        self.initialize_mmap_layout(arch_width)?;
        Ok(())
    }

    pub fn initialize_mmap_layout(&self, arch_width: ArchWidth) -> Result<(), Errno> {
        let mut state = self.state.write();

        // Place the stack at the end of the address space, subject to ASLR adjustment.
        state.stack_origin = UserAddress::from_ptr(
            state.user_vmar_info.base + state.user_vmar_info.len
                - MAX_STACK_SIZE
                - generate_random_offset_for_aslr(arch_width),
        )
        .round_up(*PAGE_SIZE)?;

        // Set the highest address that `mmap` will assign to the allocations that don't ask for a
        // specific address, subject to ASLR adjustment.
        state.mmap_top = state
            .stack_origin
            .checked_sub(generate_random_offset_for_aslr(arch_width))
            .ok_or_else(|| errno!(EINVAL))?;
        Ok(())
    }

    // Test tasks are not initialized by exec; simulate its behavior by initializing memory layout
    // as if a zero-size executable was loaded.
    pub fn initialize_mmap_layout_for_test(self: &Arc<Self>, arch_width: ArchWidth) {
        self.initialize_mmap_layout(arch_width).unwrap();
        let fake_executable_addr = self.get_random_base_for_executable(arch_width, 0).unwrap();
        self.initialize_brk_origin(arch_width, fake_executable_addr).unwrap();
    }

    pub fn initialize_brk_origin(
        self: &Arc<Self>,
        arch_width: ArchWidth,
        executable_end: UserAddress,
    ) -> Result<(), Errno> {
        self.state.write().brk_origin = executable_end
            .checked_add(generate_random_offset_for_aslr(arch_width))
            .ok_or_else(|| errno!(EINVAL))?;
        Ok(())
    }

    // Get a randomised address for loading a position-independent executable.
    pub fn get_random_base_for_executable(
        &self,
        arch_width: ArchWidth,
        length: usize,
    ) -> Result<UserAddress, Errno> {
        let state = self.state.read();

        // Place it at approx. 2/3 of the available mmap space, subject to ASLR adjustment.
        let base = round_up_to_system_page_size(2 * state.mmap_top.ptr() / 3).unwrap()
            + generate_random_offset_for_aslr(arch_width);
        if base.checked_add(length).ok_or_else(|| errno!(EINVAL))? <= state.mmap_top.ptr() {
            Ok(UserAddress::from_ptr(base))
        } else {
            Err(errno!(EINVAL))
        }
    }
    pub fn executable_node(&self) -> Option<NamespaceNode> {
        self.state.read().executable_node.clone()
    }

    fn get_errno_for_map_err(status: zx::Status) -> Errno {
        match status {
            zx::Status::INVALID_ARGS => errno!(EINVAL),
            zx::Status::ACCESS_DENIED => errno!(EPERM),
            zx::Status::NOT_SUPPORTED => errno!(ENODEV),
            zx::Status::NO_MEMORY => errno!(ENOMEM),
            zx::Status::NO_RESOURCES => errno!(ENOMEM),
            zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
            zx::Status::ALREADY_EXISTS => errno!(EEXIST),
            zx::Status::BAD_STATE => errno!(EINVAL),
            _ => impossible_error(status),
        }
    }

    pub fn map_memory(
        self: &Arc<Self>,
        addr: DesiredAddress,
        memory: Arc<MemoryObject>,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        max_access: Access,
        options: MappingOptions,
        name: MappingName,
        file_write_guard: FileWriteGuardRef,
    ) -> Result<UserAddress, Errno> {
        let flags = MappingFlags::from_access_flags_and_options(prot_flags, options);

        // Unmapped mappings must be released after the state is unlocked.
        let mut released_mappings = vec![];
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();
        let result = state.map_memory(
            self,
            addr,
            memory,
            memory_offset,
            length,
            flags,
            max_access,
            options.contains(MappingOptions::POPULATE),
            name,
            file_write_guard,
            &mut released_mappings,
        );

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        profile_duration!("DropReleasedMappings");
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result
    }

    pub fn map_anonymous(
        self: &Arc<Self>,
        addr: DesiredAddress,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        name: MappingName,
    ) -> Result<UserAddress, Errno> {
        let mut released_mappings = vec![];
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();
        let result = state.map_anonymous(
            self,
            addr,
            length,
            prot_flags,
            options,
            name,
            &mut released_mappings,
        );

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result
    }

    /// Map the stack into a pre-selected address region
    pub fn map_stack(
        self: &Arc<Self>,
        length: usize,
        prot_flags: ProtectionFlags,
    ) -> Result<UserAddress, Errno> {
        assert!(length <= MAX_STACK_SIZE);
        let addr = self.state.read().stack_origin;
        // The address range containing stack_origin should normally be available: it's above the
        // mmap_top, and this method is called early enough in the process lifetime that only the
        // main ELF and the interpreter are already loaded. However, in the rare case that the
        // static position-independent executable is overlapping the chosen address, mapping as Hint
        // will make mmap choose a new place for it.
        // TODO(https://fxbug.dev/370027241): Consider a more robust approach
        let stack_addr = self.map_anonymous(
            DesiredAddress::Hint(addr),
            length,
            prot_flags,
            MappingOptions::ANONYMOUS | MappingOptions::GROWSDOWN,
            MappingName::Stack,
        )?;
        if stack_addr != addr {
            log_warn!(
                "An address designated for stack ({}) was unavailable, mapping at {} instead.",
                addr,
                stack_addr
            );
        }
        Ok(stack_addr)
    }

    pub fn remap(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        addr: UserAddress,
        old_length: usize,
        new_length: usize,
        flags: MremapFlags,
        new_addr: UserAddress,
    ) -> Result<UserAddress, Errno> {
        let mut released_mappings = vec![];
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();
        let result = state.remap(
            current_task,
            self,
            addr,
            old_length,
            new_length,
            flags,
            new_addr,
            &mut released_mappings,
        );

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result
    }

    pub fn unmap(self: &Arc<Self>, addr: UserAddress, length: usize) -> Result<(), Errno> {
        let mut released_mappings = vec![];
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();
        let result = state.unmap(self, addr, length, &mut released_mappings);

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result
    }

    pub fn protect(
        &self,
        addr: UserAddress,
        length: usize,
        prot_flags: ProtectionFlags,
    ) -> Result<(), Errno> {
        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();
        state.protect(addr, length, prot_flags)
    }

    pub fn madvise(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        length: usize,
        advice: u32,
    ) -> Result<(), Errno> {
        self.state.write().madvise(current_task, addr, length, advice)
    }

    pub fn handle_page_fault(
        &self,
        decoded: PageFaultExceptionReport,
        error_code: zx::Status,
    ) -> ExceptionResult {
        // A page fault may be resolved by extending a growsdown mapping to cover the faulting
        // address. Mark the exception handled if so. Otherwise let the regular handling proceed.

        // We should only attempt growth on a not-present fault and we should only extend if the
        // access type matches the protection on the GROWSDOWN mapping.
        if decoded.not_present {
            match self.extend_growsdown_mapping_to_address(
                UserAddress::from(decoded.faulting_address),
                decoded.is_write,
            ) {
                Ok(true) => {
                    return ExceptionResult::Handled;
                }
                Err(e) => {
                    log_warn!("Error handling page fault: {e}")
                }
                _ => {}
            }
        }
        // For this exception type, the synth_code field in the exception report's context is the
        // error generated by the page fault handler. For us this is used to distinguish between a
        // segmentation violation and a bus error. Unfortunately this detail is not documented in
        // Zircon's public documentation and is only described in the architecture-specific
        // exception definitions such as:
        // zircon/kernel/arch/x86/include/arch/x86.h
        // zircon/kernel/arch/arm64/include/arch/arm64.h
        let signo = match error_code {
            zx::Status::OUT_OF_RANGE => SIGBUS,
            _ => SIGSEGV,
        };
        ExceptionResult::Signal(SignalInfo::new(
            signo,
            SI_KERNEL as i32,
            SignalDetail::SigFault { addr: decoded.faulting_address },
        ))
    }

    pub fn set_mapping_name(
        &self,
        addr: UserAddress,
        length: usize,
        name: Option<FsString>,
    ) -> Result<(), Errno> {
        if addr.ptr() % *PAGE_SIZE as usize != 0 {
            return error!(EINVAL);
        }
        let end = match addr.checked_add(length) {
            Some(addr) => addr.round_up(*PAGE_SIZE).map_err(|_| errno!(ENOMEM))?,
            None => return error!(EINVAL),
        };
        let mut state = self.state.write();

        let mappings_in_range = state
            .mappings
            .intersection(addr..end)
            .map(|(r, m)| (r.clone(), m.clone()))
            .collect::<Vec<_>>();

        if mappings_in_range.is_empty() {
            return error!(EINVAL);
        }
        if !mappings_in_range.first().unwrap().0.contains(&addr) {
            return error!(ENOMEM);
        }

        let mut last_range_end = None;
        // There's no get_mut on RangeMap, because it would be hard to implement correctly in
        // combination with merging of adjacent mappings. Instead, make a copy, change the copy,
        // and insert the copy.
        for (mut range, mut mapping) in mappings_in_range {
            if let MappingName::File(_) = mapping.name {
                // It's invalid to assign a name to a file-backed mapping.
                return error!(EBADF);
            }
            if range.start < addr {
                // This mapping starts before the named region. Split the mapping so we can apply the name only to
                // the specified region.
                let start_split_range = range.start..addr;
                let start_split_length = addr - range.start;
                let start_split_mapping = match &mut mapping.backing {
                    MappingBacking::Memory(backing) => {
                        // Shrink the range of the named mapping to only the named area.
                        backing.memory_offset = start_split_length as u64;
                        Mapping::new(
                            range.start,
                            backing.memory.clone(),
                            backing.memory_offset,
                            mapping.flags,
                            mapping.max_access,
                            mapping.file_write_guard.clone(),
                        )
                    }
                    #[cfg(feature = "alternate_anon_allocs")]
                    MappingBacking::PrivateAnonymous => {
                        Mapping::new_private_anonymous(mapping.flags, mapping.name.clone())
                    }
                };
                state.mappings.insert(start_split_range, start_split_mapping);

                range = addr..range.end;
            }
            if let Some(last_range_end) = last_range_end {
                if last_range_end != range.start {
                    // The name must apply to a contiguous range of mapped pages.
                    return error!(ENOMEM);
                }
            }
            last_range_end = Some(range.end.round_up(*PAGE_SIZE)?);
            // TODO(b/310255065): We have no place to store names in a way visible to programs outside of Starnix
            // such as memory analysis tools.
            #[cfg(not(feature = "alternate_anon_allocs"))]
            {
                let MappingBacking::Memory(backing) = &mapping.backing;
                match &name {
                    Some(memory_name) => {
                        backing.memory.set_zx_name(memory_name);
                    }
                    None => {
                        backing.memory.set_zx_name(b"");
                    }
                }
            }
            if range.end > end {
                // The named region ends before the last mapping ends. Split the tail off of the
                // last mapping to have an unnamed mapping after the named region.
                let tail_range = end..range.end;
                let tail_offset = range.end - end;
                let tail_mapping = match &mapping.backing {
                    MappingBacking::Memory(backing) => Mapping::new(
                        end,
                        backing.memory.clone(),
                        backing.memory_offset + tail_offset as u64,
                        mapping.flags,
                        mapping.max_access,
                        mapping.file_write_guard.clone(),
                    ),
                    #[cfg(feature = "alternate_anon_allocs")]
                    MappingBacking::PrivateAnonymous => {
                        Mapping::new_private_anonymous(mapping.flags, mapping.name.clone())
                    }
                };
                state.mappings.insert(tail_range, tail_mapping);
                range.end = end;
            }
            mapping.name = match &name {
                Some(name) => MappingName::Vma(name.clone()),
                None => MappingName::None,
            };
            state.mappings.insert(range, mapping);
        }
        if let Some(last_range_end) = last_range_end {
            if last_range_end < end {
                // The name must apply to a contiguous range of mapped pages.
                return error!(ENOMEM);
            }
        }
        Ok(())
    }

    /// Returns [`Ok`] if the entire range specified by `addr..(addr+length)` contains valid
    /// mappings.
    ///
    /// # Errors
    ///
    /// Returns [`Err(errno)`] where `errno` is:
    ///
    ///   - `EINVAL`: `addr` is not page-aligned, or the range is too large,
    ///   - `ENOMEM`: one or more pages in the range are not mapped.
    pub fn ensure_mapped(&self, addr: UserAddress, length: usize) -> Result<(), Errno> {
        if !addr.is_aligned(*PAGE_SIZE) {
            return error!(EINVAL);
        }

        let length = round_up_to_system_page_size(length)?;
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
        let state = self.state.read();
        let mut last_end = addr;
        for (range, _) in state.mappings.intersection(addr..end_addr) {
            if range.start > last_end {
                // This mapping does not start immediately after the last.
                return error!(ENOMEM);
            }
            last_end = range.end;
        }
        if last_end < end_addr {
            // There is a gap of no mappings at the end of the range.
            error!(ENOMEM)
        } else {
            Ok(())
        }
    }

    /// Returns the memory object mapped at the address and the offset into the memory object of
    /// the address. Intended for implementing futexes.
    pub fn get_mapping_memory(
        &self,
        addr: UserAddress,
        perms: ProtectionFlags,
    ) -> Result<(Arc<MemoryObject>, u64), Errno> {
        let state = self.state.read();
        let (_, mapping) = state.mappings.get(&addr).ok_or_else(|| errno!(EFAULT))?;
        if !mapping.flags.access_flags().contains(perms) {
            return error!(EACCES);
        }
        match &mapping.backing {
            MappingBacking::Memory(backing) => {
                Ok((Arc::clone(&backing.memory), mapping.address_to_offset(addr)))
            }
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                Ok((Arc::clone(&state.private_anonymous.backing), addr.ptr() as u64))
            }
        }
    }

    /// Does a rough check that the given address is plausibly in the address space of the
    /// application. This does not mean the pointer is valid for any particular purpose or that
    /// it will remain so!
    ///
    /// In some syscalls, Linux seems to do some initial validation of the pointer up front to
    /// tell the caller early if it's invalid. For example, in epoll_wait() it's returning a vector
    /// of events. If the caller passes an invalid pointer, it wants to fail without dropping any
    /// events. Failing later when actually copying the required events to userspace would mean
    /// those events will be lost. But holding a lock on the memory manager for an asynchronous
    /// wait is not desirable.
    ///
    /// Testing shows that Linux seems to do some initial plausibility checking of the pointer to
    /// be able to report common usage errors before doing any (possibly unreversable) work. This
    /// checking is easy to get around if you try, so this function is also not required to
    /// be particularly robust. Certainly the more advanced cases of races (the memory could be
    /// unmapped after this call but before it's used) are not handled.
    ///
    /// The buffer_size variable is the size of the data structure that needs to fit
    /// in the given memory.
    ///
    /// Returns the error EFAULT if invalid.
    pub fn check_plausible(&self, addr: UserAddress, buffer_size: usize) -> Result<(), Errno> {
        let state = self.state.read();

        if let Some(range) = state.mappings.last_range() {
            if range.end - buffer_size >= addr {
                return Ok(());
            }
        }
        error!(EFAULT)
    }

    pub fn get_aio_context(&self, addr: UserAddress) -> Option<Arc<AioContext>> {
        let state = self.state.read();
        state.get_aio_context(addr).map(|(_, aio_context)| aio_context)
    }

    pub fn destroy_aio_context(
        self: &Arc<Self>,
        addr: UserAddress,
    ) -> Result<Arc<AioContext>, Errno> {
        let mut released_mappings = vec![];

        // Hold the lock throughout the operation to uphold memory manager's invariants.
        // See mm/README.md.
        let mut state = self.state.write();

        // Validate that this address actually has an AioContext. We need to hold the state lock
        // until we actually remove the mappings to ensure that another thread does not manipulate
        // the mappings after we've validated that they contain an AioContext.
        let Some((range, aio_context)) = state.get_aio_context(addr) else {
            return error!(EINVAL);
        };

        let length = range.end - range.start;
        let result = state.unmap(self, range.start, length, &mut released_mappings);

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result.map(|_| aio_context)
    }

    #[cfg(test)]
    pub fn get_mapping_name(&self, addr: UserAddress) -> Result<Option<FsString>, Errno> {
        let state = self.state.read();
        let (_, mapping) = state.mappings.get(&addr).ok_or_else(|| errno!(EFAULT))?;
        if let MappingName::Vma(name) = &mapping.name {
            Ok(Some(name.clone()))
        } else {
            Ok(None)
        }
    }

    #[cfg(test)]
    pub fn get_mapping_count(&self) -> usize {
        let state = self.state.read();
        state.mappings.iter().count()
    }

    pub fn extend_growsdown_mapping_to_address(
        &self,
        addr: UserAddress,
        is_write: bool,
    ) -> Result<bool, Error> {
        self.state.write().extend_growsdown_mapping_to_address(addr, is_write)
    }

    pub fn get_stats(&self) -> Result<MemoryStats, Errno> {
        let mut result = MemoryStats::default();
        let state = self.state.read();
        for (range, mapping) in state.mappings.iter() {
            let size = range.end.ptr() - range.start.ptr();
            result.vm_size += size;

            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    let memory_info = backing.memory.info()?;
                    let committed_bytes = memory_info.committed_bytes as usize;
                    let populated_bytes = memory_info.populated_bytes as usize;

                    result.vm_rss += committed_bytes;

                    if mapping.flags.contains(MappingFlags::ANONYMOUS)
                        && !mapping.flags.contains(MappingFlags::SHARED)
                    {
                        result.rss_anonymous += committed_bytes;
                        result.vm_swap += populated_bytes - committed_bytes;
                    }

                    if memory_info.share_count > 1 {
                        result.rss_shared += committed_bytes;
                    }

                    if let MappingName::File(_) = mapping.name {
                        result.rss_file += committed_bytes;
                    }
                }
                #[cfg(feature = "alternate_anon_allocs")]
                MappingBacking::PrivateAnonymous => {
                    // TODO(b/310255065): Populate |result|
                }
            }

            if mapping.flags.contains(MappingFlags::ELF_BINARY)
                && mapping.flags.contains(MappingFlags::WRITE)
            {
                result.vm_data += size;
            }

            if mapping.flags.contains(MappingFlags::ELF_BINARY)
                && mapping.flags.contains(MappingFlags::EXEC)
            {
                result.vm_exe += size;
            }
        }
        result.vm_stack = state.stack_size;
        Ok(result)
    }

    pub fn atomic_load_u32_relaxed(&self, futex_addr: FutexAddress) -> Result<u32, Errno> {
        if let Some(usercopy) = usercopy() {
            usercopy.atomic_load_u32_relaxed(futex_addr.ptr()).map_err(|_| errno!(EFAULT))
        } else {
            // SAFETY: `self.state.read().read_memory` only returns `Ok` if all
            // bytes were read to.
            let buf = unsafe {
                read_to_array(|buf| {
                    self.state.read().read_memory(futex_addr.into(), buf).map(|bytes_read| {
                        debug_assert_eq!(bytes_read.len(), std::mem::size_of::<u32>())
                    })
                })
            }?;
            Ok(u32::from_ne_bytes(buf))
        }
    }

    pub fn atomic_store_u32_relaxed(
        &self,
        futex_addr: FutexAddress,
        value: u32,
    ) -> Result<(), Errno> {
        if let Some(usercopy) = usercopy() {
            usercopy.atomic_store_u32_relaxed(futex_addr.ptr(), value).map_err(|_| errno!(EFAULT))
        } else {
            self.state.read().write_memory(futex_addr.into(), value.as_bytes())?;
            Ok(())
        }
    }

    pub fn get_restricted_vmar_info(&self) -> Option<VmarInfo> {
        use zx::HandleBased;
        if self.root_vmar.is_invalid_handle() {
            return None;
        }
        Some(VmarInfo { base: RESTRICTED_ASPACE_BASE, len: RESTRICTED_ASPACE_SIZE })
    }
}

/// The user-space address at which a mapping should be placed. Used by [`MemoryManager::map`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredAddress {
    /// Map at any address chosen by the kernel.
    Any,
    /// The address is a hint. If the address overlaps an existing mapping a different address may
    /// be chosen.
    Hint(UserAddress),
    /// The address is a requirement. If the address overlaps an existing mapping (and cannot
    /// overwrite it), mapping fails.
    Fixed(UserAddress),
    /// The address is a requirement. If the address overlaps an existing mapping (and cannot
    /// overwrite it), they should be unmapped.
    FixedOverwrite(UserAddress),
}

/// The user-space address at which a mapping should be placed. Used by [`map_in_vmar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedAddress {
    /// See DesiredAddress::Fixed.
    Fixed(UserAddress),
    /// See DesiredAddress::FixedOverwrite.
    FixedOverwrite(UserAddress),
}

impl SelectedAddress {
    #[cfg(feature = "alternate_anon_allocs")]
    fn addr(&self) -> UserAddress {
        match self {
            SelectedAddress::Fixed(addr) => *addr,
            SelectedAddress::FixedOverwrite(addr) => *addr,
        }
    }
}

fn write_map(
    task: &Task,
    sink: &mut DynamicFileBuf,
    range: &Range<UserAddress>,
    map: &Mapping,
) -> Result<(), Errno> {
    let line_length = write!(
        sink,
        "{:08x}-{:08x} {}{}{}{} {:08x} 00:00 {} ",
        range.start.ptr(),
        range.end.ptr(),
        if map.can_read() { 'r' } else { '-' },
        if map.can_write() { 'w' } else { '-' },
        if map.can_exec() { 'x' } else { '-' },
        if map.flags.contains(MappingFlags::SHARED) { 's' } else { 'p' },
        match &map.backing {
            MappingBacking::Memory(backing) => backing.memory_offset,
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => 0,
        },
        if let MappingName::File(filename) = &map.name {
            filename.entry.node.info().ino
        } else {
            0
        }
    )?;
    let fill_to_name = |sink: &mut DynamicFileBuf| {
        // The filename goes at >= the 74th column (73rd when zero indexed)
        for _ in line_length..73 {
            sink.write(b" ");
        }
    };
    match &map.name {
        MappingName::None | MappingName::AioContext(_) => {
            if map.flags.contains(MappingFlags::SHARED)
                && map.flags.contains(MappingFlags::ANONYMOUS)
            {
                // See proc(5), "/proc/[pid]/map_files/"
                fill_to_name(sink);
                sink.write(b"/dev/zero (deleted)");
            }
        }
        MappingName::Stack => {
            fill_to_name(sink);
            sink.write(b"[stack]");
        }
        MappingName::Heap => {
            fill_to_name(sink);
            sink.write(b"[heap]");
        }
        MappingName::Vdso => {
            fill_to_name(sink);
            sink.write(b"[vdso]");
        }
        MappingName::Vvar => {
            fill_to_name(sink);
            sink.write(b"[vvar]");
        }
        MappingName::File(name) => {
            fill_to_name(sink);
            // File names can have newlines that need to be escaped before printing.
            // According to https://man7.org/linux/man-pages/man5/proc.5.html the only
            // escaping applied to paths is replacing newlines with an octal sequence.
            let path = name.path(task);
            sink.write_iter(
                path.iter()
                    .flat_map(|b| if *b == b'\n' { b"\\012" } else { std::slice::from_ref(b) })
                    .copied(),
            );
            // If the mapping is file-backed and the file has been
            // deleted, the string " (deleted)" is appended to the
            // pathname.
            if name.entry.is_dead() {
                sink.write(b" (deleted)");
            }
        }
        MappingName::Vma(name) => {
            fill_to_name(sink);
            sink.write(b"[anon:");
            sink.write(name.as_bytes());
            sink.write(b"]");
        }
        MappingName::Ashmem(name) => {
            fill_to_name(sink);
            sink.write(b"/dev/ashmem/");
            sink.write(name.as_bytes());
        }
    }
    sink.write(b"\n");
    Ok(())
}

#[derive(Default)]
pub struct MemoryStats {
    pub vm_size: usize,
    pub vm_rss: usize,
    pub rss_anonymous: usize,
    pub rss_file: usize,
    pub rss_shared: usize,
    pub vm_data: usize,
    pub vm_stack: usize,
    pub vm_exe: usize,
    pub vm_swap: usize,
}

#[derive(Clone)]
pub struct ProcMapsFile(WeakRef<Task>);
impl ProcMapsFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}

impl SequenceFileSource for ProcMapsFile {
    type Cursor = UserAddress;

    fn next(
        &self,
        cursor: UserAddress,
        sink: &mut DynamicFileBuf,
    ) -> Result<Option<UserAddress>, Errno> {
        let task = Task::from_weak(&self.0)?;
        let state = task.mm().state.read();
        let mut iter = state.mappings.iter_starting_at(&cursor);
        if let Some((range, map)) = iter.next() {
            write_map(&task, sink, range, map)?;
            return Ok(Some(range.end));
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub struct ProcSmapsFile(WeakRef<Task>);
impl ProcSmapsFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}

impl SequenceFileSource for ProcSmapsFile {
    type Cursor = UserAddress;

    fn next(
        &self,
        cursor: UserAddress,
        sink: &mut DynamicFileBuf,
    ) -> Result<Option<UserAddress>, Errno> {
        let page_size_kb = *PAGE_SIZE / 1024;
        let task = Task::from_weak(&self.0)?;
        let state = task.mm().state.read();
        let mut iter = state.mappings.iter_starting_at(&cursor);
        if let Some((range, map)) = iter.next() {
            write_map(&task, sink, range, map)?;

            let size_kb = (range.end.ptr() - range.start.ptr()) / 1024;
            writeln!(sink, "Size:           {size_kb:>8} kB",)?;

            let (committed_bytes, share_count) = match &map.backing {
                MappingBacking::Memory(backing) => {
                    let memory_info = backing.memory.info()?;
                    (memory_info.committed_bytes, memory_info.share_count as u64)
                }
                #[cfg(feature = "alternate_anon_allocs")]
                MappingBacking::PrivateAnonymous => {
                    // TODO(b/310255065): Compute committed bytes and share count
                    (0, 1)
                }
            };

            let rss_kb = committed_bytes / 1024;
            writeln!(sink, "Rss:            {rss_kb:>8} kB")?;

            let pss_kb = if map.flags.contains(MappingFlags::SHARED) {
                rss_kb / share_count as u64
            } else {
                rss_kb
            };
            writeln!(sink, "Pss:            {pss_kb:>8} kB")?;

            track_stub!(TODO("https://fxbug.dev/322874967"), "smaps dirty pages");
            let (shared_dirty_kb, private_dirty_kb) = (0, 0);

            let is_shared = share_count > 1;
            let shared_clean_kb = if is_shared { rss_kb } else { 0 };
            writeln!(sink, "Shared_Clean:   {shared_clean_kb:>8} kB")?;
            writeln!(sink, "Shared_Dirty:   {shared_dirty_kb:>8} kB")?;

            let private_clean_kb = if is_shared { 0 } else { rss_kb };
            writeln!(sink, "Private_Clean:  {private_clean_kb:>8} kB")?;
            writeln!(sink, "Private_Dirty:  {private_dirty_kb:>8} kB")?;

            let anonymous_kb = if map.private_anonymous() { rss_kb } else { 0 };
            writeln!(sink, "Anonymous:      {anonymous_kb:>8} kB")?;
            writeln!(sink, "KernelPageSize: {page_size_kb:>8} kB")?;
            writeln!(sink, "MMUPageSize:    {page_size_kb:>8} kB")?;
            writeln!(sink, "VmFlags: {}", map.vm_flags())?;

            track_stub!(TODO("https://fxbug.dev/297444691"), "optional smaps fields");
            return Ok(Some(range.end));
        }
        Ok(None)
    }
}

/// Creates a memory object that can be used in an anonymous mapping for the `mmap` syscall.
pub fn create_anonymous_mapping_memory(size: u64) -> Result<Arc<MemoryObject>, Errno> {
    let mut profile = ProfileDuration::enter("CreatAnonVmo");
    // mremap can grow memory regions, so make sure the memory object is resizable.
    let mut memory = MemoryObject::from(
        zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, size).map_err(|s| match s {
            zx::Status::NO_MEMORY => errno!(ENOMEM),
            zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
            _ => impossible_error(s),
        })?,
    );

    profile.pivot("SetAnonVmoName");
    memory.set_zx_name(b"starnix-anon");

    profile.pivot("ReplaceAnonVmoAsExecutable");
    // TODO(https://fxbug.dev/42056890): Audit replace_as_executable usage
    memory = memory.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?;
    Ok(Arc::new(memory))
}

fn generate_random_offset_for_aslr(arch_width: ArchWidth) -> usize {
    // Generate a number with ASLR_RANDOM_BITS.
    let randomness = {
        let random_bits =
            if arch_width.is_arch32() { ASLR_32_RANDOM_BITS } else { ASLR_RANDOM_BITS };
        let mask = (1 << random_bits) - 1;
        let mut bytes = [0; std::mem::size_of::<usize>()];
        zx::cprng_draw(&mut bytes);
        usize::from_le_bytes(bytes) & mask
    };

    // Transform it into a page-aligned offset.
    randomness * (*PAGE_SIZE as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::syscalls::do_mmap;
    use crate::task::syscalls::sys_prctl;
    use crate::testing::*;
    use crate::vfs::FdNumber;
    use assert_matches::assert_matches;
    use itertools::assert_equal;
    use starnix_sync::{FileOpsCore, LockEqualOrBefore};
    use starnix_uapi::{
        MAP_ANONYMOUS, MAP_FIXED, MAP_GROWSDOWN, MAP_PRIVATE, PROT_NONE, PR_SET_VMA,
        PR_SET_VMA_ANON_NAME,
    };
    use std::ffi::CString;
    use zerocopy::KnownLayout;

    #[::fuchsia::test]
    fn test_mapping_flags() {
        let options = MappingOptions::ANONYMOUS;
        let access_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
        let mapping_flags = MappingFlags::from_access_flags_and_options(access_flags, options);
        assert_eq!(mapping_flags.access_flags(), access_flags);
        assert_eq!(mapping_flags.options(), options);

        let new_access_flags = ProtectionFlags::READ | ProtectionFlags::EXEC;
        let adusted_mapping_flags = mapping_flags.with_access_flags(new_access_flags);
        assert_eq!(adusted_mapping_flags.access_flags(), new_access_flags);
        assert_eq!(adusted_mapping_flags.options(), options);
    }

    #[::fuchsia::test]
    async fn test_brk() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        // Look up the given addr in the mappings table.
        let get_range = |addr: UserAddress| {
            let state = mm.state.read();
            state.mappings.get(&addr).map(|(range, mapping)| (range.clone(), mapping.clone()))
        };

        // Initialize the program break.
        let base_addr = mm
            .set_brk(&current_task, UserAddress::default())
            .expect("failed to set initial program break");
        assert!(base_addr > UserAddress::default());

        // Page containing the program break address should not be mapped.
        assert_eq!(get_range(base_addr), None);

        // Growing it by a single byte results in that page becoming mapped.
        let addr0 = mm.set_brk(&current_task, base_addr + 1u64).expect("failed to grow brk");
        assert!(addr0 > base_addr);
        let (range0, _) = get_range(base_addr).expect("base_addr should be mapped");
        assert_eq!(range0.start, base_addr);
        assert_eq!(range0.end, base_addr + *PAGE_SIZE);

        // Grow the program break by another byte, which won't be enough to cause additional pages to be mapped.
        let addr1 = mm.set_brk(&current_task, base_addr + 2u64).expect("failed to grow brk");
        assert_eq!(addr1, base_addr + 2u64);
        let (range1, _) = get_range(base_addr).expect("base_addr should be mapped");
        assert_eq!(range1.start, range0.start);
        assert_eq!(range1.end, range0.end);

        // Grow the program break by a non-trival amount and observe the larger mapping.
        let addr2 = mm.set_brk(&current_task, base_addr + 24893u64).expect("failed to grow brk");
        assert_eq!(addr2, base_addr + 24893u64);
        let (range2, _) = get_range(base_addr).expect("base_addr should be mapped");
        assert_eq!(range2.start, base_addr);
        assert_eq!(range2.end, addr2.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break and observe the smaller mapping.
        let addr3 = mm.set_brk(&current_task, base_addr + 14832u64).expect("failed to shrink brk");
        assert_eq!(addr3, base_addr + 14832u64);
        let (range3, _) = get_range(base_addr).expect("base_addr should be mapped");
        assert_eq!(range3.start, base_addr);
        assert_eq!(range3.end, addr3.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break close to zero and observe the smaller mapping.
        let addr4 =
            mm.set_brk(&current_task, base_addr + 3u64).expect("failed to drastically shrink brk");
        assert_eq!(addr4, base_addr + 3u64);
        let (range4, _) = get_range(base_addr).expect("base_addr should be mapped");
        assert_eq!(range4.start, base_addr);
        assert_eq!(range4.end, addr4.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break to zero and observe that the mapping is entirely gone.
        let addr5 =
            mm.set_brk(&current_task, base_addr).expect("failed to drastically shrink brk to zero");
        assert_eq!(addr5, base_addr);
        assert_eq!(get_range(base_addr), None);
    }

    #[::fuchsia::test]
    async fn test_mm_exec() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let has = |addr: &UserAddress| -> bool {
            let state = mm.state.read();
            state.mappings.get(addr).is_some()
        };

        let brk_addr = mm
            .set_brk(&current_task, UserAddress::default())
            .expect("failed to set initial program break");
        assert!(brk_addr > UserAddress::default());

        // Allocate a single page of BRK space, so that the break base address is mapped.
        let _ = mm.set_brk(&current_task, brk_addr + 1u64).expect("failed to grow program break");
        assert!(has(&brk_addr));

        let mapped_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        assert!(mapped_addr > UserAddress::default());
        assert!(has(&mapped_addr));

        let node = current_task.lookup_path_from_root(&mut locked, "/".into()).unwrap();
        mm.exec(node, ArchWidth::Arch64).expect("failed to exec memory manager");

        assert!(!has(&brk_addr));
        assert!(!has(&mapped_addr));

        // Check that the old addresses are actually available for mapping.
        let brk_addr2 = map_memory(&mut locked, &current_task, brk_addr, *PAGE_SIZE);
        assert_eq!(brk_addr, brk_addr2);
        let mapped_addr2 = map_memory(&mut locked, &current_task, mapped_addr, *PAGE_SIZE);
        assert_eq!(mapped_addr, mapped_addr2);
    }

    #[::fuchsia::test]
    async fn test_get_contiguous_mappings_at() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        // Create four one-page mappings with a hole between the third one and the fourth one.
        let page_size = *PAGE_SIZE as usize;
        let addr_a = mm.base_addr + 10 * page_size;
        let addr_b = mm.base_addr + 11 * page_size;
        let addr_c = mm.base_addr + 12 * page_size;
        let addr_d = mm.base_addr + 14 * page_size;
        assert_eq!(map_memory(&mut locked, &current_task, addr_a, *PAGE_SIZE), addr_a);
        assert_eq!(map_memory(&mut locked, &current_task, addr_b, *PAGE_SIZE), addr_b);
        assert_eq!(map_memory(&mut locked, &current_task, addr_c, *PAGE_SIZE), addr_c);
        assert_eq!(map_memory(&mut locked, &current_task, addr_d, *PAGE_SIZE), addr_d);

        {
            let mm_state = mm.state.read();
            // Verify that requesting an unmapped address returns an empty iterator.
            assert_equal(mm_state.get_contiguous_mappings_at(addr_a - 100u64, 50).unwrap(), vec![]);
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a - 100u64, 200).unwrap(),
                vec![],
            );

            // Verify that requesting zero bytes returns an empty iterator.
            assert_equal(mm_state.get_contiguous_mappings_at(addr_a, 0).unwrap(), vec![]);

            // Verify errors.
            assert_eq!(
                mm_state
                    .get_contiguous_mappings_at(UserAddress::from(100), usize::MAX)
                    .err()
                    .unwrap(),
                errno!(EFAULT)
            );
            assert_eq!(
                mm_state
                    .get_contiguous_mappings_at(mm_state.max_address() + 1u64, 0)
                    .err()
                    .unwrap(),
                errno!(EFAULT)
            );
        }

        // Test strategy-specific properties.
        #[cfg(feature = "alternate_anon_allocs")]
        {
            assert_eq!(mm.get_mapping_count(), 2);
            let mm_state = mm.state.read();
            let (map_a, map_b) = {
                let mut it = mm_state.mappings.iter();
                (it.next().unwrap().1, it.next().unwrap().1)
            };

            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size).unwrap(),
                vec![(map_a, page_size)],
            );

            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size / 2).unwrap(),
                vec![(map_a, page_size / 2)],
            );

            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size * 3).unwrap(),
                vec![(map_a, page_size * 3)],
            );

            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_b, page_size).unwrap(),
                vec![(map_a, page_size)],
            );

            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_d, page_size).unwrap(),
                vec![(map_b, page_size)],
            );

            // Verify that results stop if there is a hole.
            assert_equal(
                mm_state
                    .get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10)
                    .unwrap(),
                vec![(map_a, page_size * 2 + page_size / 2)],
            );

            // Verify that results stop at the last mapped page.
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_d, page_size * 10).unwrap(),
                vec![(map_b, page_size)],
            );
        }
        #[cfg(not(feature = "alternate_anon_allocs"))]
        {
            assert_eq!(mm.get_mapping_count(), 4);

            let mm_state = mm.state.read();
            // Obtain references to the mappings.
            let (map_a, map_b, map_c, map_d) = {
                let mut it = mm_state.mappings.iter();
                (
                    it.next().unwrap().1,
                    it.next().unwrap().1,
                    it.next().unwrap().1,
                    it.next().unwrap().1,
                )
            };

            // Verify result when requesting a whole mapping or portions of it.
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size).unwrap(),
                vec![(map_a, page_size)],
            );
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size / 2).unwrap(),
                vec![(map_a, page_size / 2)],
            );
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a + page_size / 2, page_size / 2).unwrap(),
                vec![(map_a, page_size / 2)],
            );
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a + page_size / 4, page_size / 8).unwrap(),
                vec![(map_a, page_size / 8)],
            );

            // Verify result when requesting a range spanning more than one mapping.
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a + page_size / 2, page_size).unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 3 / 2)
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size)],
            );
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a, page_size * 3 / 2).unwrap(),
                vec![(map_a, page_size), (map_b, page_size / 2)],
            );
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 2).unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size), (map_c, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .get_contiguous_mappings_at(addr_b + page_size / 2, page_size * 3 / 2)
                    .unwrap(),
                vec![(map_b, page_size / 2), (map_c, page_size)],
            );

            // Verify that results stop if there is a hole.
            assert_equal(
                mm_state
                    .get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10)
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size), (map_c, page_size)],
            );

            // Verify that results stop at the last mapped page.
            assert_equal(
                mm_state.get_contiguous_mappings_at(addr_d, page_size * 10).unwrap(),
                vec![(map_d, page_size)],
            );
        }
    }

    #[::fuchsia::test]
    async fn test_read_write_crossing_mappings() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        // Map two contiguous pages at fixed addresses, but backed by distinct mappings.
        let page_size = *PAGE_SIZE;
        let addr = mm.base_addr + 10 * page_size;
        assert_eq!(map_memory(&mut locked, &current_task, addr, page_size), addr);
        assert_eq!(
            map_memory(&mut locked, &current_task, addr + page_size, page_size),
            addr + page_size
        );
        #[cfg(feature = "alternate_anon_allocs")]
        assert_eq!(mm.get_mapping_count(), 1);
        #[cfg(not(feature = "alternate_anon_allocs"))]
        assert_eq!(mm.get_mapping_count(), 2);

        // Write a pattern crossing our two mappings.
        let test_addr = addr + page_size / 2;
        let data: Vec<u8> = (0..page_size).map(|i| (i % 256) as u8).collect();
        ma.write_memory(test_addr, &data).expect("failed to write test data");

        // Read it back.
        let data_readback =
            ma.read_memory_to_vec(test_addr, data.len()).expect("failed to read test data");
        assert_eq!(&data, &data_readback);
    }

    #[::fuchsia::test]
    async fn test_read_write_errors() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();

        let page_size = *PAGE_SIZE;
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), page_size);
        let buf = vec![0u8; page_size as usize];

        // Verify that accessing data that is only partially mapped is an error.
        let partial_addr_before = addr - page_size / 2;
        assert_eq!(ma.write_memory(partial_addr_before, &buf), error!(EFAULT));
        assert_eq!(ma.read_memory_to_vec(partial_addr_before, buf.len()), error!(EFAULT));
        let partial_addr_after = addr + page_size / 2;
        assert_eq!(ma.write_memory(partial_addr_after, &buf), error!(EFAULT));
        assert_eq!(ma.read_memory_to_vec(partial_addr_after, buf.len()), error!(EFAULT));

        // Verify that accessing unmapped memory is an error.
        let unmapped_addr = addr - 10 * page_size;
        assert_eq!(ma.write_memory(unmapped_addr, &buf), error!(EFAULT));
        assert_eq!(ma.read_memory_to_vec(unmapped_addr, buf.len()), error!(EFAULT));

        // However, accessing zero bytes in unmapped memory is not an error.
        ma.write_memory(unmapped_addr, &[]).expect("failed to write no data");
        ma.read_memory_to_vec(unmapped_addr, 0).expect("failed to read no data");
    }

    #[::fuchsia::test]
    async fn test_read_c_string_to_vec_large() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        let page_size = *PAGE_SIZE;
        let max_size = 4 * page_size as usize;
        let addr = mm.base_addr + 10 * page_size;

        assert_eq!(map_memory(&mut locked, &current_task, addr, max_size as u64), addr);

        let mut random_data = vec![0; max_size];
        zx::cprng_draw(&mut random_data);
        // Remove all NUL bytes.
        for i in 0..random_data.len() {
            if random_data[i] == 0 {
                random_data[i] = 1;
            }
        }
        random_data[max_size - 1] = 0;

        ma.write_memory(addr, &random_data).expect("failed to write test string");
        // We should read the same value minus the last byte (NUL char).
        assert_eq!(
            ma.read_c_string_to_vec(UserCString::new(addr), max_size).unwrap(),
            random_data[..max_size - 1]
        );
    }

    #[::fuchsia::test]
    async fn test_read_c_string_to_vec() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        let page_size = *PAGE_SIZE;
        let max_size = 2 * page_size as usize;
        let addr = mm.base_addr + 10 * page_size;

        // Map a page at a fixed address and write an unterminated string at the end of it.
        assert_eq!(map_memory(&mut locked, &current_task, addr, page_size), addr);
        let test_str = b"foo!";
        let test_addr = addr + page_size - test_str.len();
        ma.write_memory(test_addr, test_str).expect("failed to write test string");

        // Expect error if the string is not terminated.
        assert_eq!(
            ma.read_c_string_to_vec(UserCString::new(test_addr), max_size),
            error!(ENAMETOOLONG)
        );

        // Expect success if the string is terminated.
        ma.write_memory(addr + (page_size - 1), b"\0").expect("failed to write nul");
        assert_eq!(ma.read_c_string_to_vec(UserCString::new(test_addr), max_size).unwrap(), "foo");

        // Expect success if the string spans over two mappings.
        assert_eq!(
            map_memory(&mut locked, &current_task, addr + page_size, page_size),
            addr + page_size
        );
        // TODO: Adjacent private anonymous mappings are collapsed. To test this case this test needs to
        // provide a backing for the second mapping.
        // assert_eq!(mm.get_mapping_count(), 2);
        ma.write_memory(addr + (page_size - 1), b"bar\0").expect("failed to write extra chars");
        assert_eq!(
            ma.read_c_string_to_vec(UserCString::new(test_addr), max_size).unwrap(),
            "foobar",
        );

        // Expect error if the string exceeds max limit
        assert_eq!(ma.read_c_string_to_vec(UserCString::new(test_addr), 2), error!(ENAMETOOLONG));

        // Expect error if the address is invalid.
        assert_eq!(ma.read_c_string_to_vec(UserCString::default(), max_size), error!(EFAULT));
    }

    #[::fuchsia::test]
    async fn can_read_argv_like_regions() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();

        // Map a page.
        let page_size = *PAGE_SIZE;
        let addr = map_memory_anywhere(&mut locked, &current_task, page_size);
        assert!(!addr.is_null());

        // Write an unterminated string.
        let mut payload = "first".as_bytes().to_vec();
        let mut expected_parses = vec![];
        ma.write_memory(addr, &payload).unwrap();

        // Expect success if the string is terminated.
        expected_parses.push(payload.clone());
        payload.push(0);
        ma.write_memory(addr, &payload).unwrap();
        assert_eq!(
            ma.read_nul_delimited_c_string_list(addr, payload.len()).unwrap(),
            expected_parses,
        );

        // Make sure we can parse multiple strings from the same region.
        let second = b"second";
        payload.extend(second);
        payload.push(0);
        expected_parses.push(second.to_vec());

        let third = b"third";
        payload.extend(third);
        payload.push(0);
        expected_parses.push(third.to_vec());

        ma.write_memory(addr, &payload).unwrap();
        assert_eq!(
            ma.read_nul_delimited_c_string_list(addr, payload.len()).unwrap(),
            expected_parses,
        );
    }

    #[::fuchsia::test]
    async fn truncate_argv_like_regions() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();

        // Map a page.
        let page_size = *PAGE_SIZE;
        let addr = map_memory_anywhere(&mut locked, &current_task, page_size);
        assert!(!addr.is_null());

        let payload = b"first\0second\0third\0";
        ma.write_memory(addr, payload).unwrap();
        assert_eq!(
            ma.read_nul_delimited_c_string_list(addr, payload.len() - 3).unwrap(),
            vec![b"first".to_vec(), b"second".to_vec(), b"thi".to_vec()],
            "Skipping last three bytes of payload should skip last two bytes of 3rd string"
        );
    }

    #[::fuchsia::test]
    async fn test_read_c_string() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        let page_size = *PAGE_SIZE;
        let buf_cap = 2 * page_size as usize;
        let mut buf = Vec::with_capacity(buf_cap);
        // We can't just use `spare_capacity_mut` because `Vec::with_capacity`
        // returns a `Vec` with _at least_ the requested capacity.
        let buf = &mut buf.spare_capacity_mut()[..buf_cap];
        let addr = mm.base_addr + 10 * page_size;

        // Map a page at a fixed address and write an unterminated string at the end of it.
        assert_eq!(map_memory(&mut locked, &current_task, addr, page_size), addr);
        let test_str = b"foo!";
        let test_addr = addr + page_size - test_str.len();
        ma.write_memory(test_addr, test_str).expect("failed to write test string");

        // Expect error if the string is not terminated.
        assert_eq!(ma.read_c_string(UserCString::new(test_addr), buf), error!(ENAMETOOLONG));

        // Expect success if the string is terminated.
        ma.write_memory(addr + (page_size - 1), b"\0").expect("failed to write nul");
        assert_eq!(ma.read_c_string(UserCString::new(test_addr), buf).unwrap(), "foo");

        // Expect success if the string spans over two mappings.
        assert_eq!(
            map_memory(&mut locked, &current_task, addr + page_size, page_size),
            addr + page_size
        );
        // TODO: To be multiple mappings we need to provide a file backing for the next page or the
        // mappings will be collapsed.
        //assert_eq!(mm.get_mapping_count(), 2);
        ma.write_memory(addr + (page_size - 1), b"bar\0").expect("failed to write extra chars");
        assert_eq!(ma.read_c_string(UserCString::new(test_addr), buf).unwrap(), "foobar");

        // Expect error if the string does not fit in the provided buffer.
        assert_eq!(
            ma.read_c_string(UserCString::new(test_addr), &mut [MaybeUninit::uninit(); 2]),
            error!(ENAMETOOLONG)
        );

        // Expect error if the address is invalid.
        assert_eq!(ma.read_c_string(UserCString::default(), buf), error!(EFAULT));
    }

    #[::fuchsia::test]
    async fn test_find_next_unused_range() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let mmap_top = mm.state.read().find_next_unused_range(0).unwrap().ptr();
        let page_size = *PAGE_SIZE as usize;
        assert!(mmap_top <= RESTRICTED_ASPACE_HIGHEST_ADDRESS);

        // No mappings - top address minus requested size is available
        assert_eq!(
            mm.state.read().find_next_unused_range(page_size).unwrap(),
            UserAddress::from_ptr(mmap_top - page_size)
        );

        // Fill it.
        let addr = UserAddress::from_ptr(mmap_top - page_size);
        assert_eq!(map_memory(&mut locked, &current_task, addr, *PAGE_SIZE), addr);

        // The next available range is right before the new mapping.
        assert_eq!(
            mm.state.read().find_next_unused_range(page_size).unwrap(),
            UserAddress::from_ptr(addr.ptr() - page_size)
        );

        // Allocate an extra page before a one-page gap.
        let addr2 = UserAddress::from_ptr(addr.ptr() - 2 * page_size);
        assert_eq!(map_memory(&mut locked, &current_task, addr2, *PAGE_SIZE), addr2);

        // Searching for one-page range still gives the same result
        assert_eq!(
            mm.state.read().find_next_unused_range(page_size).unwrap(),
            UserAddress::from_ptr(addr.ptr() - page_size)
        );

        // Searching for a bigger range results in the area before the second mapping
        assert_eq!(
            mm.state.read().find_next_unused_range(2 * page_size).unwrap(),
            UserAddress::from_ptr(addr2.ptr() - 2 * page_size)
        );

        // Searching for more memory than available should fail.
        assert_eq!(mm.state.read().find_next_unused_range(mmap_top), None);
    }

    #[::fuchsia::test]
    async fn test_count_placements() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        // ten-page range
        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        assert_eq!(
            mm.state.read().count_possible_placements(11 * page_size, &subrange_ten),
            Some(0)
        );
        assert_eq!(
            mm.state.read().count_possible_placements(10 * page_size, &subrange_ten),
            Some(1)
        );
        assert_eq!(
            mm.state.read().count_possible_placements(9 * page_size, &subrange_ten),
            Some(2)
        );
        assert_eq!(mm.state.read().count_possible_placements(page_size, &subrange_ten), Some(10));

        // map 6th page
        let addr = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 5 * page_size);
        assert_eq!(map_memory(&mut locked, &current_task, addr, *PAGE_SIZE), addr);

        assert_eq!(
            mm.state.read().count_possible_placements(10 * page_size, &subrange_ten),
            Some(0)
        );
        assert_eq!(
            mm.state.read().count_possible_placements(5 * page_size, &subrange_ten),
            Some(1)
        );
        assert_eq!(
            mm.state.read().count_possible_placements(4 * page_size, &subrange_ten),
            Some(3)
        );
        assert_eq!(mm.state.read().count_possible_placements(page_size, &subrange_ten), Some(9));
    }

    #[::fuchsia::test]
    async fn test_pick_placement() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        let addr = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 5 * page_size);
        assert_eq!(map_memory(&mut locked, &current_task, addr, *PAGE_SIZE), addr);
        assert_eq!(
            mm.state.read().count_possible_placements(4 * page_size, &subrange_ten),
            Some(3)
        );

        assert_eq!(
            mm.state.read().pick_placement(4 * page_size, 0, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE))
        );
        assert_eq!(
            mm.state.read().pick_placement(4 * page_size, 1, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + page_size))
        );
        assert_eq!(
            mm.state.read().pick_placement(4 * page_size, 2, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 6 * page_size))
        );
    }

    #[::fuchsia::test]
    async fn test_find_random_unused_range() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        // ten-page range
        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        for _ in 0..10 {
            let addr = mm.state.read().find_random_unused_range(page_size, &subrange_ten);
            assert!(addr.is_some());
            assert_eq!(
                map_memory(&mut locked, &current_task, addr.unwrap(), *PAGE_SIZE),
                addr.unwrap()
            );
        }
        assert_eq!(mm.state.read().find_random_unused_range(page_size, &subrange_ten), None);
    }

    #[::fuchsia::test]
    async fn test_grows_down_near_aspace_base() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let page_count = 10;

        let page_size = *PAGE_SIZE as usize;
        let addr = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE) + page_count * page_size;
        assert_eq!(
            map_memory_with_flags(
                &mut locked,
                &current_task,
                addr,
                page_size as u64,
                MAP_ANONYMOUS | MAP_PRIVATE | MAP_GROWSDOWN
            ),
            addr
        );

        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)..addr;
        assert_eq!(mm.state.read().find_random_unused_range(page_size, &subrange_ten), None);
    }

    #[::fuchsia::test]
    async fn test_unmap_returned_mappings() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let mut released_mappings = vec![];
        let unmap_result =
            mm.state.write().unmap(mm, addr, *PAGE_SIZE as usize, &mut released_mappings);
        assert!(unmap_result.is_ok());
        assert_eq!(released_mappings.len(), 1);
    }

    #[::fuchsia::test]
    async fn test_unmap_returns_multiple_mappings() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = mm.state.read().find_next_unused_range(3 * *PAGE_SIZE as usize).unwrap();
        let addr = map_memory(&mut locked, &current_task, addr, *PAGE_SIZE);
        let _ = map_memory(&mut locked, &current_task, addr + 2 * *PAGE_SIZE, *PAGE_SIZE);

        let mut released_mappings = vec![];
        let unmap_result =
            mm.state.write().unmap(mm, addr, (*PAGE_SIZE * 3) as usize, &mut released_mappings);
        assert!(unmap_result.is_ok());
        assert_eq!(released_mappings.len(), 2);
    }

    /// Maps two pages, then unmaps the first page.
    /// The second page should be re-mapped with a new child COW memory object.
    #[::fuchsia::test]
    async fn test_unmap_beginning() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let state = mm.state.read();
        let (range, mapping) = state.mappings.get(&addr).expect("mapping");
        assert_eq!(range.start, addr);
        assert_eq!(range.end, addr + (*PAGE_SIZE * 2));
        #[cfg(feature = "alternate_anon_allocs")]
        let _ = mapping;
        #[cfg(not(feature = "alternate_anon_allocs"))]
        let original_memory = {
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE * 2);
                    backing.memory.clone()
                }
            }
        };
        std::mem::drop(state);

        assert_eq!(mm.unmap(addr, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The first page should be unmapped.
            assert!(state.mappings.get(&addr).is_none());

            // The second page should be a new child COW memory object.
            let (range, mapping) = state.mappings.get(&(addr + *PAGE_SIZE)).expect("second page");
            assert_eq!(range.start, addr + *PAGE_SIZE);
            assert_eq!(range.end, addr + *PAGE_SIZE * 2);
            #[cfg(feature = "alternate_anon_allocs")]
            let _ = mapping;
            #[cfg(not(feature = "alternate_anon_allocs"))]
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr + *PAGE_SIZE);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                    assert_ne!(original_memory.get_koid(), backing.memory.get_koid());
                }
            }
        }
    }

    /// Maps two pages, then unmaps the second page.
    /// The first page's memory object should be shrunk.
    #[::fuchsia::test]
    async fn test_unmap_end() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let state = mm.state.read();
        let (range, mapping) = state.mappings.get(&addr).expect("mapping");
        assert_eq!(range.start, addr);
        assert_eq!(range.end, addr + (*PAGE_SIZE * 2));
        #[cfg(feature = "alternate_anon_allocs")]
        let _ = mapping;
        #[cfg(not(feature = "alternate_anon_allocs"))]
        let original_memory = {
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE * 2);
                    backing.memory.clone()
                }
            }
        };
        std::mem::drop(state);

        assert_eq!(mm.unmap(addr + *PAGE_SIZE, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The second page should be unmapped.
            assert!(state.mappings.get(&(addr + *PAGE_SIZE)).is_none());

            // The first page's memory object should be the same as the original, only shrunk.
            let (range, mapping) = state.mappings.get(&addr).expect("first page");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + *PAGE_SIZE);
            #[cfg(feature = "alternate_anon_allocs")]
            let _ = mapping;
            #[cfg(not(feature = "alternate_anon_allocs"))]
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                    assert_eq!(original_memory.get_koid(), backing.memory.get_koid());
                }
            }
        }
    }

    /// Maps two pages in separate mappings next to each other, then unmaps the first page.
    /// The second page should not be modified.
    #[::fuchsia::test]
    async fn test_map_two_unmap_one() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        // reserve memory for both pages
        let addr_reserve =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE * 2);
        let addr1 = map_memory_with_flags(
            &mut locked,
            &current_task,
            addr_reserve,
            *PAGE_SIZE,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_FIXED,
        );
        let addr2 = map_memory_with_flags(
            &mut locked,
            &current_task,
            addr_reserve + *PAGE_SIZE,
            *PAGE_SIZE,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_FIXED,
        );
        let state = mm.state.read();
        let (range1, _) = state.mappings.get(&addr1).expect("mapping");
        assert_eq!(range1.start, addr1);
        assert_eq!(range1.end, addr1 + *PAGE_SIZE);
        let (range2, mapping2) = state.mappings.get(&addr2).expect("mapping");
        assert_eq!(range2.start, addr2);
        assert_eq!(range2.end, addr2 + *PAGE_SIZE);
        #[cfg(feature = "alternate_anon_allocs")]
        let _ = mapping2;
        #[cfg(not(feature = "alternate_anon_allocs"))]
        let original_memory2 = {
            match &mapping2.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr2);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                    backing.memory.clone()
                }
            }
        };
        std::mem::drop(state);

        assert_eq!(mm.unmap(addr1, *PAGE_SIZE as usize), Ok(()));

        let state = mm.state.read();

        // The first page should be unmapped.
        assert!(state.mappings.get(&addr1).is_none());

        // The second page should remain unchanged.
        let (range2, mapping2) = state.mappings.get(&addr2).expect("second page");
        assert_eq!(range2.start, addr2);
        assert_eq!(range2.end, addr2 + *PAGE_SIZE);
        #[cfg(feature = "alternate_anon_allocs")]
        let _ = mapping2;
        #[cfg(not(feature = "alternate_anon_allocs"))]
        match &mapping2.backing {
            MappingBacking::Memory(backing) => {
                assert_eq!(backing.base, addr2);
                assert_eq!(backing.memory_offset, 0);
                assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                assert_eq!(original_memory2.get_koid(), backing.memory.get_koid());
            }
        }
    }

    /// Maps three pages, then unmaps the middle page.
    /// The last page should be re-mapped with a new COW child memory object.
    /// The first page's memory object should be shrunk,
    #[::fuchsia::test]
    async fn test_unmap_middle() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE * 3);

        let state = mm.state.read();
        let (range, mapping) = state.mappings.get(&addr).expect("mapping");
        assert_eq!(range.start, addr);
        assert_eq!(range.end, addr + (*PAGE_SIZE * 3));
        #[cfg(feature = "alternate_anon_allocs")]
        let _ = mapping;
        #[cfg(not(feature = "alternate_anon_allocs"))]
        let original_memory = {
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE * 3);
                    backing.memory.clone()
                }
            }
        };
        std::mem::drop(state);

        assert_eq!(mm.unmap(addr + *PAGE_SIZE, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The middle page should be unmapped.
            assert!(state.mappings.get(&(addr + *PAGE_SIZE)).is_none());

            let (range, mapping) = state.mappings.get(&addr).expect("first page");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + *PAGE_SIZE);
            #[cfg(feature = "alternate_anon_allocs")]
            let _ = mapping;
            #[cfg(not(feature = "alternate_anon_allocs"))]
            // The first page's memory object should be the same as the original, only shrunk.
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                    assert_eq!(original_memory.get_koid(), backing.memory.get_koid());
                }
            }

            let (range, mapping) = state.mappings.get(&(addr + *PAGE_SIZE * 2)).expect("last page");
            assert_eq!(range.start, addr + *PAGE_SIZE * 2);
            assert_eq!(range.end, addr + *PAGE_SIZE * 3);
            #[cfg(feature = "alternate_anon_allocs")]
            let _ = mapping;
            #[cfg(not(feature = "alternate_anon_allocs"))]
            // The last page should be a new child COW memory object.
            match &mapping.backing {
                MappingBacking::Memory(backing) => {
                    assert_eq!(backing.base, addr + *PAGE_SIZE * 2);
                    assert_eq!(backing.memory_offset, 0);
                    assert_eq!(backing.memory.get_size(), *PAGE_SIZE);
                    assert_ne!(original_memory.get_koid(), backing.memory.get_koid());
                }
            }
        }
    }

    #[::fuchsia::test]
    async fn test_read_write_objects() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let items_ref = UserRef::<i32>::new(addr);

        let items_written = vec![0, 2, 3, 7, 1];
        ma.write_objects(items_ref, &items_written).expect("Failed to write object array.");

        let items_read = ma
            .read_objects_to_vec(items_ref, items_written.len())
            .expect("Failed to read object array.");

        assert_eq!(items_written, items_read);
    }

    #[::fuchsia::test]
    async fn test_read_write_objects_null() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();
        let items_ref = UserRef::<i32>::new(UserAddress::default());

        let items_written = vec![];
        ma.write_objects(items_ref, &items_written).expect("Failed to write empty object array.");

        let items_read = ma
            .read_objects_to_vec(items_ref, items_written.len())
            .expect("Failed to read empty object array.");

        assert_eq!(items_written, items_read);
    }

    #[::fuchsia::test]
    async fn test_read_object_partial() {
        #[derive(Debug, Default, Copy, Clone, KnownLayout, FromBytes, Immutable, PartialEq)]
        struct Items {
            val: [i32; 4],
        }

        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let ma = current_task.deref();
        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        let items_array_ref = UserRef::<i32>::new(addr);

        // Populate some values.
        let items_written = vec![75, 23, 51, 98];
        ma.write_objects(items_array_ref, &items_written).expect("Failed to write object array.");

        // Full read of all 4 values.
        let items_ref = UserRef::<Items>::new(addr);
        let items_read = ma
            .read_object_partial(items_ref, std::mem::size_of::<Items>())
            .expect("Failed to read object");
        assert_eq!(items_written, items_read.val);

        // Partial read of the first two.
        let items_read = ma.read_object_partial(items_ref, 8).expect("Failed to read object");
        assert_eq!(vec![75, 23, 0, 0], items_read.val);

        // The API currently allows reading 0 bytes (this could be re-evaluated) so test that does
        // the right thing.
        let items_read = ma.read_object_partial(items_ref, 0).expect("Failed to read object");
        assert_eq!(vec![0, 0, 0, 0], items_read.val);

        // Size bigger than the object.
        assert_eq!(
            ma.read_object_partial(items_ref, std::mem::size_of::<Items>() + 8),
            error!(EINVAL)
        );

        // Bad pointer.
        assert_eq!(
            ma.read_object_partial(UserRef::<Items>::new(UserAddress::from(1)), 16),
            error!(EFAULT)
        );
    }

    #[::fuchsia::test]
    async fn test_partial_read() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        let addr = mm.state.read().find_next_unused_range(2 * *PAGE_SIZE as usize).unwrap();
        let addr = map_memory(&mut locked, &current_task, addr, *PAGE_SIZE);
        let second_map = map_memory(&mut locked, &current_task, addr + *PAGE_SIZE, *PAGE_SIZE);

        let bytes = vec![0xf; (*PAGE_SIZE * 2) as usize];
        assert!(ma.write_memory(addr, &bytes).is_ok());
        mm.state
            .write()
            .protect(second_map, *PAGE_SIZE as usize, ProtectionFlags::empty())
            .unwrap();
        assert_eq!(
            ma.read_memory_partial_to_vec(addr, bytes.len()).unwrap().len(),
            *PAGE_SIZE as usize,
        );
    }

    fn map_memory_growsdown<L>(
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: u64,
    ) -> UserAddress
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        map_memory_with_flags(
            locked,
            current_task,
            UserAddress::default(),
            length,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_GROWSDOWN,
        )
    }

    #[::fuchsia::test]
    async fn test_grow_mapping_empty_mm() {
        let (_kernel, current_task, _) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = UserAddress::from(0x100000);

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_inside_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_write_fault_inside_read_only_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = do_mmap(
            &mut locked,
            &current_task,
            UserAddress::default(),
            *PAGE_SIZE as usize,
            PROT_READ,
            MAP_ANONYMOUS | MAP_PRIVATE,
            FdNumber::from_raw(-1),
            0,
        )
        .expect("Could not map memory");

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, true), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_fault_inside_prot_none_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = do_mmap(
            &mut locked,
            &current_task,
            UserAddress::default(),
            *PAGE_SIZE as usize,
            PROT_NONE,
            MAP_ANONYMOUS | MAP_PRIVATE,
            FdNumber::from_raw(-1),
            0,
        )
        .expect("Could not map memory");

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, true), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_below_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory_growsdown(&mut locked, &current_task, *PAGE_SIZE) - *PAGE_SIZE;

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(true));

        // Should see two mappings
        assert_eq!(mm.get_mapping_count(), 2);
    }

    #[::fuchsia::test]
    async fn test_grow_above_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let addr = map_memory_growsdown(&mut locked, &current_task, *PAGE_SIZE) + *PAGE_SIZE;

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_write_fault_below_read_only_mapping() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();

        let mapped_addr = map_memory_growsdown(&mut locked, &current_task, *PAGE_SIZE);

        mm.protect(mapped_addr, *PAGE_SIZE as usize, ProtectionFlags::READ).unwrap();

        assert_matches!(
            mm.extend_growsdown_mapping_to_address(mapped_addr - *PAGE_SIZE, true),
            Ok(false)
        );

        assert_eq!(mm.get_mapping_count(), 1);
    }

    #[::fuchsia::test]
    async fn test_snapshot_paged_memory() {
        use zx::sys::zx_page_request_command_t::ZX_PAGER_VMO_READ;

        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm();
        let ma = current_task.deref();

        let port = Arc::new(zx::Port::create());
        let port_clone = port.clone();
        let pager = Arc::new(zx::Pager::create(zx::PagerOptions::empty()).expect("create failed"));
        let pager_clone = pager.clone();

        const VMO_SIZE: u64 = 128 * 1024;
        let vmo = Arc::new(
            pager
                .create_vmo(zx::VmoOptions::RESIZABLE, &port, 1, VMO_SIZE)
                .expect("create_vmo failed"),
        );
        let vmo_clone = vmo.clone();

        // Create a thread to service the port where we will receive pager requests.
        let thread = std::thread::spawn(move || loop {
            let packet = port_clone.wait(zx::MonotonicInstant::INFINITE).expect("wait failed");
            match packet.contents() {
                zx::PacketContents::Pager(contents) => {
                    if contents.command() == ZX_PAGER_VMO_READ {
                        let range = contents.range();
                        let source_vmo =
                            zx::Vmo::create(range.end - range.start).expect("create failed");
                        pager_clone
                            .supply_pages(&vmo_clone, range, &source_vmo, 0)
                            .expect("supply_pages failed");
                    }
                }
                zx::PacketContents::User(_) => break,
                _ => {}
            }
        });

        let child_vmo =
            vmo.create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, VMO_SIZE).unwrap();

        // Write something to the source VMO.
        vmo.write(b"foo", 0).expect("write failed");

        let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
        let addr = mm
            .map_memory(
                DesiredAddress::Any,
                Arc::new(MemoryObject::from(child_vmo)),
                0,
                VMO_SIZE as usize,
                prot_flags,
                Access::rwx(),
                MappingOptions::empty(),
                MappingName::None,
                FileWriteGuardRef(None),
            )
            .expect("map failed");

        let target = create_task(&mut locked, &kernel, "another-task");
        mm.snapshot_to(&mut locked, target.mm()).expect("snapshot_to failed");

        // Make sure it has what we wrote.
        let buf = target.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"foo");

        // Write something to both source and target and make sure they are forked.
        ma.write_memory(addr, b"bar").expect("write_memory failed");

        let buf = target.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"foo");

        target.write_memory(addr, b"baz").expect("write_memory failed");
        let buf = ma.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"bar");

        let buf = target.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"baz");

        port.queue(&zx::Packet::from_user_packet(0, 0, zx::UserPacket::from_u8_array([0; 32])))
            .unwrap();
        thread.join().unwrap();
    }

    #[::fuchsia::test]
    async fn test_set_vma_name() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        let vma_name = "vma name";
        current_task.write_memory(name_addr, vma_name.as_bytes()).unwrap();

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        sys_prctl(
            &mut locked,
            &mut current_task,
            PR_SET_VMA,
            PR_SET_VMA_ANON_NAME as u64,
            mapping_addr.ptr() as u64,
            *PAGE_SIZE,
            name_addr.ptr() as u64,
        )
        .unwrap();

        assert_eq!(current_task.mm().get_mapping_name(mapping_addr).unwrap().unwrap(), vma_name);
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_adjacent_mappings() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let first_mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), 2 * *PAGE_SIZE);
        let second_mapping_addr = map_memory_with_flags(
            &mut locked,
            &current_task,
            first_mapping_addr + *PAGE_SIZE,
            *PAGE_SIZE,
            MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS,
        );

        assert_eq!(first_mapping_addr + *PAGE_SIZE, second_mapping_addr);

        sys_prctl(
            &mut locked,
            &mut current_task,
            PR_SET_VMA,
            PR_SET_VMA_ANON_NAME as u64,
            first_mapping_addr.ptr() as u64,
            2 * *PAGE_SIZE,
            name_addr.ptr() as u64,
        )
        .unwrap();

        {
            let state = current_task.mm().state.read();

            // The name should apply to both mappings.
            let (_, mapping) = state.mappings.get(&first_mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));

            let (_, mapping) = state.mappings.get(&second_mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_beyond_end() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), 2 * *PAGE_SIZE);

        let second_page = mapping_addr + *PAGE_SIZE;
        current_task.mm().unmap(second_page, *PAGE_SIZE as usize).unwrap();

        // This should fail with ENOMEM since it extends past the end of the mapping into unmapped memory.
        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                2 * *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Err(errno!(ENOMEM))
        );

        // Despite returning an error, the prctl should still assign a name to the region at the start of the region.
        {
            let state = current_task.mm().state.read();

            let (_, mapping) = state.mappings.get(&mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_before_start() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), 2 * *PAGE_SIZE);

        let second_page = mapping_addr + *PAGE_SIZE;
        current_task.mm().unmap(mapping_addr, *PAGE_SIZE as usize).unwrap();

        // This should fail with ENOMEM since the start of the range is in unmapped memory.
        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                2 * *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Err(errno!(ENOMEM))
        );

        // Unlike a range which starts within a mapping and extends past the end, this should not assign
        // a name to any mappings.
        {
            let state = current_task.mm().state.read();

            let (_, mapping) = state.mappings.get(&second_page).unwrap();
            assert_eq!(mapping.name, MappingName::None);
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_partial() {
        let (_kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), 3 * *PAGE_SIZE);

        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                (mapping_addr + *PAGE_SIZE).ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Ok(starnix_syscalls::SUCCESS)
        );

        // This should split the mapping into 3 pieces with the second piece having the name "foo"
        {
            let state = current_task.mm().state.read();

            let (_, mapping) = state.mappings.get(&mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::None);

            let (_, mapping) = state.mappings.get(&(mapping_addr + *PAGE_SIZE)).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));

            let (_, mapping) = state.mappings.get(&(mapping_addr + 2 * *PAGE_SIZE)).unwrap();
            assert_eq!(mapping.name, MappingName::None);
        }
    }

    #[::fuchsia::test]
    async fn test_preserve_name_snapshot() {
        let (kernel, mut current_task, mut locked) = create_kernel_task_and_unlocked();

        let name_addr = map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr =
            map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);

        assert_eq!(
            sys_prctl(
                &mut locked,
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Ok(starnix_syscalls::SUCCESS)
        );

        let target = create_task(&mut locked, &kernel, "another-task");
        current_task.mm().snapshot_to(&mut locked, target.mm()).expect("snapshot_to failed");

        {
            let state = target.mm().state.read();

            let (_, mapping) = state.mappings.get(&mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }
}
