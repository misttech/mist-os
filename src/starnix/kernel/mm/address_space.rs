// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::mapping::MappingBackingMemory;
use crate::mm::memory::MemoryObject;
use crate::mm::memory_manager::ReleasedMappings;
use crate::mm::{
    DesiredAddress, Mapping, MappingBacking, MappingFlags, MappingName, MemoryManager,
    SelectedAddress, VmsplicePayload, VmsplicePayloadSegment, PAGE_SIZE, VMEX_RESOURCE,
};
use crate::vfs::aio::AioContext;
use fuchsia_inspect_contrib::profile_duration;
use range_map::RangeMap;
use starnix_logging::impossible_error;
use starnix_types::math::round_up_to_system_page_size;
use starnix_types::user_buffer::{UserBuffer, UserBuffers};
use starnix_uapi::errors::Errno;
use starnix_uapi::range_ext::RangeExt;
use starnix_uapi::restricted_aspace::{RESTRICTED_ASPACE_BASE, RESTRICTED_ASPACE_RANGE};
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{errno, error};
use std::mem::MaybeUninit;
use std::ops::{Range, RangeBounds};
use std::sync::Arc;

pub const GUARD_PAGE_COUNT_FOR_GROWSDOWN_MAPPINGS: usize = 256;

pub struct AddressSpace {
    pub(super) base: UserAddress,

    pub(super) length: usize,

    /// The memory mappings currently used by this address space.
    ///
    /// The mappings record which object backs each address.
    pub(super) mappings: RangeMap<UserAddress, Mapping>,

    /// Memory object backing private, anonymous memory allocations in this address space.
    #[cfg(feature = "alternate_anon_allocs")]
    pub(super) private_anonymous: PrivateAnonymousMemoryManager,
}

impl AddressSpace {
    pub fn new(base: UserAddress, length: usize) -> Self {
        #[cfg(feature = "alternate_anon_allocs")]
        // The private anonymous backing memory object extend from the user address 0 up to the
        // highest mappable address. The pages below `user_vmar_info.base` are never mapped, but
        // including them in the memory object makes the math for mapping address to memory object
        // offsets simpler.
        let backing_size = (base.ptr() + length) as u64;

        Self {
            base,
            length,
            mappings: Default::default(),
            #[cfg(feature = "alternate_anon_allocs")]
            private_anonymous: PrivateAnonymousMemoryManager::new(backing_size),
        }
    }

    pub(super) fn total_locked_bytes(&self) -> u64 {
        self.num_locked_bytes(self.address_range())
    }

    fn address_range(&self) -> Range<UserAddress> {
        self.base..self.max_address()
    }

    pub(super) fn max_address(&self) -> UserAddress {
        (self.base + self.length).unwrap()
    }

    pub(super) fn user_address_to_vmar_offset(&self, addr: UserAddress) -> Result<usize, ()> {
        if !self.address_range().contains(&addr) {
            return Err(());
        }
        Ok(addr - self.base)
    }

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
        self.mappings.range(query_range).filter_map(|(range, mapping)| {
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
        let chosen_placement_idx = rand::random_range(0..possible_placements);
        self.pick_placement(length, chosen_placement_idx, subrange)
    }

    pub(super) fn find_next_unused_range_below(
        &self,
        mut upper_bound: UserAddress,
        length: usize,
    ) -> Option<UserAddress> {
        let gap_size = length as u64;

        loop {
            let gap_end = self.mappings.find_gap_end(gap_size, &upper_bound);
            let candidate = gap_end.checked_sub(length)?;

            // Is there a next mapping? If not, the candidate is already good.
            let Some((occupied_range, mapping)) = self.mappings.get(gap_end) else {
                return Some(candidate);
            };
            let occupied_range = mapping.inflate_to_include_guard_pages(occupied_range);
            // If it doesn't overlap, the gap is big enough to fit.
            if occupied_range.start >= gap_end {
                return Some(candidate);
            }
            // If there was a mapping in the way, use the start of that range as the upper bound.
            upper_bound = occupied_range.start;
        }
    }

    // Accept the hint if the range is unused and within the range available for mapping.
    fn is_hint_acceptable(&self, hint_addr: UserAddress, length: usize) -> bool {
        let Some(hint_end) = hint_addr.checked_add(length) else {
            return false;
        };
        if !RESTRICTED_ASPACE_RANGE.contains(&hint_addr.ptr())
            || !RESTRICTED_ASPACE_RANGE.contains(&hint_end.ptr())
        {
            return false;
        };
        self.get_occupied_address_ranges(&(hint_addr..hint_end)).next().is_none()
    }

    pub(super) fn select_address_below(
        &self,
        upper_bound: UserAddress,
        addr: DesiredAddress,
        length: usize,
        flags: MappingFlags,
    ) -> Result<SelectedAddress, Errno> {
        let adjusted_length = round_up_to_system_page_size(length).or_else(|_| error!(ENOMEM))?;

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
                self.find_next_unused_range_below(upper_bound, adjusted_length)
                    .ok_or_else(|| errno!(ENOMEM))?
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

    pub(super) fn validate_addr(&self, addr: DesiredAddress, length: usize) -> Result<(), Errno> {
        if let DesiredAddress::FixedOverwrite(addr) = addr {
            if self.check_has_unauthorized_splits(addr, length) {
                return error!(ENOMEM);
            }
        }
        Ok(())
    }

    // Checks if an operation may be performed over the target mapping that may
    // result in a split mapping.
    //
    // An operation may be forbidden if the target mapping only partially covers
    // an existing mapping with the `MappingOptions::DONT_SPLIT` flag set.
    pub(super) fn check_has_unauthorized_splits(&self, addr: UserAddress, length: usize) -> bool {
        let query_range = addr..addr.saturating_add(length);
        let mut intersection = self.mappings.range(query_range.clone());

        // A mapping is not OK if it disallows splitting and the target range
        // does not fully cover the mapping range.
        let check_if_mapping_has_unauthorized_split =
            |mapping: Option<(&Range<UserAddress>, &Mapping)>| {
                mapping.is_some_and(|(mapping_range, mapping)| {
                    mapping.flags().contains(MappingFlags::DONT_SPLIT)
                        && (mapping_range.start < query_range.start
                            || query_range.end < mapping_range.end)
                })
            };

        // We only check the first and last mappings in the range because naturally,
        // the mappings in the middle are fully covered by the target mapping and
        // won't be split.
        check_if_mapping_has_unauthorized_split(intersection.next())
            || check_if_mapping_has_unauthorized_split(intersection.next_back())
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
    pub(super) fn update_after_unmap(
        &mut self,
        mm: &Arc<MemoryManager>,
        addr: UserAddress,
        length: usize,
        released_mappings: &mut ReleasedMappings,
    ) -> Result<(), Errno> {
        profile_duration!("UpdateAfterUnmap");
        let end_addr = addr.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
        let unmap_range = addr..end_addr;

        #[cfg(feature = "alternate_anon_allocs")]
        {
            for (range, mapping) in self.mappings.range(unmap_range.clone()) {
                // Deallocate any pages in the private, anonymous backing that are now unreachable.
                if let MappingBacking::PrivateAnonymous = self.get_mapping_backing(mapping) {
                    let unmapped_range = &unmap_range.intersect(range);

                    mm.inflight_vmspliced_payloads
                        .handle_unmapping(&self.private_anonymous.backing, unmapped_range)?;

                    self.private_anonymous
                        .zero(unmapped_range.start, unmapped_range.end - unmapped_range.start)?;
                }
            }
            released_mappings.extend(self.mappings.remove(unmap_range));
            return Ok(());
        }

        #[cfg(not(feature = "alternate_anon_allocs"))]
        {
            // Find the private, anonymous mapping that will get its tail cut off by this unmap call.
            let truncated_head = match self.mappings.get(addr) {
                Some((range, mapping)) if range.start != addr && mapping.private_anonymous() => {
                    Some((range.start..addr, mapping.clone()))
                }
                _ => None,
            };

            // Find the private, anonymous mapping that will get its head cut off by this unmap call.
            // A mapping that starts exactly at `end_addr` is excluded since it is not affected by
            // the unmapping.
            let truncated_tail = match self.mappings.get(end_addr) {
                Some((range, mapping))
                    if range.start != end_addr && mapping.private_anonymous() =>
                {
                    Some((end_addr..range.end, mapping.clone()))
                }
                _ => None,
            };

            // Remove the original range of mappings from our map.
            released_mappings.extend(self.mappings.remove(addr..end_addr));

            if let Some((range, mut mapping)) = truncated_tail {
                let MappingBacking::Memory(mut backing) = self.get_mapping_backing(mapping).clone();
                mm.inflight_vmspliced_payloads
                    .handle_unmapping(&backing.memory(), &unmap_range.intersect(&range))?;

                // Create and map a child COW memory object mapping that represents the truncated tail.
                let memory_info = backing.memory().basic_info();
                let child_memory_offset = backing.address_to_offset(range.start);
                let child_length = range.end - range.start;
                let mut child_memory = backing
                    .memory()
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
                backing.set_memory(Arc::new(child_memory));
                backing.set_base(range.start);
                backing.set_memory_offset(0);

                self.map_in_user_vmar(
                    SelectedAddress::FixedOverwrite(range.start),
                    backing.memory(),
                    0,
                    child_length,
                    mapping.flags(),
                    false,
                )?;

                // Replace the mapping with a new one that contains updated memory object handle.
                self.set_mapping_backing(&mut mapping, MappingBacking::Memory(backing));
                self.mappings.insert(range, mapping);
            }

            if let Some((range, mapping)) = truncated_head {
                let MappingBacking::Memory(backing) = self.get_mapping_backing(mapping);
                mm.inflight_vmspliced_payloads
                    .handle_unmapping(backing.memory(), &unmap_range.intersect(&range))?;

                // Resize the memory object of the head mapping, whose tail was cut off.
                let new_mapping_size = (range.end - range.start) as u64;
                let new_memory_size = backing.memory_offset() + new_mapping_size;
                backing
                    .memory()
                    .set_size(new_memory_size)
                    .map_err(MemoryManager::get_errno_for_map_err)?;
            }

            Ok(())
        }
    }

    pub fn num_locked_bytes(&self, range: impl RangeBounds<UserAddress>) -> u64 {
        self.mappings
            .range(range)
            .filter(|(_, mapping)| mapping.flags().contains(MappingFlags::LOCKED))
            .map(|(range, _)| (range.end - range.start) as u64)
            .sum()
    }

    pub(super) fn get_mappings_for_vmsplice(
        &self,
        mm: &Arc<MemoryManager>,
        buffers: &UserBuffers,
    ) -> Result<Vec<Arc<VmsplicePayload>>, Errno> {
        let mut vmsplice_mappings = Vec::new();

        for UserBuffer { mut address, length } in buffers.iter().copied() {
            let mappings = self.get_contiguous_mappings_at(address, length)?;
            for (mapping, length) in mappings {
                let vmsplice_payload = match self.get_mapping_backing(mapping) {
                    MappingBacking::Memory(m) => VmsplicePayloadSegment {
                        addr_offset: address,
                        length,
                        memory: m.memory().clone(),
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

                address = (address + length)?;
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
    pub(super) fn get_contiguous_mappings_at(
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
        let mut mappings = self.mappings.range(addr..end_addr);
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
    pub(super) fn next_mapping_if_growsdown(
        &self,
        addr: UserAddress,
    ) -> Option<(UserAddress, &Mapping)> {
        match self.mappings.find_at_or_after(addr) {
            Some((range, mapping)) => {
                if range.contains(&addr) {
                    // |addr| is already contained within a mapping, nothing to grow.
                    None
                } else if !mapping.flags().contains(MappingFlags::GROWSDOWN) {
                    None
                } else {
                    Some((range.start, mapping))
                }
            }
            None => None,
        }
    }

    /// Reads exactly `bytes.len()` bytes of memory.
    ///
    /// # Parameters
    /// - `addr`: The address to read data from.
    /// - `bytes`: The byte array to read into.
    pub(super) fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            self.read_mapping_memory(
                (addr + bytes_read)?,
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
        match self.get_mapping_backing(mapping) {
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
    pub(super) fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        let mut bytes_read = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_read + len;
            if self
                .read_mapping_memory(
                    (addr + bytes_read)?,
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
    pub(super) fn read_memory_partial_until_null_byte<'a>(
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
    pub(super) fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        profile_duration!("WriteMemory");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            self.write_mapping_memory(
                (addr + bytes_written)?,
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
        match self.get_mapping_backing(mapping) {
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
    pub(super) fn write_memory_partial(
        &self,
        addr: UserAddress,
        bytes: &[u8],
    ) -> Result<usize, Errno> {
        profile_duration!("WriteMemoryPartial");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, bytes.len())? {
            let next_offset = bytes_written + len;
            if self
                .write_mapping_memory(
                    (addr + bytes_written)?,
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

    pub(super) fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        profile_duration!("Zero");
        let mut bytes_written = 0;
        for (mapping, len) in self.get_contiguous_mappings_at(addr, length)? {
            let next_offset = bytes_written + len;
            if self.zero_mapping((addr + bytes_written)?, mapping, len).is_err() {
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

        match self.get_mapping_backing(mapping) {
            MappingBacking::Memory(backing) => backing.zero(addr, length),
            #[cfg(feature = "alternate_anon_allocs")]
            MappingBacking::PrivateAnonymous => self.private_anonymous.zero(addr, length),
        }
    }

    pub fn create_memory_backing(
        &self,
        base: UserAddress,
        memory: Arc<MemoryObject>,
        memory_offset: u64,
    ) -> MappingBacking {
        MappingBacking::Memory(Box::new(MappingBackingMemory::new(base, memory, memory_offset)))
    }

    pub fn get_mapping_backing<'a>(&self, mapping: &'a Mapping) -> &'a MappingBacking {
        mapping.get_backing_internal()
    }

    #[cfg(not(feature = "alternate_anon_allocs"))]
    fn set_mapping_backing(&mut self, mapping: &mut Mapping, backing: MappingBacking) {
        mapping.set_backing_internal(backing);
    }

    pub(super) fn get_aio_context(
        &self,
        addr: UserAddress,
    ) -> Option<(Range<UserAddress>, Arc<AioContext>)> {
        let Some((range, mapping)) = self.mappings.get(addr) else {
            return None;
        };
        let MappingName::AioContext(ref aio_context) = mapping.name() else {
            return None;
        };
        if !mapping.can_read() {
            return None;
        }
        Some((range.clone(), aio_context.clone()))
    }
}

#[cfg(feature = "alternate_anon_allocs")]
pub(super) struct PrivateAnonymousMemoryManager {
    /// Memory object backing private, anonymous memory allocations in this address space.
    pub(super) backing: Arc<MemoryObject>,
}

#[cfg(feature = "alternate_anon_allocs")]
impl PrivateAnonymousMemoryManager {
    fn new(backing_size: u64) -> Self {
        let backing = Arc::new(
            MemoryObject::from(
                zx::Vmo::create(backing_size)
                    .unwrap()
                    .replace_as_executable(&VMEX_RESOURCE)
                    .unwrap(),
            )
            .with_zx_name(b"starnix:memory_manager"),
        );
        Self { backing }
    }

    pub(super) fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.backing.read_uninit(bytes, addr.ptr() as u64).map_err(|_| errno!(EFAULT))
    }

    pub(super) fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<(), Errno> {
        self.backing.write(bytes, addr.ptr() as u64).map_err(|_| errno!(EFAULT))
    }

    pub(super) fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.backing
            .op_range(zx::VmoOp::ZERO, addr.ptr() as u64, length as u64)
            .map_err(|_| errno!(EFAULT))?;
        Ok(length)
    }

    pub(super) fn move_pages(
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

    pub(super) fn snapshot(&self, backing_size: u64) -> Result<Self, Errno> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use itertools::assert_equal;
    use starnix_uapi::restricted_aspace::RESTRICTED_ASPACE_HIGHEST_ADDRESS;
    use starnix_uapi::{MAP_ANONYMOUS, MAP_GROWSDOWN, MAP_PRIVATE};

    #[::fuchsia::test]
    async fn test_count_placements() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

        // ten-page range
        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        assert_eq!(
            mm.state.read().address_space.count_possible_placements(11 * page_size, &subrange_ten),
            Some(0)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(10 * page_size, &subrange_ten),
            Some(1)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(9 * page_size, &subrange_ten),
            Some(2)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(page_size, &subrange_ten),
            Some(10)
        );

        // map 6th page
        let addr = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 5 * page_size);
        assert_eq!(map_memory(locked, &current_task, addr, *PAGE_SIZE), addr);

        assert_eq!(
            mm.state.read().address_space.count_possible_placements(10 * page_size, &subrange_ten),
            Some(0)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(5 * page_size, &subrange_ten),
            Some(1)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(4 * page_size, &subrange_ten),
            Some(3)
        );
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(page_size, &subrange_ten),
            Some(9)
        );
    }

    #[::fuchsia::test]
    async fn test_find_next_unused_range() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

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
        assert_eq!(map_memory(locked, &current_task, addr, *PAGE_SIZE), addr);

        // The next available range is right before the new mapping.
        assert_eq!(
            mm.state.read().find_next_unused_range(page_size).unwrap(),
            UserAddress::from_ptr(addr.ptr() - page_size)
        );

        // Allocate an extra page before a one-page gap.
        let addr2 = UserAddress::from_ptr(addr.ptr() - 2 * page_size);
        assert_eq!(map_memory(locked, &current_task, addr2, *PAGE_SIZE), addr2);

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
    async fn test_pick_placement() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        let addr = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 5 * page_size);
        assert_eq!(map_memory(locked, &current_task, addr, *PAGE_SIZE), addr);
        assert_eq!(
            mm.state.read().address_space.count_possible_placements(4 * page_size, &subrange_ten),
            Some(3)
        );

        assert_eq!(
            mm.state.read().address_space.pick_placement(4 * page_size, 0, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE))
        );
        assert_eq!(
            mm.state.read().address_space.pick_placement(4 * page_size, 1, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + page_size))
        );
        assert_eq!(
            mm.state.read().address_space.pick_placement(4 * page_size, 2, &subrange_ten),
            Some(UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 6 * page_size))
        );
    }

    #[::fuchsia::test]
    async fn test_find_random_unused_range() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

        // ten-page range
        let page_size = *PAGE_SIZE as usize;
        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)
            ..UserAddress::from_ptr(RESTRICTED_ASPACE_BASE + 10 * page_size);

        for _ in 0..10 {
            let addr =
                mm.state.read().address_space.find_random_unused_range(page_size, &subrange_ten);
            assert!(addr.is_some());
            assert_eq!(map_memory(locked, &current_task, addr.unwrap(), *PAGE_SIZE), addr.unwrap());
        }
        assert_eq!(
            mm.state.read().address_space.find_random_unused_range(page_size, &subrange_ten),
            None
        );
    }

    #[::fuchsia::test]
    async fn test_grows_down_near_aspace_base() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

        let page_count = 10;

        let page_size = *PAGE_SIZE as usize;
        let addr =
            (UserAddress::from_ptr(RESTRICTED_ASPACE_BASE) + page_count * page_size).unwrap();
        assert_eq!(
            map_memory_with_flags(
                locked,
                &current_task,
                addr,
                page_size as u64,
                MAP_ANONYMOUS | MAP_PRIVATE | MAP_GROWSDOWN
            ),
            addr
        );

        let subrange_ten = UserAddress::from_ptr(RESTRICTED_ASPACE_BASE)..addr;
        assert_eq!(
            mm.state.read().address_space.find_random_unused_range(page_size, &subrange_ten),
            None
        );
    }

    #[::fuchsia::test]
    async fn test_get_contiguous_mappings_at() {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let mm = current_task.mm().unwrap();

        // Create four one-page mappings with a hole between the third one and the fourth one.
        let page_size = *PAGE_SIZE as usize;
        let addr_a = (mm.base_addr + 10 * page_size).unwrap();
        let addr_b = (mm.base_addr + 11 * page_size).unwrap();
        let addr_c = (mm.base_addr + 12 * page_size).unwrap();
        let addr_d = (mm.base_addr + 14 * page_size).unwrap();
        assert_eq!(map_memory(locked, &current_task, addr_a, *PAGE_SIZE), addr_a);
        assert_eq!(map_memory(locked, &current_task, addr_b, *PAGE_SIZE), addr_b);
        assert_eq!(map_memory(locked, &current_task, addr_c, *PAGE_SIZE), addr_c);
        assert_eq!(map_memory(locked, &current_task, addr_d, *PAGE_SIZE), addr_d);

        {
            let mm_state = mm.state.read();
            // Verify that requesting an unmapped address returns an empty iterator.
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a - 100u64).unwrap(), 50)
                    .unwrap(),
                vec![],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a - 100u64).unwrap(), 200)
                    .unwrap(),
                vec![],
            );

            // Verify that requesting zero bytes returns an empty iterator.
            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_a, 0).unwrap(),
                vec![],
            );

            // Verify errors.
            assert_eq!(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at(UserAddress::from(100), usize::MAX)
                    .err()
                    .unwrap(),
                errno!(EFAULT)
            );
            assert_eq!(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at(
                        (mm_state.address_space.max_address() + 1u64).unwrap(),
                        0
                    )
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
                let mut it = mm_state.address_space.mappings.iter();
                (it.next().unwrap().1, it.next().unwrap().1)
            };

            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_a, page_size).unwrap(),
                vec![(map_a, page_size)],
            );

            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_a, page_size / 2).unwrap(),
                vec![(map_a, page_size / 2)],
            );

            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_a, page_size * 3).unwrap(),
                vec![(map_a, page_size * 3)],
            );

            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_b, page_size).unwrap(),
                vec![(map_a, page_size)],
            );

            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_d, page_size).unwrap(),
                vec![(map_b, page_size)],
            );

            // Verify that results stop if there is a hole.
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 2).unwrap(), page_size * 10)
                    .unwrap(),
                vec![(map_a, page_size * 2 + page_size / 2)],
            );

            // Verify that results stop at the last mapped page.
            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_d, page_size * 10).unwrap(),
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
                mm_state.address_space.get_contiguous_mappings_at(addr_a, page_size).unwrap(),
                vec![(map_a, page_size)],
            );
            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_a, page_size / 2).unwrap(),
                vec![(map_a, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 2).unwrap(), page_size / 2)
                    .unwrap(),
                vec![(map_a, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 4).unwrap(), page_size / 8)
                    .unwrap(),
                vec![(map_a, page_size / 8)],
            );

            // Verify result when requesting a range spanning more than one mapping.
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 2).unwrap(), page_size)
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at(
                        (addr_a + page_size / 2).unwrap(),
                        page_size * 3 / 2,
                    )
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at(addr_a, page_size * 3 / 2)
                    .unwrap(),
                vec![(map_a, page_size), (map_b, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 2).unwrap(), page_size * 2)
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size), (map_c, page_size / 2)],
            );
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at(
                        (addr_b + page_size / 2).unwrap(),
                        page_size * 3 / 2,
                    )
                    .unwrap(),
                vec![(map_b, page_size / 2), (map_c, page_size)],
            );

            // Verify that results stop if there is a hole.
            assert_equal(
                mm_state
                    .address_space
                    .get_contiguous_mappings_at((addr_a + page_size / 2).unwrap(), page_size * 10)
                    .unwrap(),
                vec![(map_a, page_size / 2), (map_b, page_size), (map_c, page_size)],
            );

            // Verify that results stop at the last mapped page.
            assert_equal(
                mm_state.address_space.get_contiguous_mappings_at(addr_d, page_size * 10).unwrap(),
                vec![(map_d, page_size)],
            );
        }
    }
}
