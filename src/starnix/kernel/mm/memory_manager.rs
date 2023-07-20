// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use bitflags::bitflags;
use fuchsia_zircon::{self as zx, AsHandleRef};
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    convert::TryInto,
    ffi::CStr,
    ops::Range,
    sync::{Arc, Weak},
};
use zerocopy::{AsBytes, FromBytes};

use crate::{
    collections::*,
    fs::*,
    lock::{Mutex, RwLock},
    logging::*,
    mm::{vmo::round_up_to_system_page_size, FutexTable},
    task::*,
    types::{range_ext::RangeExt, *},
    vmex_resource::VMEX_RESOURCE,
};

pub static PAGE_SIZE: Lazy<u64> = Lazy::new(|| zx::system_get_page_size() as u64);

bitflags! {
    pub struct MappingOptions: u32 {
      const SHARED = 1;
      const ANONYMOUS = 2;
      const LOWER_32BIT = 4;
      const GROWSDOWN = 8;
      const ELF_BINARY = 16;
    }
}

bitflags! {
    pub struct ProtectionFlags: u32 {
      const READ = PROT_READ;
      const WRITE = PROT_WRITE;
      const EXEC = PROT_EXEC;
    }
}

impl ProtectionFlags {
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
}

bitflags! {
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

    /// This ampping is the vdso.
    Vdso,

    /// The file backing this mapping.
    File(NamespaceNode),

    /// The name associated with the mapping. Set by prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
    /// An empty name is distinct from an unnamed mapping. Mappings are initially created with no
    /// name and can be reset to the unnamed state by passing NULL to
    /// prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, ...).
    Vma(FsString),
}

#[derive(Debug, Eq, PartialEq, Clone)]
struct Mapping {
    /// The base address of this mapping.
    ///
    /// Keep in mind that the mapping might be trimmed in the RangeMap if the
    /// part of the mapping is unmapped, which means the base might extend
    /// before the currently valid portion of the mapping.
    base: UserAddress,

    /// The VMO that contains the memory used in this mapping.
    vmo: Arc<zx::Vmo>,

    /// The offset in the VMO that corresponds to the base address.
    vmo_offset: u64,

    /// The protection flags used by the mapping.
    prot_flags: ProtectionFlags,

    /// The flags for this mapping.
    options: MappingOptions,

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
        vmo: Arc<zx::Vmo>,
        vmo_offset: u64,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        file_write_guard: FileWriteGuardRef,
    ) -> Mapping {
        Mapping {
            base,
            vmo,
            vmo_offset,
            prot_flags,
            options,
            name: MappingName::None,
            file_write_guard,
        }
    }

    /// Converts a `UserAddress` to an offset in this mapping's VMO.
    fn address_to_offset(&self, addr: UserAddress) -> u64 {
        (addr.ptr() - self.base.ptr()) as u64 + self.vmo_offset
    }

    /// Reads exactly `bytes.len()` bytes of memory from `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    fn read_memory(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        if !self.prot_flags.contains(ProtectionFlags::READ) {
            return error!(EFAULT);
        }
        self.vmo.read(bytes, self.address_to_offset(addr)).map_err(|e| {
            log_warn!("Got an error when reading from vmo: {:?}", e);
            errno!(EFAULT)
        })
    }

    /// Writes the provided bytes to `addr`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write to the VMO.
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<(), Errno> {
        if !self.prot_flags.contains(ProtectionFlags::WRITE) {
            return error!(EFAULT);
        }
        self.vmo.write(bytes, self.address_to_offset(addr)).map_err(|_| errno!(EFAULT))
    }
}

const PROGRAM_BREAK_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct ProgramBreak {
    // These base address at which the data segment is mapped.
    base: UserAddress,

    // The current program break.
    //
    // The addresses from [base, current.round_up(*PAGE_SIZE)) are mapped into the
    // client address space from the underlying |vmo|.
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
    user_vmar: zx::Vmar,

    /// Cached VmarInfo for user_vmar.
    user_vmar_info: zx::VmarInfo,

    /// State for the brk and sbrk syscalls.
    brk: Option<ProgramBreak>,

    /// The memory mappings currently used by this address space.
    ///
    /// The mappings record which VMO backs each address.
    mappings: RangeMap<UserAddress, Mapping>,

    /// The namespace node that represents the executable associated with this task.
    executable_node: Option<NamespaceNode>,

    /// Stack location and size
    pub stack_base: UserAddress,
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
}

fn map_in_vmar(
    vmar: &zx::Vmar,
    vmar_info: &zx::VmarInfo,
    addr: DesiredAddress,
    vmo: &zx::Vmo,
    vmo_offset: u64,
    length: usize,
    prot_flags: ProtectionFlags,
    options: MappingOptions,
) -> Result<UserAddress, Errno> {
    let base_addr = UserAddress::from_ptr(vmar_info.base);
    let (vmar_offset, vmar_extra_flags) = match addr {
        DesiredAddress::Any if options.contains(MappingOptions::LOWER_32BIT) => {
            // MAP_32BIT specifies that the memory allocated will
            // be within the first 2 GB of the process address space.
            (0x80000000 - base_addr.ptr(), zx::VmarFlags::OFFSET_IS_UPPER_LIMIT)
        }
        DesiredAddress::Any => (0, zx::VmarFlags::empty()),
        DesiredAddress::Hint(addr) | DesiredAddress::Fixed(addr) => {
            (addr - base_addr, zx::VmarFlags::SPECIFIC)
        }
        DesiredAddress::FixedOverwrite(addr) => {
            let specific_overwrite = unsafe {
                zx::VmarFlags::from_bits_unchecked(zx::VmarFlagsExtended::SPECIFIC_OVERWRITE.bits())
            };
            (addr - base_addr, specific_overwrite)
        }
    };

    let vmar_flags = prot_flags.to_vmar_flags() | zx::VmarFlags::ALLOW_FAULTS | vmar_extra_flags;

    let mut map_result = vmar.map(vmar_offset, vmo, vmo_offset, length, vmar_flags);

    // Retry mapping if the target address was a Hint.
    if map_result.is_err() {
        if let DesiredAddress::Hint(_) = addr {
            let vmar_flags = vmar_flags - zx::VmarFlags::SPECIFIC;
            map_result = vmar.map(0, vmo, vmo_offset, length, vmar_flags);
        }
    }

    let mapped_addr = map_result.map_err(MemoryManager::get_errno_for_map_err)?;
    Ok(UserAddress::from_ptr(mapped_addr))
}

impl MemoryManagerState {
    // Map the memory without updating `self.mappings`.
    fn map_internal(
        &self,
        addr: DesiredAddress,
        vmo: &zx::Vmo,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
    ) -> Result<UserAddress, Errno> {
        map_in_vmar(
            &self.user_vmar,
            &self.user_vmar_info,
            addr,
            vmo,
            vmo_offset,
            length,
            prot_flags,
            options,
        )
    }

    fn map(
        &mut self,
        addr: DesiredAddress,
        vmo: Arc<zx::Vmo>,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        name: MappingName,
        file_write_guard: FileWriteGuardRef,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<UserAddress, Errno> {
        let mapped_addr = self.map_internal(addr, &vmo, vmo_offset, length, prot_flags, options)?;

        #[cfg(any(test, debug_assertions))]
        {
            // Take the lock on directory entry while holding the one on the mm state to ensure any
            // wrong ordering will trigger the tracing-mutex at the right call site.
            if let MappingName::File(name) = &name {
                let _l1 = name.entry.parent();
            }
        }

        let end = (mapped_addr + length).round_up(*PAGE_SIZE)?;

        if let DesiredAddress::FixedOverwrite(addr) = addr {
            assert_eq!(addr, mapped_addr);
            self.update_after_unmap(addr, end - addr, released_mappings)?;
        }

        let mut mapping =
            Mapping::new(mapped_addr, vmo, vmo_offset, prot_flags, options, file_write_guard);
        mapping.name = name;
        self.mappings.insert(mapped_addr..end, mapping);

        // TODO(https://fxbug.dev/97514): Create a guard region below this mapping if GROWSDOWN is
        // in |options|.

        Ok(mapped_addr)
    }

    fn remap(
        &mut self,
        _current_task: &CurrentTask,
        old_addr: UserAddress,
        old_length: usize,
        new_length: usize,
        flags: MremapFlags,
        new_address: UserAddress,
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

        // TODO(fxbug.dev/88262): Implement support for MREMAP_DONTUNMAP.
        if flags.contains(MremapFlags::DONTUNMAP) {
            not_implemented!("mremap flag MREMAP_DONTUNMAP not implemented");
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

        if !flags.contains(MremapFlags::FIXED) && old_length != 0 {
            // We are not requested to remap to a specific address, so first we see if we can remap
            // in-place. In-place copies (old_length == 0) are not allowed.
            if let Some(new_address) =
                self.try_remap_in_place(old_addr, old_length, new_length, released_mappings)?
            {
                return Ok(new_address);
            }
        }

        // There is no space to grow in place, or there is an explicit request to move.
        if flags.contains(MremapFlags::MAYMOVE) {
            let dst_address =
                if flags.contains(MremapFlags::FIXED) { Some(new_address) } else { None };
            self.remap_move(old_addr, old_length, dst_address, new_length, released_mappings)
        } else {
            error!(ENOMEM)
        }
    }

    /// Attempts to grow or shrink the mapping in-place. Returns `Ok(Some(addr))` if the remap was
    /// successful. Returns `Ok(None)` if there was no space to grow.
    fn try_remap_in_place(
        &mut self,
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
                self.unmap(new_range_in_place.end, old_length - new_length, released_mappings)?;
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

        if original_mapping.options.contains(MappingOptions::ANONYMOUS)
            && !original_mapping.options.contains(MappingOptions::SHARED)
        {
            // As a special case for private, anonymous mappings, allocate more space in the
            // VMO. FD-backed mappings have their backing memory handled by the file system.
            let new_vmo_size = original_mapping
                .vmo_offset
                .checked_add(final_length as u64)
                .ok_or_else(|| errno!(EINVAL))?;
            original_mapping
                .vmo
                .set_size(new_vmo_size)
                .map_err(MemoryManager::get_errno_for_map_err)?;
            // Zero-out the pages that were added when growing. This is not necessary, but ensures
            // correctness of our COW implementation. Ignore any errors.
            let original_length = original_range.end - original_range.start;
            let _ = original_mapping.vmo.op_range(
                zx::VmoOp::ZERO,
                original_mapping.vmo_offset + original_length as u64,
                (final_length - original_length) as u64,
            );
        }

        let prot_flags = original_mapping.prot_flags;

        // Re-map the original range, which may include pages before the requested range.
        Ok(Some(self.map(
            DesiredAddress::FixedOverwrite(original_range.start),
            original_mapping.vmo,
            original_mapping.vmo_offset,
            final_length,
            prot_flags,
            original_mapping.options,
            original_mapping.name,
            original_mapping.file_write_guard,
            released_mappings,
        )?))
    }

    /// Grows or shrinks the mapping while moving it to a new destination.
    fn remap_move(
        &mut self,
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

        if src_length == 0 && !src_mapping.options.contains(MappingOptions::SHARED) {
            // src_length == 0 means that the mapping is to be copied. This behavior is only valid
            // with MAP_SHARED mappings.
            return error!(EINVAL);
        }

        // If the destination range is smaller than the source range, we must first shrink
        // the source range in place. This must be done now and visible to processes, even if
        // a later failure causes the remap operation to fail.
        if src_length != 0 && src_length > dst_length {
            self.unmap(src_addr + dst_length, src_length - dst_length, released_mappings)?;
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
                self.unmap(dst_addr, dst_length, released_mappings)?;

                DesiredAddress::Fixed(dst_addr)
            }
        };

        if src_range.end > original_range.end {
            // The source range is not one contiguous mapping. This check must be done only after
            // the source range is shrunk and the destination unmapped.
            return error!(EFAULT);
        }

        let offset_into_original_range = (src_addr - original_range.start) as u64;
        let mut dst_vmo_offset = src_mapping.vmo_offset + offset_into_original_range;

        let vmo = if src_mapping.options.contains(MappingOptions::ANONYMOUS)
            && !src_mapping.options.contains(MappingOptions::SHARED)
        {
            // This mapping is a private, anonymous mapping. Create a COW child VMO that covers
            // the pages being moved and map that into the destination.
            let child_vmo = src_mapping
                .vmo
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::RESIZABLE,
                    dst_vmo_offset,
                    dst_length as u64,
                )
                .map_err(MemoryManager::get_errno_for_map_err)?;
            if dst_length > src_length {
                // The mapping has grown. Zero-out the pages that were "added" when growing the
                // mapping. These pages might be pointing inside the parent VMO, in which case
                // we want to zero them out to make them look like new pages. Since this is a COW
                // child VMO, this will simply allocate new pages.
                // This is not necessary, but ensures correctness of our COW implementation.
                // Ignore any errors.
                let _ = child_vmo.op_range(
                    zx::VmoOp::ZERO,
                    src_length as u64,
                    (dst_length - src_length) as u64,
                );
            }
            dst_vmo_offset = 0;
            Arc::new(child_vmo)
        } else {
            // This mapping is backed by an FD, just map the range of the VMO covering the moved
            // pages. If the VMO already had COW semantics, this preserves them.
            src_mapping.vmo
        };

        let new_address = self.map(
            dst_addr_for_map,
            vmo,
            dst_vmo_offset,
            dst_length,
            src_mapping.prot_flags,
            src_mapping.options,
            src_mapping.name,
            src_mapping.file_write_guard,
            released_mappings,
        )?;

        if src_length != 0 {
            // Only unmap the source range if this is not a copy. It was checked earlier that
            // this mapping is MAP_SHARED.
            self.unmap(src_addr, src_length, released_mappings)?;
        }

        Ok(new_address)
    }

    /// Unmaps the specified range. Unmapped mappings are placed in `released_mappings`.
    fn unmap(
        &mut self,
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

        self.update_after_unmap(addr, length, released_mappings)?;

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
    // - the pages of A, B, and C that fall in range R are unmapped; the VMO backing B is dropped.
    // - the VMO backing A is shrunk.
    // - a COW child VMO is created from C, which is mapped in the range of C that falls outside R.
    //
    // File-backed mappings don't need to have their VMOs modified.
    //
    // Unmapped mappings are placed in `released_mappings`.
    fn update_after_unmap(
        &mut self,
        addr: UserAddress,
        length: usize,
        released_mappings: &mut Vec<Mapping>,
    ) -> Result<(), Errno> {
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?;

        // Find the private, anonymous mapping that will get its tail cut off by this unmap call.
        let truncated_head = match self.mappings.get(&addr) {
            Some((range, mapping))
                if range.start != addr
                    && mapping.options.contains(MappingOptions::ANONYMOUS)
                    && !mapping.options.contains(MappingOptions::SHARED) =>
            {
                Some((range.start..addr, mapping.clone()))
            }
            _ => None,
        };

        // Find the private, anonymous mapping that will get its head cut off by this unmap call.
        let truncated_tail = match self.mappings.get(&end_addr) {
            Some((range, mapping))
                if range.end != end_addr
                    && mapping.options.contains(MappingOptions::ANONYMOUS)
                    && !mapping.options.contains(MappingOptions::SHARED) =>
            {
                Some((end_addr..range.end, mapping.clone()))
            }
            _ => None,
        };

        // Remove the original range of mappings from our map.
        released_mappings.extend(self.mappings.remove(&(addr..end_addr)));

        if let Some((range, mut mapping)) = truncated_tail {
            // Create and map a child COW VMO mapping that represents the truncated tail.
            let vmo_info = mapping.vmo.basic_info().map_err(impossible_error)?;
            let child_vmo_offset = (range.start - mapping.base) as u64 + mapping.vmo_offset;
            let child_length = range.end - range.start;
            let mut child_vmo = mapping
                .vmo
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::RESIZABLE,
                    child_vmo_offset,
                    child_length as u64,
                )
                .map_err(MemoryManager::get_errno_for_map_err)?;
            if vmo_info.rights.contains(zx::Rights::EXECUTE) {
                child_vmo =
                    child_vmo.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?;
            }

            // Update the mapping.
            mapping.vmo = Arc::new(child_vmo);
            mapping.base = range.start;
            mapping.vmo_offset = 0;

            self.map_internal(
                DesiredAddress::FixedOverwrite(range.start),
                &mapping.vmo,
                0,
                child_length,
                mapping.prot_flags,
                mapping.options,
            )?;

            // Replace the mapping with a new one that contains updated VMO handle.
            self.mappings.insert(range, mapping);
        }

        if let Some((range, mapping)) = truncated_head {
            // Resize the VMO of the head mapping, whose tail was cut off.
            let new_mapping_size = (range.end - range.start) as u64;
            let new_vmo_size = mapping.vmo_offset + new_mapping_size;
            mapping.vmo.set_size(new_vmo_size).map_err(MemoryManager::get_errno_for_map_err)?;
        }

        Ok(())
    }

    fn protect(
        &mut self,
        addr: UserAddress,
        length: usize,
        prot_flags: ProtectionFlags,
    ) -> Result<(), Errno> {
        // TODO(https://fxbug.dev/97514): If the mprotect flags include PROT_GROWSDOWN then the specified protection may
        // extend below the provided address if the lowest mapping is a MAP_GROWSDOWN mapping. This function has to
        // compute the potentially extended range before modifying the Zircon protections or metadata.
        let vmar_flags = prot_flags.to_vmar_flags();

        // Make one call to mprotect to update all the zircon protections.
        // SAFETY: This is safe because the vmar belongs to a different process.
        unsafe { self.user_vmar.protect(addr.ptr(), length, vmar_flags) }.map_err(|s| match s {
            zx::Status::INVALID_ARGS => errno!(EINVAL),
            // TODO: This should still succeed and change protection on whatever is mapped.
            zx::Status::NOT_FOUND => errno!(EINVAL),
            zx::Status::ACCESS_DENIED => errno!(EACCES),
            _ => impossible_error(s),
        })?;

        // Update the flags on each mapping in the range.
        let end = (addr + length).round_up(*PAGE_SIZE)?;
        let prot_range = addr..end;
        let mut updates = vec![];
        for (range, mapping) in self.mappings.intersection(addr..end) {
            let range = range.intersect(&prot_range);
            let mut mapping = mapping.clone();
            mapping.prot_flags = prot_flags;
            updates.push((range, mapping));
        }
        // Use a separate loop to avoid mutating the mappings structure while iterating over it.
        for (range, mapping) in updates {
            self.mappings.insert(range, mapping);
        }
        Ok(())
    }

    fn madvise(
        &self,
        _current_task: &CurrentTask,
        addr: UserAddress,
        length: usize,
        advice: u32,
    ) -> Result<(), Errno> {
        if !addr.is_aligned(*PAGE_SIZE) {
            return error!(EINVAL);
        }

        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EFAULT))?;
        if end_addr > self.max_address() {
            return error!(EFAULT);
        }
        let end_addr = end_addr.round_up(*PAGE_SIZE)?;

        let range_for_op = addr..end_addr;
        for (range, mapping) in self.mappings.intersection(&range_for_op) {
            if mapping.options.contains(MappingOptions::SHARED) {
                continue;
            }
            let range_to_zero = range.intersect(&range_for_op);
            if range_to_zero.is_empty() {
                continue;
            }
            let start = mapping.address_to_offset(range_to_zero.start);
            let end = mapping.address_to_offset(range_to_zero.end);
            let op = match advice {
                MADV_DONTNEED if !mapping.options.contains(MappingOptions::ANONYMOUS) => {
                    // Note, we cannot simply implemented MADV_DONTNEED with
                    // zx::VmoOp::DONT_NEED because they have different
                    // semantics.
                    not_implemented!(
                        "madvise advise {} with file-backed mapping not implemented",
                        advice
                    );
                    return error!(EINVAL);
                }
                MADV_DONTNEED => zx::VmoOp::ZERO,
                MADV_WILLNEED => zx::VmoOp::COMMIT,
                MADV_NOHUGEPAGE => return Ok(()),
                advice => {
                    not_implemented!("madvise advice {} not implemented", advice);
                    return error!(EINVAL);
                }
            };

            mapping.vmo.op_range(op, start, end - start).map_err(|s| match s {
                zx::Status::OUT_OF_RANGE => errno!(EINVAL),
                zx::Status::NO_MEMORY => errno!(ENOMEM),
                zx::Status::INVALID_ARGS => errno!(EINVAL),
                zx::Status::ACCESS_DENIED => errno!(EACCES),
                _ => impossible_error(s),
            })?;
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
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EFAULT))?;
        if end_addr > self.max_address() {
            return error!(EFAULT);
        }

        // Iterate over all contiguous mappings intersecting the requested range.
        let mut mappings = self.mappings.intersection(addr..end_addr);
        let mut prev_range_end = None;
        let mut offset = 0;
        let result = std::iter::from_fn(move || {
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

    /// Determines if an access at a given address could be covered by extending a growsdown mapping and
    /// extends it if possible. Returns true if the given address is covered by a mapping.
    pub fn extend_growsdown_mapping_to_address(
        &mut self,
        addr: UserAddress,
        is_write: bool,
    ) -> Result<bool, Error> {
        let (mapping_to_grow, mapping_low_addr) = match self.mappings.iter_starting_at(&addr).next()
        {
            Some((range, mapping)) => {
                if range.contains(&addr) {
                    // |addr| is already contained within a mapping, nothing to grow.
                    return Ok(false);
                }
                if !mapping.options.contains(MappingOptions::GROWSDOWN) {
                    return Ok(false);
                }
                (mapping, range.start)
            }
            None => return Ok(false),
        };
        if is_write && !mapping_to_grow.prot_flags.contains(ProtectionFlags::WRITE) {
            // Don't grow a read-only GROWSDOWN mapping for a write fault, it won't work.
            return Ok(false);
        }
        // TODO(https://fxbug.dev/97514): Once we add a guard region below a growsdown mapping we will need to move that
        // before attempting to map the grown area.
        let low_addr = addr - (addr.ptr() as u64 % *PAGE_SIZE);
        let high_addr = mapping_low_addr;
        let length = high_addr
            .ptr()
            .checked_sub(low_addr.ptr())
            .ok_or_else(|| anyhow!("Invalid growth range"))?;
        // TODO(https://fxbug.dev/97514): - Instead of making a new VMO, perhaps a growsdown mapping should be oversized to start with the end mapped.
        // Then on extension we could map further down in the VMO for as long as we had space.
        let vmo = Arc::new(zx::Vmo::create(length as u64).map_err(|s| match s {
            zx::Status::NO_MEMORY | zx::Status::OUT_OF_RANGE => {
                anyhow!("Could not allocate VMO for mapping growth")
            }
            _ => anyhow!("Unexpected error creating VMO: {s}"),
        })?);
        let vmar_flags = mapping_to_grow.prot_flags.to_vmar_flags() | zx::VmarFlags::SPECIFIC;
        let mapping = Mapping::new(
            low_addr,
            vmo.clone(),
            0,
            mapping_to_grow.prot_flags,
            mapping_to_grow.options,
            FileWriteGuardRef(None),
        );
        let vmar_offset = self
            .user_address_to_vmar_offset(low_addr)
            .map_err(|_| anyhow!("Address outside of user range"))?;
        let mapped_address = self
            .user_vmar
            .map(vmar_offset, &vmo, 0, length, vmar_flags)
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
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            mapping.read_memory(addr + bytes_read, &mut bytes[bytes_read..next_offset])?;
            bytes_read = next_offset;
        }

        if bytes_read != bytes.len() {
            error!(EFAULT)
        } else {
            Ok(())
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
    fn read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            if mapping.read_memory(addr + bytes_read, &mut bytes[bytes_read..next_offset]).is_err()
            {
                break;
            }
            bytes_read = next_offset;
        }

        // If at least one byte was requested but we got none, it means that `addr` was invalid.
        if !bytes.is_empty() && bytes_read == 0 {
            error!(EFAULT)
        } else {
            Ok(bytes_read)
        }
    }

    /// Writes the provided bytes.
    ///
    /// In case of success, the number of bytes written will always be `bytes.len()`.
    ///
    /// # Parameters
    /// - `addr`: The address to write to.
    /// - `bytes`: The bytes to write.
    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            mapping.write_memory(addr + bytes_written, &bytes[bytes_written..next_offset])?;
            bytes_written = next_offset;
        }

        if bytes_written != bytes.len() {
            error!(EFAULT)
        } else {
            Ok(bytes.len())
        }
    }

    /// Writes bytes starting at `addr`, continuing until either `bytes.len()` bytes have been
    /// written or no more bytes can be written.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to write from.
    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            if mapping
                .write_memory(addr + bytes_written, &bytes[bytes_written..next_offset])
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
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno>;

    /// Reads bytes starting at `addr`, continuing until either `bytes.len()` bytes have been read
    /// or no more bytes can be read from the target.
    ///
    /// This is used, for example, to read null-terminated strings where the exact length is not
    /// known, only the maximum length is.
    ///
    /// Consider using `MemoryAccessorExt::read_memory_partial_to_*` methods if you do not require
    /// control over the allocation.
    fn read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno>;

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
}

pub trait MemoryAccessorExt: MemoryAccessor {
    /// Read exactly `len` bytes of memory, returning them as a a Vec.
    fn read_memory_to_vec(&self, addr: UserAddress, len: usize) -> Result<Vec<u8>, Errno> {
        let mut buffer = vec![0u8; len];
        self.read_memory_to_slice(addr, &mut buffer[..])?;
        Ok(buffer)
    }

    /// Read up to `max_len` bytes from `addr`, returning them as a Vec.
    fn read_memory_partial_to_vec(
        &self,
        addr: UserAddress,
        max_len: usize,
    ) -> Result<Vec<u8>, Errno> {
        let mut buffer = vec![0u8; max_len];
        let bytes_read = self.read_memory_partial_to_slice(addr, &mut buffer[..])?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    /// Read exactly `N` bytes from `addr`, returning them as an array.
    fn read_memory_to_array<const N: usize>(&self, addr: UserAddress) -> Result<[u8; N], Errno> {
        let mut buffer = [0u8; N];
        self.read_memory_to_slice(addr, &mut buffer[..])?;
        Ok(buffer)
    }

    /// Read the contents of `buffer`, returning them as a Vec.
    fn read_buffer(&self, buffer: &UserBuffer) -> Result<Vec<u8>, Errno> {
        self.read_memory_to_vec(buffer.address, buffer.length)
    }

    /// Read an instance of T from `user`.
    fn read_object<T: FromBytes>(&self, user: UserRef<T>) -> Result<T, Errno> {
        // SAFETY: T is FromBytes, which means that any bit pattern is valid. Interpreting T as u8
        // is safe because T's alignment requirements are larger than u8.
        let mut object = T::new_zeroed();
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                &mut object as *mut T as *mut u8,
                std::mem::size_of::<T>(),
            )
        };
        self.read_memory_to_slice(user.addr(), buffer)?;
        Ok(object)
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
        let mut full_buffer = std::vec::Vec::<u8>::with_capacity(full_size);
        full_buffer.resize(partial_size, 0u8);

        self.read_memory_to_slice(user.addr(), &mut full_buffer)?;
        full_buffer.resize(full_size, 0u8); // Zero pad out to the correct size.

        // This should only fail if we provided a mis-sized buffers so panicking is OK.
        Ok(T::read_from(&*full_buffer).unwrap())
    }

    /// Read exactly `objects.len()` objects into `objects` from `user`.
    fn read_objects_to_slice<T: FromBytes>(
        &self,
        user: UserRef<T>,
        objects: &mut [T],
    ) -> Result<(), Errno> {
        // TODO(b/287679867) make this a single read_memory call
        for (index, object) in objects.iter_mut().enumerate() {
            *object = self.read_object(user.at(index))?;
        }
        Ok(())
    }

    /// Read exactly `len` objects from `user`, returning them as a Vec.
    fn read_objects_to_vec<T: Clone + FromBytes>(
        &self,
        user: UserRef<T>,
        len: usize,
    ) -> Result<Vec<T>, Errno> {
        let mut objects = vec![T::new_zeroed(); len];
        self.read_objects_to_slice(user, &mut objects[..])?;
        Ok(objects)
    }

    /// Read exactly `N` objects from `user`, returning them as an array.
    fn read_objects_to_array<T: Copy + FromBytes, const N: usize>(
        &self,
        user: UserRef<T>,
    ) -> Result<[T; N], Errno> {
        let mut objects = [T::new_zeroed(); N];
        self.read_objects_to_slice(user, &mut objects[..])?;
        Ok(objects)
    }

    /// Read exactly `iovec_count` `UserBuffer`s from `iovec_addr`, returning them as a Vec.
    ///
    /// Fails if `iovec_count` is greater than `UIO_MAXIOV`.
    fn read_iovec(
        &self,
        iovec_addr: UserAddress,
        iovec_count: i32,
    ) -> Result<Vec<UserBuffer>, Errno> {
        let iovec_count: usize = iovec_count.try_into().map_err(|_| errno!(EINVAL))?;
        if iovec_count > UIO_MAXIOV as usize {
            return error!(EINVAL);
        }

        self.read_objects_to_vec(iovec_addr.into(), iovec_count)
    }

    /// Read up to `max_size` bytes from `string`, stopping at the first discovered null byte and
    /// returning the results as a Vec.
    fn read_c_string_to_vec(&self, string: UserCString, max_size: usize) -> Result<Vec<u8>, Errno> {
        let min_chunk_size = std::cmp::min(*PAGE_SIZE as usize, max_size);

        let mut buf = vec![0; min_chunk_size];
        let mut index = 0;
        loop {
            let read = self.read_memory_partial_to_slice(string.addr(), &mut buf[index..])?;

            if let Some(nul_index) = memchr::memchr(b'\0', &buf[index..index + read]) {
                buf.resize(index + nul_index, 0u8);
                if buf.len() > max_size {
                    return error!(ENAMETOOLONG);
                }

                return Ok(buf);
            }
            index += read;

            if index != buf.len() || index >= max_size {
                // There's no more for us to read.
                return error!(ENAMETOOLONG);
            }

            // Trigger a capacity increase.
            buf.reserve(min_chunk_size);
            buf.resize(std::cmp::min(buf.capacity(), max_size), 0);
        }
    }

    /// Read `len` bytes from `start` and parse the region as null-delimited CStrings, for example
    /// how `argv` is stored.
    ///
    /// There can be an arbitrary number of null bytes in between `start` and `end`, but `end` must
    /// point to a null byte.
    fn read_nul_delimited_c_string_list(
        &self,
        start: UserAddress,
        len: usize,
    ) -> Result<Vec<FsString>, Errno> {
        let buf = self.read_memory_to_vec(start, len)?;
        let mut buf = &buf[..];

        let mut list = vec![];
        while !buf.is_empty() {
            let len_consumed = {
                let segment = CStr::from_bytes_until_nul(buf).map_err(|e| errno!(EINVAL, e))?;

                // Return the string without the null to match our other APIs, but advance the
                // "cursor" of the buf variable past the null byte.
                list.push(segment.to_bytes().to_owned());
                segment.to_bytes_with_nul().len()
            };
            buf = &buf[len_consumed..];
        }

        Ok(list)
    }

    /// Read up to `buffer.len()` bytes from `string`, stopping at the first discovered null byte
    /// and returning the result as a slice that ends before that null.
    ///
    /// Consider using `read_c_string_to_vec` if you do not require control over the allocation.
    fn read_c_string_to_slice<'a>(
        &self,
        string: UserCString,
        buffer: &'a mut [u8],
    ) -> Result<&'a [u8], Errno> {
        let actual = self.read_memory_partial_to_slice(string.addr(), buffer)?;
        let buffer = &mut buffer[..actual];
        let null_index = memchr::memchr(b'\0', buffer).ok_or_else(|| errno!(ENAMETOOLONG))?;
        Ok(&buffer[..null_index])
    }

    fn write_object<T: AsBytes>(&self, user: UserRef<T>, object: &T) -> Result<usize, Errno> {
        self.write_memory(user.addr(), object.as_bytes())
    }

    fn write_objects<T: AsBytes>(&self, user: UserRef<T>, objects: &[T]) -> Result<usize, Errno> {
        let mut bytes_written = 0;
        for (index, object) in objects.iter().enumerate() {
            bytes_written += self.write_object(user.at(index), object)?;
        }
        Ok(bytes_written)
    }
}

impl MemoryAccessor for MemoryManager {
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        self.state.read().read_memory_to_slice(addr, bytes)
    }

    fn read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        self.state.read().read_memory_partial_to_slice(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.state.read().write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.state.read().write_memory_partial(addr, bytes)
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
    pub futex: FutexTable,

    /// Mutable state for the memory manager.
    pub state: RwLock<MemoryManagerState>,

    /// Whether this address space is dumpable.
    pub dumpable: Mutex<DumpPolicy>,

    /// Maximum valid user address for this vmar.
    pub maximum_valid_user_address: UserAddress,
}

impl MemoryManager {
    pub fn new(root_vmar: zx::Vmar) -> Result<Self, zx::Status> {
        let info = root_vmar.info()?;
        let user_vmar = create_user_vmar(&root_vmar, &info)?;
        let user_vmar_info = user_vmar.info()?;
        Ok(Self::from_vmar(root_vmar, user_vmar, user_vmar_info))
    }

    pub fn new_empty() -> Self {
        let root_vmar = zx::Vmar::from(zx::Handle::invalid());
        let user_vmar = zx::Vmar::from(zx::Handle::invalid());
        Self::from_vmar(root_vmar, user_vmar, Default::default())
    }

    fn from_vmar(root_vmar: zx::Vmar, user_vmar: zx::Vmar, user_vmar_info: zx::VmarInfo) -> Self {
        MemoryManager {
            root_vmar,
            base_addr: UserAddress::from_ptr(user_vmar_info.base),
            futex: FutexTable::default(),
            state: RwLock::new(MemoryManagerState {
                user_vmar,
                user_vmar_info,
                brk: None,
                mappings: RangeMap::new(),
                executable_node: None,
                stack_base: UserAddress::NULL,
                stack_size: 0,
                stack_start: UserAddress::NULL,
                auxv_start: UserAddress::NULL,
                auxv_end: UserAddress::NULL,
                argv_start: UserAddress::NULL,
                argv_end: UserAddress::NULL,
                environ_start: UserAddress::NULL,
                environ_end: UserAddress::NULL,
                vdso_base: UserAddress::NULL,
            }),
            // TODO(security): Reset to DISABLE, or the value in the fs.suid_dumpable sysctl, under
            // certain conditions as specified in the prctl(2) man page.
            dumpable: Mutex::new(DumpPolicy::User),
            maximum_valid_user_address: UserAddress::from_ptr(
                user_vmar_info.base + user_vmar_info.len,
            ),
        }
    }

    pub fn set_brk(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
    ) -> Result<UserAddress, Errno> {
        let rlimit_data = std::cmp::min(
            PROGRAM_BREAK_LIMIT,
            current_task.thread_group.get_rlimit(Resource::DATA),
        );

        let mut released_mappings = vec![];
        let mut state = self.state.write();

        // Ensure that a program break exists by mapping at least one page.
        let mut brk = match state.brk {
            None => {
                let vmo = zx::Vmo::create(PROGRAM_BREAK_LIMIT).map_err(|_| errno!(ENOMEM))?;
                set_zx_name(&vmo, b"starnix-brk");
                let length = *PAGE_SIZE as usize;
                let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
                let addr = state.map(
                    DesiredAddress::Any,
                    Arc::new(vmo),
                    0,
                    length,
                    prot_flags,
                    MappingOptions::empty(),
                    MappingName::Heap,
                    FileWriteGuardRef(None),
                    &mut released_mappings,
                )?;
                let brk = ProgramBreak { base: addr, current: addr };
                state.brk = Some(brk);
                brk
            }
            Some(brk) => brk,
        };

        if addr < brk.base || addr > brk.base + rlimit_data {
            // The requested program break is out-of-range. We're supposed to simply
            // return the current program break.
            return Ok(brk.current);
        }

        let (range, mapping) = state.mappings.get(&brk.current).ok_or_else(|| errno!(EFAULT))?;

        brk.current = addr;

        let old_end = range.end;
        let new_end = (brk.current + 1u64).round_up(*PAGE_SIZE).unwrap();

        match new_end.cmp(&old_end) {
            std::cmp::Ordering::Less => {
                // We've been asked to free memory.
                let delta = old_end - new_end;
                let vmo = mapping.vmo.clone();
                state.unmap(new_end, delta, &mut released_mappings)?;
                let vmo_offset = new_end - brk.base;
                vmo.op_range(zx::VmoOp::ZERO, vmo_offset as u64, delta as u64)
                    .map_err(impossible_error)?;
            }
            std::cmp::Ordering::Greater => {
                // We've been asked to map more memory.
                let delta = new_end - old_end;
                let vmo_offset = old_end - brk.base;
                let range = range.clone();
                let mapping = mapping.clone();

                released_mappings.extend(state.mappings.remove(&range));
                match state.user_vmar.map(
                    old_end - self.base_addr,
                    &mapping.vmo,
                    vmo_offset as u64,
                    delta,
                    zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE | zx::VmarFlags::SPECIFIC,
                ) {
                    Ok(_) => {
                        state.mappings.insert(brk.base..new_end, mapping);
                    }
                    Err(e) => {
                        // We failed to extend the mapping, which means we need to add
                        // back the old mapping.
                        state.mappings.insert(brk.base..old_end, mapping);
                        return Err(Self::get_errno_for_map_err(e));
                    }
                }
            }
            _ => {}
        };

        state.brk = Some(brk);
        Ok(brk.current)
    }

    pub fn snapshot_to(&self, target: &MemoryManager) -> Result<(), Errno> {
        // TODO(fxbug.dev/123742): When SNAPSHOT (or equivalent) is supported on pager-backed VMOs
        // we can remove the hack below (which also won't be performant). For now, as a workaround,
        // we use SNAPSHOT_AT_LEAST_ON_WRITE on both the child and the parent.

        struct VmoInfo {
            vmo: Arc<zx::Vmo>,
            size: u64,

            // Indicates whether or not the VMO needs to be replaced on the parent as well.
            needs_snapshot_on_parent: bool,
        }

        // Clones the `vmo` and returns the `VmoInfo` with the clone.
        fn clone_vmo(vmo: &Arc<zx::Vmo>, rights: zx::Rights) -> Result<VmoInfo, Errno> {
            let vmo_info = vmo.info().map_err(impossible_error)?;
            let pager_backed = vmo_info.flags.contains(zx::VmoInfoFlags::PAGER_BACKED);
            Ok(if pager_backed && !rights.contains(zx::Rights::WRITE) {
                VmoInfo {
                    vmo: vmo.clone(),
                    size: vmo_info.size_bytes,
                    needs_snapshot_on_parent: false,
                }
            } else {
                let mut cloned_vmo = vmo
                    .create_child(
                        if pager_backed {
                            zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE
                        } else {
                            zx::VmoChildOptions::SNAPSHOT
                        } | zx::VmoChildOptions::RESIZABLE,
                        0,
                        vmo_info.size_bytes,
                    )
                    .map_err(MemoryManager::get_errno_for_map_err)?;
                if rights.contains(zx::Rights::EXECUTE) {
                    cloned_vmo = cloned_vmo
                        .replace_as_executable(&VMEX_RESOURCE)
                        .map_err(impossible_error)?;
                }
                VmoInfo {
                    vmo: Arc::new(cloned_vmo),
                    size: vmo_info.size_bytes,
                    needs_snapshot_on_parent: pager_backed,
                }
            })
        }

        fn snapshot_vmo(
            vmo: &Arc<zx::Vmo>,
            size: u64,
            rights: zx::Rights,
        ) -> Result<Arc<zx::Vmo>, Errno> {
            let mut cloned_vmo = vmo
                .create_child(
                    zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE
                        | zx::VmoChildOptions::RESIZABLE,
                    0,
                    size,
                )
                .map_err(MemoryManager::get_errno_for_map_err)?;

            if rights.contains(zx::Rights::EXECUTE) {
                cloned_vmo =
                    cloned_vmo.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?;
            }
            Ok(Arc::new(cloned_vmo))
        }

        let state: &mut MemoryManagerState = &mut self.state.write();
        let mut target_state = target.state.write();
        let mut child_vmos = HashMap::<zx::Koid, VmoInfo>::new();
        let mut replaced_vmos = HashMap::<zx::Koid, Arc<zx::Vmo>>::new();

        for (range, mapping) in state.mappings.iter_mut() {
            let vmo_offset = mapping.vmo_offset + (range.start - mapping.base) as u64;
            let length = range.end - range.start;

            let target_vmo = if mapping.options.contains(MappingOptions::SHARED) {
                &mapping.vmo
            } else {
                let basic_info = mapping.vmo.basic_info().map_err(impossible_error)?;

                let VmoInfo { vmo, size: vmo_size, needs_snapshot_on_parent } = child_vmos
                    .entry(basic_info.koid)
                    .or_insert(clone_vmo(&mapping.vmo, basic_info.rights)?);

                if *needs_snapshot_on_parent {
                    let vmo = replaced_vmos.entry(basic_info.koid).or_insert(snapshot_vmo(
                        &mapping.vmo,
                        *vmo_size,
                        basic_info.rights,
                    )?);
                    map_in_vmar(
                        &state.user_vmar,
                        &state.user_vmar_info,
                        DesiredAddress::FixedOverwrite(range.start),
                        vmo,
                        vmo_offset,
                        length,
                        mapping.prot_flags,
                        mapping.options,
                    )?;

                    mapping.vmo = vmo.clone();
                }
                vmo
            };

            let mut released_mappings = vec![];
            target_state.map(
                DesiredAddress::Fixed(range.start),
                target_vmo.clone(),
                vmo_offset,
                length,
                mapping.prot_flags,
                mapping.options,
                mapping.name.clone(),
                FileWriteGuardRef(None),
                &mut released_mappings,
            )?;
            assert!(released_mappings.is_empty());
        }

        target_state.brk = state.brk;
        target_state.executable_node = state.executable_node.clone();
        *target.dumpable.lock() = *self.dumpable.lock();
        target_state.vdso_base = state.vdso_base;

        Ok(())
    }

    pub fn exec(&self, exe_node: NamespaceNode) -> Result<(), zx::Status> {
        // The previous mapping should be dropped only after the lock to state is released to
        // prevent lock order inversion.
        let _old_mappings = {
            let mut state = self.state.write();
            let info = self.root_vmar.info()?;
            // SAFETY: This operation is safe because the VMAR is for another process.
            unsafe { state.user_vmar.destroy()? }
            state.user_vmar = create_user_vmar(&self.root_vmar, &info)?;
            state.user_vmar_info = state.user_vmar.info()?;
            state.brk = None;
            state.executable_node = Some(exe_node);

            std::mem::replace(&mut state.mappings, RangeMap::new())
        };
        Ok(())
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
            _ => impossible_error(status),
        }
    }

    pub fn map(
        &self,
        addr: DesiredAddress,
        vmo: Arc<zx::Vmo>,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        name: MappingName,
        file_write_guard: FileWriteGuardRef,
    ) -> Result<UserAddress, Errno> {
        // Unmapped mappings must be released after the state is unlocked.
        let mut released_mappings = vec![];
        let mut state = self.state.write();
        let result = state.map(
            addr,
            vmo,
            vmo_offset,
            length,
            prot_flags,
            options,
            name,
            file_write_guard,
            &mut released_mappings,
        );

        // Drop the state before the unmapped mappings, since dropping a mapping may acquire a lock
        // in `DirEntry`'s `drop`.
        std::mem::drop(state);
        std::mem::drop(released_mappings);

        result
    }

    pub fn remap(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        old_length: usize,
        new_length: usize,
        flags: MremapFlags,
        new_addr: UserAddress,
    ) -> Result<UserAddress, Errno> {
        let mut released_mappings = vec![];
        let mut state = self.state.write();
        let result = state.remap(
            current_task,
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

    pub fn unmap(&self, addr: UserAddress, length: usize) -> Result<(), Errno> {
        let mut released_mappings = vec![];
        let mut state = self.state.write();
        let result = state.unmap(addr, length, &mut released_mappings);

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
        self.state.read().madvise(current_task, addr, length, advice)
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
                let start_split_mapping = Mapping::new(
                    range.start,
                    mapping.vmo.clone(),
                    mapping.vmo_offset,
                    mapping.prot_flags,
                    mapping.options,
                    mapping.file_write_guard.clone(),
                );
                state.mappings.insert(start_split_range, start_split_mapping);

                // Shrink the range of the named mapping to only the named area.
                mapping.vmo_offset = start_split_length as u64;
                range = addr..range.end;
            }
            if let Some(last_range_end) = last_range_end {
                if last_range_end != range.start {
                    // The name must apply to a contiguous range of mapped pages.
                    return error!(ENOMEM);
                }
            }
            last_range_end = Some(range.end.round_up(*PAGE_SIZE)?);
            match &name {
                Some(vmo_name) => {
                    set_zx_name(&*mapping.vmo, vmo_name);
                }
                None => {
                    set_zx_name(&*mapping.vmo, b"");
                }
            };
            if range.end > end {
                // The named region ends before the last mapping ends. Split the tail off of the
                // last mapping to have an unnamed mapping after the named region.
                let tail_range = end..range.end;
                let tail_offset = range.end - end;
                let tail_mapping = Mapping::new(
                    end,
                    mapping.vmo.clone(),
                    mapping.vmo_offset + tail_offset as u64,
                    mapping.prot_flags,
                    mapping.options,
                    mapping.file_write_guard.clone(),
                );
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

    /// Returns the VMO mapped at the address and the offset into the VMO of the address. Intended
    /// for implementing futexes.
    pub fn get_mapping_vmo(
        &self,
        addr: UserAddress,
        perms: ProtectionFlags,
    ) -> Result<(Arc<zx::Vmo>, u64), Errno> {
        let state = self.state.read();
        let (_, mapping) = state.mappings.get(&addr).ok_or_else(|| errno!(EFAULT))?;
        if !mapping.prot_flags.contains(perms) {
            return error!(EACCES);
        }
        Ok((Arc::clone(&mapping.vmo), mapping.address_to_offset(addr)))
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

    pub fn get_random_base(&self, length: usize) -> UserAddress {
        let state = self.state.read();
        // Allocate a vmar of the correct size, get the random location, then immediately destroy it.
        // This randomizes the load address without loading into a sub-vmar and breaking mprotect.
        // This is different from how Linux actually lays out the address space. We might need to
        // rewrite it eventually.
        let (temp_vmar, base) =
            state.user_vmar.allocate(0, length, zx::VmarFlags::empty()).unwrap();
        // SAFETY: This is safe because the vmar is not in the current process.
        unsafe { temp_vmar.destroy().unwrap() };
        UserAddress::from_ptr(base)
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

            let vmo_info = mapping.vmo.info().map_err(|_| errno!(EIO))?;
            let committed_bytes = vmo_info.committed_bytes as usize;
            result.vm_rss += committed_bytes;

            if mapping.options.contains(MappingOptions::ANONYMOUS)
                && !mapping.options.contains(MappingOptions::SHARED)
            {
                result.rss_anonymous += committed_bytes;
            }

            if vmo_info.share_count > 1 {
                result.rss_shared += committed_bytes;
            }

            if let MappingName::File(_) = mapping.name {
                result.rss_file += committed_bytes;
            }

            if mapping.options.contains(MappingOptions::ELF_BINARY)
                && mapping.prot_flags.contains(ProtectionFlags::WRITE)
            {
                result.vm_data += size;
            }

            if mapping.options.contains(MappingOptions::ELF_BINARY)
                && mapping.prot_flags.contains(ProtectionFlags::EXEC)
            {
                result.vm_exe += size;
            }
        }
        result.vm_stack = state.stack_size;
        Ok(result)
    }
}

/// A VMO and the userspace address at which it was mapped.
#[derive(Debug, Clone)]
pub struct MappedVmo {
    pub vmo: Arc<zx::Vmo>,
    pub user_address: UserAddress,
}

impl MappedVmo {
    pub fn new(vmo: Arc<zx::Vmo>, user_address: UserAddress) -> Self {
        Self { vmo, user_address }
    }
}

/// The user-space address at which a mapping should be placed. Used by [`MemoryManager::map`].
#[derive(Debug, Clone, Copy)]
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
        if map.prot_flags.contains(ProtectionFlags::READ) { 'r' } else { '-' },
        if map.prot_flags.contains(ProtectionFlags::WRITE) { 'w' } else { '-' },
        if map.prot_flags.contains(ProtectionFlags::EXEC) { 'x' } else { '-' },
        if map.options.contains(MappingOptions::SHARED) { 's' } else { 'p' },
        map.vmo_offset,
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
        MappingName::None => {
            if map.options.contains(MappingOptions::SHARED)
                && map.options.contains(MappingOptions::ANONYMOUS)
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
}

#[derive(Clone)]
pub struct ProcMapsFile(Weak<Task>);
impl ProcMapsFile {
    pub fn new_node(task: Weak<Task>) -> impl FsNodeOps {
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
        let state = task.mm.state.read();
        let mut iter = state.mappings.iter_starting_at(&cursor);
        if let Some((range, map)) = iter.next() {
            write_map(&task, sink, range, map)?;
            return Ok(Some(range.end));
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub struct ProcSmapsFile(Weak<Task>);
impl ProcSmapsFile {
    pub fn new_node(task: Weak<Task>) -> impl FsNodeOps {
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
        let state = task.mm.state.read();
        let mut iter = state.mappings.iter_starting_at(&cursor);
        if let Some((range, map)) = iter.next() {
            write_map(&task, sink, range, map)?;

            let size_kb = (range.end.ptr() - range.start.ptr()) / 1024;
            writeln!(sink, "Size:\t{size_kb} kB",)?;

            let vmo_info = map.vmo.info().map_err(|_| errno!(EIO))?;
            let rss_kb = vmo_info.committed_bytes / 1024;
            writeln!(sink, "Rss:\t{rss_kb} kB")?;

            writeln!(
                sink,
                "Pss:\t{} kB",
                if map.options.contains(MappingOptions::SHARED) {
                    rss_kb / vmo_info.share_count as u64
                } else {
                    rss_kb
                }
            )?;

            let is_shared = vmo_info.share_count > 1;
            let shared_clean_kb = if is_shared { rss_kb } else { 0 };
            // TODO: Report dirty pages for paged VMOs.
            let shared_dirty_kb = 0;
            writeln!(sink, "Shared_Clean:\t{shared_clean_kb} kB")?;
            writeln!(sink, "Shared_Dirty:\t{shared_dirty_kb} kB")?;

            let private_clean_kb = if is_shared { 0 } else { rss_kb };
            // TODO: Report dirty pages for paged VMOs.
            let private_dirty_kb = 0;
            writeln!(sink, "Private_Clean:\t{} kB", private_clean_kb)?;
            writeln!(sink, "Private_Dirty:\t{} kB", private_dirty_kb)?;

            let is_anonymous = map.options.contains(MappingOptions::ANONYMOUS)
                && !map.options.contains(MappingOptions::SHARED);
            let anonymous_kb = if is_anonymous { rss_kb } else { 0 };
            writeln!(sink, "Anonymous:\t{anonymous_kb} kB")?;
            writeln!(sink, "KernelPageSize:\t{page_size_kb} kB")?;
            writeln!(sink, "MMUPageSize:\t{page_size_kb} kB")?;

            // TODO(https://fxrev.dev/79328): Add optional fields.
            return Ok(Some(range.end));
        }
        Ok(None)
    }
}

/// Creates a VMO that can be used in an anonymous mapping for the `mmap`
/// syscall.
pub fn create_anonymous_mapping_vmo(size: u64) -> Result<Arc<zx::Vmo>, Errno> {
    // mremap can grow memory regions, so make sure the VMO is resizable.
    let mut vmo =
        zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, size).map_err(|s| match s {
            zx::Status::NO_MEMORY => errno!(ENOMEM),
            zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
            _ => impossible_error(s),
        })?;
    set_zx_name(&vmo, b"starnix-anon");
    // TODO(fxbug.dev/105639): Audit replace_as_executable usage
    vmo = vmo.replace_as_executable(&VMEX_RESOURCE).map_err(impossible_error)?;
    Ok(Arc::new(vmo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mm::syscalls::do_mmap, task::syscalls::sys_prctl, testing::*};
    use assert_matches::assert_matches;
    use itertools::assert_equal;
    use std::ffi::CString;
    use zerocopy::FromZeroes;

    #[::fuchsia::test]
    async fn test_brk() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        // Look up the given addr in the mappings table.
        let get_range = |addr: &UserAddress| {
            let state = mm.state.read();
            let (range, _) = state.mappings.get(addr).expect("failed to find mapping");
            range.clone()
        };

        // Initialize the program break.
        let base_addr = mm
            .set_brk(&current_task, UserAddress::default())
            .expect("failed to set initial program break");
        assert!(base_addr > UserAddress::default());

        // Check that the initial program break actually maps some memory.
        let range0 = get_range(&base_addr);
        assert_eq!(range0.start, base_addr);
        assert_eq!(range0.end, base_addr + *PAGE_SIZE);

        // Grow the program break by a tiny amount that does not actually result in a change.
        let addr1 = mm.set_brk(&current_task, base_addr + 1u64).expect("failed to grow brk");
        assert_eq!(addr1, base_addr + 1u64);
        let range1 = get_range(&base_addr);
        assert_eq!(range1.start, range0.start);
        assert_eq!(range1.end, range0.end);

        // Grow the program break by a non-trival amount and observe the larger mapping.
        let addr2 = mm.set_brk(&current_task, base_addr + 24893u64).expect("failed to grow brk");
        assert_eq!(addr2, base_addr + 24893u64);
        let range2 = get_range(&base_addr);
        assert_eq!(range2.start, base_addr);
        assert_eq!(range2.end, addr2.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break and observe the smaller mapping.
        let addr3 = mm.set_brk(&current_task, base_addr + 14832u64).expect("failed to shrink brk");
        assert_eq!(addr3, base_addr + 14832u64);
        let range3 = get_range(&base_addr);
        assert_eq!(range3.start, base_addr);
        assert_eq!(range3.end, addr3.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break close to zero and observe the smaller mapping.
        let addr4 =
            mm.set_brk(&current_task, base_addr + 3u64).expect("failed to drastically shrink brk");
        assert_eq!(addr4, base_addr + 3u64);
        let range4 = get_range(&base_addr);
        assert_eq!(range4.start, base_addr);
        assert_eq!(range4.end, addr4.round_up(*PAGE_SIZE).unwrap());

        // Shrink the program break close to zero and observe that the mapping is not entirely gone.
        let addr5 =
            mm.set_brk(&current_task, base_addr).expect("failed to drastically shrink brk to zero");
        assert_eq!(addr5, base_addr);
        let range5 = get_range(&base_addr);
        assert_eq!(range5.start, base_addr);
        assert_eq!(range5.end, addr5 + *PAGE_SIZE);
    }

    #[::fuchsia::test]
    async fn test_mm_exec() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let has = |addr: &UserAddress| -> bool {
            let state = mm.state.read();
            state.mappings.get(addr).is_some()
        };

        let brk_addr = mm
            .set_brk(&current_task, UserAddress::default())
            .expect("failed to set initial program break");
        assert!(brk_addr > UserAddress::default());
        assert!(has(&brk_addr));

        let mapped_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        assert!(mapped_addr > UserAddress::default());
        assert!(has(&mapped_addr));

        let node = current_task.lookup_path_from_root(b"/").unwrap();
        mm.exec(node).expect("failed to exec memory manager");

        assert!(!has(&brk_addr));
        assert!(!has(&mapped_addr));

        // Check that the old addresses are actually available for mapping.
        let brk_addr2 = map_memory(&current_task, brk_addr, *PAGE_SIZE);
        assert_eq!(brk_addr, brk_addr2);
        let mapped_addr2 = map_memory(&current_task, mapped_addr, *PAGE_SIZE);
        assert_eq!(mapped_addr, mapped_addr2);
    }

    #[::fuchsia::test]
    async fn test_get_contiguous_mappings_at() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        // Create four one-page mappings with a hole between the third one and the fourth one.
        let page_size = *PAGE_SIZE as usize;
        let addr_a = mm.base_addr + 10 * page_size;
        let addr_b = mm.base_addr + 11 * page_size;
        let addr_c = mm.base_addr + 12 * page_size;
        let addr_d = mm.base_addr + 14 * page_size;
        assert_eq!(map_memory(&current_task, addr_a, *PAGE_SIZE), addr_a);
        assert_eq!(map_memory(&current_task, addr_b, *PAGE_SIZE), addr_b);
        assert_eq!(map_memory(&current_task, addr_c, *PAGE_SIZE), addr_c);
        assert_eq!(map_memory(&current_task, addr_d, *PAGE_SIZE), addr_d);
        assert_eq!(mm.get_mapping_count(), 4);

        // Obtain references to the mappings.
        let mm_state = mm.state.read();
        let (map_a, map_b, map_c, map_d) = {
            let mut it = mm_state.mappings.iter();
            (it.next().unwrap().1, it.next().unwrap().1, it.next().unwrap().1, it.next().unwrap().1)
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
            mm_state.get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 3 / 2).unwrap(),
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
            mm_state.get_contiguous_mappings_at(addr_b + page_size / 2, page_size * 3 / 2).unwrap(),
            vec![(map_b, page_size / 2), (map_c, page_size)],
        );

        // Verify that results stop if there is a hole.
        assert_equal(
            mm_state.get_contiguous_mappings_at(addr_a + page_size / 2, page_size * 10).unwrap(),
            vec![(map_a, page_size / 2), (map_b, page_size), (map_c, page_size)],
        );

        // Verify that results stop at the last mapped page.
        assert_equal(
            mm_state.get_contiguous_mappings_at(addr_d, page_size * 10).unwrap(),
            vec![(map_d, page_size)],
        );

        // Verify that requesting an unmapped address returns an empty iterator.
        assert_equal(mm_state.get_contiguous_mappings_at(addr_a - 100u64, 50).unwrap(), vec![]);
        assert_equal(mm_state.get_contiguous_mappings_at(addr_a - 100u64, 200).unwrap(), vec![]);

        // Verify that requesting zero bytes returns an empty iterator.
        assert_equal(mm_state.get_contiguous_mappings_at(addr_a, 0).unwrap(), vec![]);

        // Verify errors.
        assert_eq!(
            mm_state.get_contiguous_mappings_at(UserAddress::from(100), usize::MAX).err().unwrap(),
            errno!(EFAULT)
        );
        assert_eq!(
            mm_state.get_contiguous_mappings_at(mm_state.max_address() + 1u64, 0).err().unwrap(),
            errno!(EFAULT)
        );
    }

    #[::fuchsia::test]
    async fn test_read_write_crossing_mappings() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        // Map two contiguous pages at fixed addresses, but backed by distinct mappings.
        let page_size = *PAGE_SIZE;
        let addr = mm.base_addr + 10 * page_size;
        assert_eq!(map_memory(&current_task, addr, page_size), addr);
        assert_eq!(map_memory(&current_task, addr + page_size, page_size), addr + page_size);
        assert_eq!(mm.get_mapping_count(), 2);

        // Write a pattern crossing our two mappings.
        let test_addr = addr + page_size / 2;
        let data: Vec<u8> = (0..page_size).map(|i| (i % 256) as u8).collect();
        mm.write_memory(test_addr, &data).expect("failed to write test data");

        // Read it back.
        let data_readback =
            mm.read_memory_to_vec(test_addr, data.len()).expect("failed to read test data");
        assert_eq!(&data, &data_readback);
    }

    #[::fuchsia::test]
    async fn test_read_write_errors() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let page_size = *PAGE_SIZE;
        let addr = map_memory(&current_task, UserAddress::default(), page_size);
        let buf = vec![0u8; page_size as usize];

        // Verify that accessing data that is only partially mapped is an error.
        let partial_addr_before = addr - page_size / 2;
        assert_eq!(mm.write_memory(partial_addr_before, &buf), error!(EFAULT));
        assert_eq!(mm.read_memory_to_vec(partial_addr_before, buf.len()), error!(EFAULT));
        let partial_addr_after = addr + page_size / 2;
        assert_eq!(mm.write_memory(partial_addr_after, &buf), error!(EFAULT));
        assert_eq!(mm.read_memory_to_vec(partial_addr_after, buf.len()), error!(EFAULT));

        // Verify that accessing unmapped memory is an error.
        let unmapped_addr = addr + 10 * page_size;
        assert_eq!(mm.write_memory(unmapped_addr, &buf), error!(EFAULT));
        assert_eq!(mm.read_memory_to_vec(unmapped_addr, buf.len()), error!(EFAULT));

        // However, accessing zero bytes in unmapped memory is not an error.
        mm.write_memory(unmapped_addr, &[]).expect("failed to write no data");
        mm.read_memory_to_vec(unmapped_addr, 0).expect("failed to read no data");
    }

    #[::fuchsia::test]
    async fn test_read_c_string_to_vec() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let page_size = *PAGE_SIZE;
        let max_size = 2 * page_size as usize;
        let addr = mm.base_addr + 10 * page_size;

        // Map a page at a fixed address and write an unterminated string at the end of it.
        assert_eq!(map_memory(&current_task, addr, page_size), addr);
        let test_str = b"foo!";
        let test_addr = addr + page_size - test_str.len();
        mm.write_memory(test_addr, test_str).expect("failed to write test string");

        // Expect error if the string is not terminated.
        assert_eq!(
            mm.read_c_string_to_vec(UserCString::new(test_addr), max_size),
            error!(ENAMETOOLONG)
        );

        // Expect success if the string is terminated.
        mm.write_memory(addr + (page_size - 1), b"\0").expect("failed to write nul");
        assert_eq!(mm.read_c_string_to_vec(UserCString::new(test_addr), max_size).unwrap(), b"foo");

        // Expect success if the string spans over two mappings.
        assert_eq!(map_memory(&current_task, addr + page_size, page_size), addr + page_size);
        assert_eq!(mm.get_mapping_count(), 2);
        mm.write_memory(addr + (page_size - 1), b"bar\0").expect("failed to write extra chars");
        assert_eq!(
            mm.read_c_string_to_vec(UserCString::new(test_addr), max_size).unwrap(),
            b"foobar"
        );

        // Expect error if the string exceeds max limit
        assert_eq!(mm.read_c_string_to_vec(UserCString::new(test_addr), 2), error!(ENAMETOOLONG));

        // Expect error if the address is invalid.
        assert_eq!(mm.read_c_string_to_vec(UserCString::default(), max_size), error!(EFAULT));
    }

    #[::fuchsia::test]
    async fn can_read_argv_like_regions() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        // Map a page.
        let page_size = *PAGE_SIZE;
        let addr = map_memory_anywhere(&current_task, page_size);
        assert!(!addr.is_null());

        // Write an unterminated string.
        let mut payload = b"first".to_vec();
        let mut expected_parses = vec![];
        mm.write_memory(addr, &payload).unwrap();

        // Expect error if the string is not terminated.
        assert_eq!(mm.read_nul_delimited_c_string_list(addr, payload.len()), error!(EINVAL));

        // Expect success if the string is terminated.
        expected_parses.push(payload.clone());
        payload.push(0);
        mm.write_memory(addr, &payload).unwrap();
        assert_eq!(
            mm.read_nul_delimited_c_string_list(addr, payload.len()).unwrap(),
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

        mm.write_memory(addr, &payload).unwrap();
        assert_eq!(
            mm.read_nul_delimited_c_string_list(addr, payload.len()).unwrap(),
            expected_parses,
        );
    }

    #[::fuchsia::test]
    async fn test_read_c_string_to_slice() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let page_size = *PAGE_SIZE;
        let mut buf = vec![0u8; 2 * page_size as usize];
        let addr = mm.base_addr + 10 * page_size;

        // Map a page at a fixed address and write an unterminated string at the end of it.
        assert_eq!(map_memory(&current_task, addr, page_size), addr);
        let test_str = b"foo!";
        let test_addr = addr + page_size - test_str.len();
        mm.write_memory(test_addr, test_str).expect("failed to write test string");

        // Expect error if the string is not terminated.
        assert_eq!(
            mm.read_c_string_to_slice(UserCString::new(test_addr), &mut buf),
            error!(ENAMETOOLONG)
        );

        // Expect success if the string is terminated.
        mm.write_memory(addr + (page_size - 1), b"\0").expect("failed to write nul");
        assert_eq!(
            mm.read_c_string_to_slice(UserCString::new(test_addr), &mut buf).unwrap(),
            b"foo"
        );

        // Expect success if the string spans over two mappings.
        assert_eq!(map_memory(&current_task, addr + page_size, page_size), addr + page_size);
        assert_eq!(mm.get_mapping_count(), 2);
        mm.write_memory(addr + (page_size - 1), b"bar\0").expect("failed to write extra chars");
        assert_eq!(
            mm.read_c_string_to_slice(UserCString::new(test_addr), &mut buf).unwrap(),
            b"foobar"
        );

        // Expect error if the string does not fit in the provided buffer.
        assert_eq!(
            mm.read_c_string_to_slice(UserCString::new(test_addr), &mut [0u8; 2]),
            error!(ENAMETOOLONG)
        );

        // Expect error if the address is invalid.
        assert_eq!(mm.read_c_string_to_slice(UserCString::default(), &mut buf), error!(EFAULT));
    }

    #[::fuchsia::test]
    async fn test_unmap_returned_mappings() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let mut released_mappings = vec![];
        let unmap_result =
            mm.state.write().unmap(addr, *PAGE_SIZE as usize, &mut released_mappings);
        assert!(unmap_result.is_ok());
        assert_eq!(released_mappings.len(), 1);
    }

    #[::fuchsia::test]
    async fn test_unmap_returns_multiple_mappings() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let _ = map_memory(&current_task, addr + *PAGE_SIZE, *PAGE_SIZE);

        let mut released_mappings = vec![];
        let unmap_result =
            mm.state.write().unmap(addr, (*PAGE_SIZE * 3) as usize, &mut released_mappings);
        assert!(unmap_result.is_ok());
        assert_eq!(released_mappings.len(), 2);
    }

    /// Maps two pages, then unmaps the first page.
    /// The second page should be re-mapped with a new child COW VMO.
    #[::fuchsia::test]
    async fn test_unmap_beginning() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let original_vmo = {
            let state = mm.state.read();
            let (range, mapping) = state.mappings.get(&addr).expect("mapping");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + (*PAGE_SIZE * 2));
            assert_eq!(mapping.base, addr);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE * 2);
            mapping.vmo.clone()
        };

        assert_eq!(mm.unmap(addr, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The first page should be unmapped.
            assert!(state.mappings.get(&addr).is_none());

            // The second page should be a new child COW VMO.
            let (range, mapping) = state.mappings.get(&(addr + *PAGE_SIZE)).expect("second page");
            assert_eq!(range.start, addr + *PAGE_SIZE);
            assert_eq!(range.end, addr + *PAGE_SIZE * 2);
            assert_eq!(mapping.base, addr + *PAGE_SIZE);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE);
            assert_ne!(original_vmo.get_koid().unwrap(), mapping.vmo.get_koid().unwrap());
        }
    }

    /// Maps two pages, then unmaps the second page.
    /// The first page's VMO should be shrunk.
    #[::fuchsia::test]
    async fn test_unmap_end() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE * 2);

        let original_vmo = {
            let state = mm.state.read();
            let (range, mapping) = state.mappings.get(&addr).expect("mapping");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + (*PAGE_SIZE * 2));
            assert_eq!(mapping.base, addr);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE * 2);
            mapping.vmo.clone()
        };

        assert_eq!(mm.unmap(addr + *PAGE_SIZE, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The second page should be unmapped.
            assert!(state.mappings.get(&(addr + *PAGE_SIZE)).is_none());

            // The first page's VMO should be the same as the original, only shrunk.
            let (range, mapping) = state.mappings.get(&addr).expect("first page");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + *PAGE_SIZE);
            assert_eq!(mapping.base, addr);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE);
            assert_eq!(original_vmo.get_koid().unwrap(), mapping.vmo.get_koid().unwrap());
        }
    }

    /// Maps three pages, then unmaps the middle page.
    /// The last page should be re-mapped with a new COW child VMO.
    /// The first page's VMO should be shrunk,
    #[::fuchsia::test]
    async fn test_unmap_middle() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE * 3);

        let original_vmo = {
            let state = mm.state.read();
            let (range, mapping) = state.mappings.get(&addr).expect("mapping");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + (*PAGE_SIZE * 3));
            assert_eq!(mapping.base, addr);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE * 3);
            mapping.vmo.clone()
        };

        assert_eq!(mm.unmap(addr + *PAGE_SIZE, *PAGE_SIZE as usize), Ok(()));

        {
            let state = mm.state.read();

            // The middle page should be unmapped.
            assert!(state.mappings.get(&(addr + *PAGE_SIZE)).is_none());

            // The first page's VMO should be the same as the original, only shrunk.
            let (range, mapping) = state.mappings.get(&addr).expect("first page");
            assert_eq!(range.start, addr);
            assert_eq!(range.end, addr + *PAGE_SIZE);
            assert_eq!(mapping.base, addr);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE);
            assert_eq!(original_vmo.get_koid().unwrap(), mapping.vmo.get_koid().unwrap());

            // The last page should be a new child COW VMO.
            let (range, mapping) = state.mappings.get(&(addr + *PAGE_SIZE * 2)).expect("last page");
            assert_eq!(range.start, addr + *PAGE_SIZE * 2);
            assert_eq!(range.end, addr + *PAGE_SIZE * 3);
            assert_eq!(mapping.base, addr + *PAGE_SIZE * 2);
            assert_eq!(mapping.vmo_offset, 0);
            assert_eq!(mapping.vmo.get_size().unwrap(), *PAGE_SIZE);
            assert_ne!(original_vmo.get_koid().unwrap(), mapping.vmo.get_koid().unwrap());
        }
    }

    #[::fuchsia::test]
    async fn test_read_write_objects() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;
        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let items_ref = UserRef::<i32>::new(addr);

        let items_written = vec![0, 2, 3, 7, 1];
        mm.write_objects(items_ref, &items_written).expect("Failed to write object array.");

        let items_read = mm
            .read_objects_to_vec(items_ref, items_written.len())
            .expect("Failed to read object array.");

        assert_eq!(items_written, items_read);
    }

    #[::fuchsia::test]
    async fn test_read_write_objects_null() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;
        let items_ref = UserRef::<i32>::new(UserAddress::default());

        let items_written = vec![];
        mm.write_objects(items_ref, &items_written).expect("Failed to write empty object array.");

        let items_read = mm
            .read_objects_to_vec(items_ref, items_written.len())
            .expect("Failed to read empty object array.");

        assert_eq!(items_written, items_read);
    }

    #[::fuchsia::test]
    async fn test_read_object_partial() {
        #[derive(Debug, Default, Copy, Clone, FromZeroes, FromBytes, PartialEq)]
        struct Items {
            val: [i32; 4],
        }

        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;
        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let items_array_ref = UserRef::<i32>::new(addr);

        // Populate some values.
        let items_written = vec![75, 23, 51, 98];
        mm.write_objects(items_array_ref, &items_written).expect("Failed to write object array.");

        // Full read of all 4 values.
        let items_ref = UserRef::<Items>::new(addr);
        let items_read = mm
            .read_object_partial(items_ref, std::mem::size_of::<Items>())
            .expect("Failed to read object");
        assert_eq!(items_written, items_read.val);

        // Partial read of the first two.
        let items_read = mm.read_object_partial(items_ref, 8).expect("Failed to read object");
        assert_eq!(vec![75, 23, 0, 0], items_read.val);

        // The API currently allows reading 0 bytes (this could be re-evaluated) so test that does
        // the right thing.
        let items_read = mm.read_object_partial(items_ref, 0).expect("Failed to read object");
        assert_eq!(vec![0, 0, 0, 0], items_read.val);

        // Size bigger than the object.
        assert_eq!(
            mm.read_object_partial(items_ref, std::mem::size_of::<Items>() + 8),
            error!(EINVAL)
        );

        // Bad pointer.
        assert_eq!(
            mm.read_object_partial(UserRef::<Items>::new(UserAddress::from(1)), 16),
            error!(EFAULT)
        );
    }

    #[::fuchsia::test]
    async fn test_partial_read() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let second_map = map_memory(&current_task, addr + *PAGE_SIZE, *PAGE_SIZE);

        let bytes = vec![0xf; (*PAGE_SIZE * 2) as usize];
        assert!(mm.write_memory(addr, &bytes).is_ok());
        mm.state
            .write()
            .protect(second_map, *PAGE_SIZE as usize, ProtectionFlags::empty())
            .unwrap();
        assert_eq!(
            mm.read_memory_partial_to_vec(addr, bytes.len()).unwrap().len(),
            *PAGE_SIZE as usize,
        );
    }

    fn map_memory_growsdown(current_task: &CurrentTask, length: u64) -> UserAddress {
        map_memory_with_flags(
            current_task,
            UserAddress::default(),
            length,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_GROWSDOWN,
        )
    }

    #[::fuchsia::test]
    async fn test_grow_mapping_empty_mm() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = UserAddress::from(0x100000);

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_inside_mapping() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_write_fault_inside_read_only_mapping() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = do_mmap(
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
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = do_mmap(
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
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory_growsdown(&current_task, *PAGE_SIZE) - *PAGE_SIZE;

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(true));

        // Should see two mappings
        assert_eq!(mm.get_mapping_count(), 2);
    }

    #[::fuchsia::test]
    async fn test_grow_above_mapping() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let addr = map_memory_growsdown(&current_task, *PAGE_SIZE) + *PAGE_SIZE;

        assert_matches!(mm.extend_growsdown_mapping_to_address(addr, false), Ok(false));
    }

    #[::fuchsia::test]
    async fn test_grow_write_fault_below_read_only_mapping() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let mapped_addr = map_memory_growsdown(&current_task, *PAGE_SIZE);

        mm.protect(mapped_addr, *PAGE_SIZE as usize, ProtectionFlags::READ).unwrap();

        assert_matches!(
            mm.extend_growsdown_mapping_to_address(mapped_addr - *PAGE_SIZE, true),
            Ok(false)
        );

        assert_eq!(mm.get_mapping_count(), 1);
    }

    #[::fuchsia::test]
    async fn test_snapshot_paged_memory() {
        use fuchsia_zircon::sys::zx_page_request_command_t::ZX_PAGER_VMO_READ;

        let (kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

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
            let packet = port_clone.wait(zx::Time::INFINITE).expect("wait failed");
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

        let child_vmo = Arc::new(
            vmo.create_child(zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE, 0, VMO_SIZE).unwrap(),
        );

        // Write something to the source VMO.
        vmo.write(b"foo", 0).expect("write failed");

        let prot_flags = ProtectionFlags::READ | ProtectionFlags::WRITE;
        let addr = mm
            .map(
                DesiredAddress::Any,
                child_vmo,
                0,
                VMO_SIZE as usize,
                prot_flags,
                MappingOptions::empty(),
                MappingName::None,
                FileWriteGuardRef(None),
            )
            .expect("map failed");

        let target = create_task(&kernel, "another-task");
        mm.snapshot_to(&target.mm).expect("snapshot_to failed");

        // Make sure it has what we wrote.
        let buf = target.mm.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"foo");

        // Write something to both source and target and make sure they are forked.
        mm.write_memory(addr, b"bar").expect("write_memory failed");

        let buf = target.mm.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"foo");

        target.mm.write_memory(addr, b"baz").expect("write_memory failed");
        let buf = mm.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"bar");

        let buf = target.mm.read_memory_to_vec(addr, 3).expect("read_memory failed");
        assert_eq!(buf, b"baz");

        port.queue(&zx::Packet::from_user_packet(0, 0, zx::UserPacket::from_u8_array([0; 32])))
            .unwrap();
        thread.join().unwrap();
    }

    #[::fuchsia::test]
    async fn test_set_vma_name() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        let vma_name = b"vma name".to_vec();
        current_task.write_memory(name_addr, vma_name.as_bytes()).unwrap();

        let mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        sys_prctl(
            &mut current_task,
            PR_SET_VMA,
            PR_SET_VMA_ANON_NAME as u64,
            mapping_addr.ptr() as u64,
            *PAGE_SIZE,
            name_addr.ptr() as u64,
        )
        .unwrap();

        assert_eq!(current_task.mm.get_mapping_name(mapping_addr).unwrap(), Some(vma_name));
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_adjacent_mappings() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .mm
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let first_mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        let second_mapping_addr = map_memory_with_flags(
            &current_task,
            first_mapping_addr + *PAGE_SIZE,
            *PAGE_SIZE,
            MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS,
        );

        assert_eq!(first_mapping_addr + *PAGE_SIZE, second_mapping_addr);

        sys_prctl(
            &mut current_task,
            PR_SET_VMA,
            PR_SET_VMA_ANON_NAME as u64,
            first_mapping_addr.ptr() as u64,
            2 * *PAGE_SIZE,
            name_addr.ptr() as u64,
        )
        .unwrap();

        {
            let state = current_task.mm.state.read();

            // The name should apply to both mappings.
            let (_, mapping) = state.mappings.get(&first_mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));

            let (_, mapping) = state.mappings.get(&second_mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_beyond_end() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .mm
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr = map_memory(&current_task, UserAddress::default(), 2 * *PAGE_SIZE);

        let second_page = mapping_addr + *PAGE_SIZE;
        current_task.mm.unmap(second_page, *PAGE_SIZE as usize).unwrap();

        // This should fail with ENOMEM since it extends past the end of the mapping into unmapped memory.
        assert_eq!(
            sys_prctl(
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
            let state = current_task.mm.state.read();

            let (_, mapping) = state.mappings.get(&mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_before_start() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .mm
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr = map_memory(&current_task, UserAddress::default(), 2 * *PAGE_SIZE);

        let second_page = mapping_addr + *PAGE_SIZE;
        current_task.mm.unmap(mapping_addr, *PAGE_SIZE as usize).unwrap();

        // This should fail with ENOMEM since the start of the range is in unmapped memory.
        assert_eq!(
            sys_prctl(
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
            let state = current_task.mm.state.read();

            let (_, mapping) = state.mappings.get(&second_page).unwrap();
            assert_eq!(mapping.name, MappingName::None);
        }
    }

    #[::fuchsia::test]
    async fn test_set_vma_name_partial() {
        let (_kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .mm
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr = map_memory(&current_task, UserAddress::default(), 3 * *PAGE_SIZE);

        assert_eq!(
            sys_prctl(
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                (mapping_addr + *PAGE_SIZE).ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Ok(crate::syscalls::SUCCESS)
        );

        // This should split the mapping into 3 pieces with the second piece having the name "foo"
        {
            let state = current_task.mm.state.read();

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
        let (kernel, mut current_task) = create_kernel_and_task();

        let name_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);
        current_task
            .mm
            .write_memory(name_addr, CString::new("foo").unwrap().as_bytes_with_nul())
            .unwrap();

        let mapping_addr = map_memory(&current_task, UserAddress::default(), *PAGE_SIZE);

        assert_eq!(
            sys_prctl(
                &mut current_task,
                PR_SET_VMA,
                PR_SET_VMA_ANON_NAME as u64,
                mapping_addr.ptr() as u64,
                *PAGE_SIZE,
                name_addr.ptr() as u64,
            ),
            Ok(crate::syscalls::SUCCESS)
        );

        let target = create_task(&kernel, "another-task");
        current_task.mm.snapshot_to(&target.mm).expect("snapshot_to failed");

        {
            let state = target.mm.state.read();

            let (_, mapping) = state.mappings.get(&mapping_addr).unwrap();
            assert_eq!(mapping.name, MappingName::Vma("foo".into()));
        }
    }
}
