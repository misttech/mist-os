// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::memory::MemoryObject;
use crate::mm::{
    FaultRegisterMode, MappingOptions, ProtectionFlags, UserFault,
    GUARD_PAGE_COUNT_FOR_GROWSDOWN_MAPPINGS, PAGE_SIZE,
};
use crate::vfs::aio::AioContext;
use crate::vfs::{ActiveNamespaceNode, FileWriteGuardRef, FsString};
use bitflags::bitflags;
use fuchsia_inspect::HistogramProperty;
use fuchsia_inspect_contrib::profile_duration;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::Access;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{errno, PROT_EXEC, PROT_READ, PROT_WRITE};
use static_assertions::const_assert_eq;
use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::{Arc, Weak};

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Mapping {
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

    /// If the mapping is registered with a userfaultfd, this field contains information about
    /// the userfault object associated with it and the fault handling mode.
    userfault: Option<Box<UserFaultRegistration>>,

    /// Lock guard held to prevent this file from being written while it's being executed.
    file_write_guard: FileWriteGuardRef,
}

impl Mapping {
    pub fn new(
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

    pub fn with_name(
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
            userfault: None,
            file_write_guard,
        }
    }

    pub fn name(&self) -> &MappingName {
        &self.name
    }

    pub fn set_name(&mut self, new_name: MappingName) {
        self.name = new_name;
    }

    pub fn flags(&self) -> MappingFlags {
        self.flags
    }

    pub fn set_flags(&mut self, new_flags: MappingFlags) {
        self.flags = new_flags;
    }

    pub fn max_access(&self) -> Access {
        self.max_access
    }

    pub fn backing(&self) -> &MappingBacking {
        &self.backing
    }

    pub fn set_backing(&mut self, backing: MappingBacking) {
        self.backing = backing;
    }

    pub fn file_write_guard(&self) -> &FileWriteGuardRef {
        &self.file_write_guard
    }

    pub fn userfault(&self) -> Option<&Box<UserFaultRegistration>> {
        self.userfault.as_ref()
    }

    pub fn set_userfault(&mut self, new_userfault: Box<UserFaultRegistration>) {
        self.userfault = Some(new_userfault);
    }

    pub fn clear_userfault(&mut self) {
        self.userfault = None;
    }

    #[cfg(feature = "alternate_anon_allocs")]
    pub fn new_private_anonymous(flags: MappingFlags, name: MappingName) -> Mapping {
        Mapping {
            backing: MappingBacking::PrivateAnonymous,
            flags,
            max_access: Access::rwx(),
            name,
            userfault: None,
            file_write_guard: FileWriteGuardRef(None),
        }
    }

    pub fn inflate_to_include_guard_pages(&self, range: &Range<UserAddress>) -> Range<UserAddress> {
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
    pub fn address_to_offset(&self, addr: UserAddress) -> u64 {
        match &self.backing {
            MappingBacking::Memory(backing) => backing.address_to_offset(addr),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                // For private, anonymous allocations the virtual address is the offset in the backing memory object.
                addr.ptr() as u64
            }
        }
    }

    pub fn can_read(&self) -> bool {
        self.flags.contains(MappingFlags::READ)
    }

    pub fn can_write(&self) -> bool {
        self.flags.contains(MappingFlags::WRITE)
    }

    pub fn can_exec(&self) -> bool {
        self.flags.contains(MappingFlags::EXEC)
    }

    pub fn private_anonymous(&self) -> bool {
        #[cfg(feature = "alternate_anon_allocs")]
        if let MappingBacking::PrivateAnonymous = &self.backing {
            return true;
        }
        !self.flags.contains(MappingFlags::SHARED) && self.flags.contains(MappingFlags::ANONYMOUS)
    }

    pub fn vm_flags(&self) -> String {
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
        if let Some(userfault) = &self.userfault {
            if userfault.mode == FaultRegisterMode::MISSING {
                string.push_str("um");
            }
        }
        // uw   -   userfaultfd wprotect pages tracking (since Linux 4.3)
        // ui   -   userfaultfd minor fault pages tracking (since Linux 5.13)
        string
    }

    pub fn split_prefix_off(&mut self, start: UserAddress, prefix_len: u64) -> Self {
        match &mut self.backing {
            MappingBacking::Memory(backing) => {
                // Shrink the range of the named mapping to only the named area.
                backing.memory_offset = prefix_len;
                Mapping::new(
                    start,
                    backing.memory.clone(),
                    backing.memory_offset,
                    self.flags,
                    self.max_access,
                    self.file_write_guard.clone(),
                )
            }
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => {
                Mapping::new_private_anonymous(self.flags, self.name.clone())
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum MappingBacking {
    Memory(MappingBackingMemory),

    #[cfg(feature = "alternate_anon_allocs")]
    PrivateAnonymous,
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
pub struct MappingBackingMemory {
    /// The base address of this mapping.
    ///
    /// Keep in mind that the mapping might be trimmed in the RangeMap if the
    /// part of the mapping is unmapped, which means the base might extend
    /// before the currently valid portion of the mapping.
    pub base: UserAddress,

    /// The memory object that contains the memory used in this mapping.
    pub memory: Arc<MemoryObject>,

    /// The offset in the memory object that corresponds to the base address.
    pub memory_offset: u64,
}

impl MappingBackingMemory {
    /// Reads exactly `bytes.len()` bytes of memory from `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    pub fn read_memory<'a>(
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
    pub fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<(), Errno> {
        self.memory.write(bytes, self.address_to_offset(addr)).map_err(|_| errno!(EFAULT))
    }

    pub fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.memory
            .op_range(zx::VmoOp::ZERO, self.address_to_offset(addr), length as u64)
            .map_err(|_| errno!(EFAULT))?;
        Ok(length)
    }

    /// Converts a `UserAddress` to an offset in this mapping's memory object.
    pub fn address_to_offset(&self, addr: UserAddress) -> u64 {
        (addr.ptr() - self.base.ptr()) as u64 + self.memory_offset
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[rustfmt::skip]  // Preserve column alignment.
    pub struct MappingFlags: u16 {
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
const_assert_eq!(MappingFlags::READ.bits(), PROT_READ as u16);
const_assert_eq!(MappingFlags::WRITE.bits(), PROT_WRITE as u16);
const_assert_eq!(MappingFlags::EXEC.bits(), PROT_EXEC as u16);

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
    pub fn access_flags(&self) -> ProtectionFlags {
        ProtectionFlags::from_bits_truncate(
            self.bits() as u32 & ProtectionFlags::ACCESS_FLAGS.bits(),
        )
    }

    pub fn with_access_flags(&self, prot_flags: ProtectionFlags) -> Self {
        let mapping_flags =
            *self & (MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXEC).complement();
        mapping_flags | Self::from_bits_truncate(prot_flags.access_flags().bits() as u16)
    }

    #[cfg(any(feature = "alternate_anon_allocs", test))]
    pub fn options(&self) -> MappingOptions {
        MappingOptions::from_bits_truncate(self.bits() >> 3)
    }

    pub fn from_access_flags_and_options(
        prot_flags: ProtectionFlags,
        options: MappingOptions,
    ) -> Self {
        Self::from_bits_truncate(prot_flags.access_flags().bits() as u16)
            | Self::from_bits_truncate(options.bits() << 3)
    }
}

#[derive(Debug, Clone)]
pub struct UserFaultRegistration {
    pub userfault: Weak<UserFault>,
    pub mode: FaultRegisterMode,
}

impl UserFaultRegistration {
    pub fn new(userfault: Weak<UserFault>, mode: FaultRegisterMode) -> Self {
        Self { userfault, mode }
    }
}

impl PartialEq for UserFaultRegistration {
    fn eq(&self, other: &Self) -> bool {
        (self.mode == other.mode) && self.userfault.ptr_eq(&other.userfault)
    }
}

impl Eq for UserFaultRegistration {}

#[derive(Debug, Default)]
pub struct MappingSummary {
    no_kind: MappingKindSummary,
    stack: MappingKindSummary,
    heap: MappingKindSummary,
    vdso: MappingKindSummary,
    vvar: MappingKindSummary,
    file: MappingKindSummary,
    vma: MappingKindSummary,
    ashmem: MappingKindSummary,
    aiocontext: MappingKindSummary,

    name_lengths: Vec<usize>,
}

impl MappingSummary {
    pub fn add(&mut self, mapping: &Mapping) {
        let kind_summary = match &mapping.name {
            MappingName::None => &mut self.no_kind,
            MappingName::Stack => &mut self.stack,
            MappingName::Heap => &mut self.heap,
            MappingName::Vdso => &mut self.vdso,
            MappingName::Vvar => &mut self.vvar,
            MappingName::File(_) => &mut self.file,
            MappingName::Vma(name) => {
                self.name_lengths.push(name.len());
                &mut self.vma
            }
            MappingName::Ashmem(name) => {
                self.name_lengths.push(name.len());
                &mut self.ashmem
            }
            MappingName::AioContext(_) => &mut self.aiocontext,
        };

        kind_summary.count += 1;
        if mapping.flags.contains(MappingFlags::SHARED) {
            kind_summary.num_shared += 1;
        } else {
            kind_summary.num_private += 1;
        }
        if mapping.file_write_guard().0.is_some() {
            kind_summary.num_file_write_guards += 1;
        }
        match &mapping.backing {
            MappingBacking::Memory(m) => {
                kind_summary.num_memory_objects += 1;
                if m.memory_offset != 0 {
                    kind_summary.num_non_zero_memory_offset += 1;
                }
            }
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => kind_summary.num_private_anon += 1,
        }
    }

    pub fn record(self, node: &fuchsia_inspect::Node) {
        node.record_child("no_kind", |node| self.no_kind.record(node));
        node.record_child("stack", |node| self.stack.record(node));
        node.record_child("heap", |node| self.heap.record(node));
        node.record_child("vdso", |node| self.vdso.record(node));
        node.record_child("vvar", |node| self.vvar.record(node));
        node.record_child("file", |node| self.file.record(node));
        node.record_child("vma", |node| self.vma.record(node));
        node.record_child("ashmem", |node| self.ashmem.record(node));
        node.record_child("aiocontext", |node| self.aiocontext.record(node));

        let name_lengths = node.create_uint_linear_histogram(
            "name_lengths",
            fuchsia_inspect::LinearHistogramParams { floor: 0, step_size: 8, buckets: 4 },
        );
        for l in self.name_lengths {
            name_lengths.insert(l as u64);
        }
        node.record(name_lengths);
    }
}

#[derive(Debug, Default)]
struct MappingKindSummary {
    count: u64,
    num_private: u64,
    num_shared: u64,
    num_non_zero_memory_offset: u64,
    num_file_write_guards: u64,
    num_memory_objects: u64,
    #[cfg(feature = "alternate_anon_allocs")]
    num_private_anon: u64,
}

impl MappingKindSummary {
    fn record(&self, node: &fuchsia_inspect::Node) {
        node.record_uint("count", self.count);
        node.record_uint("num_private", self.num_private);
        node.record_uint("num_shared", self.num_shared);
        node.record_uint("num_non_zero_memory_offset", self.num_non_zero_memory_offset);
        node.record_uint("num_file_write_guards", self.num_file_write_guards);
        node.record_uint("num_memory_objects", self.num_memory_objects);
        #[cfg(feature = "alternate_anon_allocs")]
        node.record_uint("num_private_anon", self.num_private_anon);
    }
}
