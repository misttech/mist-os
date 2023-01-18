// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/102872): Remove.
#![allow(unused_variables, dead_code)]

use {
    crate::hypervisor::Hypervisor,
    fidl_fuchsia_virtualization::{GuestConfig, GuestError},
    fuchsia_zircon as zx,
    lazy_static::lazy_static,
    thiserror::Error,
};

lazy_static! {
    static ref PAGE_SIZE: GuestUsize = zx::system_get_page_size().into();
}

const ONE_KIB: u64 = 1 << 10;
const ONE_MIB: u64 = 1 << 20;
const ONE_GIB: u64 = 1 << 30;

// The physical address where the VMM starts placing memory mapped devices such as PCI devices. For
// x86 this is colloquially referred to as the "PCI hole", and for our ARM64 machine this is the
// start of the GIC.
#[cfg(target_arch = "x86_64")]
const DEVICE_PHYS_BASE: GuestAddress = 0xf8000000;
#[cfg(target_arch = "aarch64")]
const DEVICE_PHYS_BASE: GuestAddress = 0x800000000;

// For devices that can have their addresses anywhere we run a dynamic allocator that starts
// fairly high in the guest physical address space.
const FIRST_DYNAMIC_DEVICE_ADDR: GuestAddress = 0xb00000000;

// Arbitrarily large number used when restricting guest memory ranges. If a restricted range
// has this size, it means "restrict from the base address until +INF".
const ALL_REMAINING_MEMORY_RANGE: GuestAddress = 1 << 52;

#[cfg(target_arch = "aarch64")]
const fn const_min(a: GuestAddress, b: GuestAddress) -> GuestAddress {
    // std::cmp::min is not stable as a const function (see #![feature(const_cmp)]).
    if a <= b {
        a
    } else {
        b
    }
}

#[cfg(target_arch = "aarch64")]
const RESTRICTED_RANGES: &'static [MemoryRegion] = &[
    // For ARM PCI devices are mapped in at a relatively high address, so it's reasonable to just
    // block off the rest of guest memory.
    MemoryRegion::new_until_end(const_min(DEVICE_PHYS_BASE, FIRST_DYNAMIC_DEVICE_ADDR)),
];

#[cfg(target_arch = "x86_64")]
const RESTRICTED_RANGES: &'static [MemoryRegion] = &[
    // Reserve regions in the first MiB for use by the BIOS.
    MemoryRegion::new_from_range(0x0, 32 * ONE_KIB),
    MemoryRegion::new_from_range(512 * ONE_KIB, ONE_MIB),
    // For x86 PCI devices are mapped in somewhere below 4 GiB, and the range extends to 4 GiB.
    MemoryRegion::new_from_range(DEVICE_PHYS_BASE, 4 * ONE_GIB),
    // Dynamic devices are mapped in at a very high address, so everything beyond that point
    // can be blocked off.
    MemoryRegion::new_until_end(FIRST_DYNAMIC_DEVICE_ADDR),
];

const fn check_no_overlapping_restricted_regions() -> bool {
    // Note that iterators are not const compatible, see RFC 2344.
    let mut i = 0;
    while i < RESTRICTED_RANGES.len() - 1 {
        let mut j = i + 1;
        while j < RESTRICTED_RANGES.len() {
            if RESTRICTED_RANGES[i].has_overlap(&RESTRICTED_RANGES[j]) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }

    true
}

// Restricted regions shouldn't overlap. If adding a region that overlaps with another, just
// merge them into one larger region.
static_assertions::const_assert!(check_no_overlapping_restricted_regions());

const fn check_restricted_regions_are_sorted() -> bool {
    // Note that iterators are not const compatible, see RFC 2344.
    let mut i = 0;
    while i < RESTRICTED_RANGES.len() - 1 {
        if !MemoryRegion::compare_min_by_base(&RESTRICTED_RANGES[i], &RESTRICTED_RANGES[i + 1]) {
            return false;
        }
        i += 1;
    }

    true
}

// Restricted regions should be sorted based on an incrementing base address.
static_assertions::const_assert!(check_restricted_regions_are_sorted());

#[derive(PartialEq, Debug, Error)]
pub enum MemoryError {
    #[error("Guest config didn't specify a memory size")]
    NoMemoryRequested,

    #[error(
        "Unable to allocate total memory due to layout restrictions. Allocated {} of {} bytes.",
        actual,
        target
    )]
    FailedToPlaceGuestMemory { actual: u64, target: u64 },

    #[error("Failed to allocate child VMAR in host process: {}", status)]
    FailedToAllocateVmar { status: zx::Status },

    #[error("Failed to map region into VMAR: {}", status)]
    FailedToMapMemoryRegion { status: zx::Status },

    #[error("Failed to allocate memory: {}", status)]
    FailedToAllocateMemory { status: zx::Status },

    #[error("Device {} registered with a zero length range", tag)]
    ZeroDeviceSize { tag: String },

    #[error("Address region {} overlaps with region {}", tag1, tag2)]
    AddressConflict { tag1: String, tag2: String },
}

impl Into<GuestError> for MemoryError {
    fn into(self) -> GuestError {
        match self {
            MemoryError::NoMemoryRequested => GuestError::BadConfig,
            MemoryError::FailedToPlaceGuestMemory { .. } => GuestError::GuestInitializationFailure,
            MemoryError::FailedToMapMemoryRegion { .. }
            | MemoryError::FailedToAllocateVmar { .. }
            | MemoryError::FailedToAllocateMemory { .. }
            | MemoryError::ZeroDeviceSize { .. } => GuestError::InternalError,
            MemoryError::AddressConflict { .. } => GuestError::DeviceMemoryOverlap,
        }
    }
}

// TODO(fxbug.dev/102872): Replace with the implementation from vm-memory.
struct GuestMemoryMmap;
pub type GuestAddress = u64;
pub type GuestUsize = u64;

// Get the size of the guest physical memory address space. In most cases this will be larger
// than the amount of RAM the guest actually sees, as this would include MMIO device ranges
// (if encompassed by RAM), and pluggable memory regions.
fn get_guest_physical_address_space_size(
    standard_ram: &Vec<MemoryRegion>,
    pluggable_ram: &Vec<MemoryRegion>,
) -> GuestUsize {
    let region = if let Some(last) = pluggable_ram.last() {
        last
    } else {
        standard_ram.last().expect("there must be at least one region of RAM")
    };

    region.base + region.size
}

// Attempts to place `guest_memory` bytes into the guest physical address space, avoiding
// restricted regions. Restricted regions are architecture dependent, and include things such
// as memory mapped devices and the BIOS registers.
fn generate_guest_ram_regions(
    guest_memory: GuestUsize,
    restrictions: &[MemoryRegion],
) -> Result<Vec<MemoryRegion>, MemoryError> {
    let mut iter = GuestMemoryRegion::new(restrictions);
    let mut regions = Vec::new();
    let mut mem_remaining = guest_memory;

    while mem_remaining > 0 {
        if let Some(current) = iter.next() {
            let mem_used = std::cmp::min(current.size, mem_remaining);
            regions.push(MemoryRegion::new_tagged(current.base, mem_used, "RAM".to_string()));
            mem_remaining -= mem_used;
        } else {
            return Err(MemoryError::FailedToPlaceGuestMemory {
                actual: mem_remaining,
                target: guest_memory,
            });
        }
    }

    Ok(regions)
}

// Page align memory if necessary, rounding up to the nearest page.
fn page_align_memory(memory: GuestAddress, tag: &str) -> GuestAddress {
    let page_alignment = memory % *PAGE_SIZE;
    if page_alignment != 0 {
        let padding = *PAGE_SIZE - page_alignment;
        tracing::info!(
            "Memory ({}b) for {} is not a multiple of page size ({}b), so increasing by {}b.",
            memory,
            tag,
            *PAGE_SIZE,
            padding
        );
        memory + padding
    } else {
        memory
    }
}

// Page aligns a memory region. The region is reduced when page aligning to avoid encroaching
// on adjacent restricted regions.
fn page_align_memory_region(mut region: MemoryRegion) -> Option<MemoryRegion> {
    if region.size < *PAGE_SIZE {
        // This guest region may be bounded by restricted regions, so size cannot be
        // increased. If this region is smaller than a page this region must just be
        // discarded.
        return None;
    }

    let mut start = region.base;
    if start % *PAGE_SIZE != 0 {
        start += *PAGE_SIZE - (start % *PAGE_SIZE);
    }

    let mut end = start + region.size;
    if end % *PAGE_SIZE != 0 {
        end -= end % *PAGE_SIZE;
    }

    // Require this region be at least a page.
    if start >= end {
        return None;
    }

    region.base = start;
    region.size = end - start;

    Some(region)
}

// x86 has reserved memory from 0 to 32KiB, and 512KiB to 1MiB. While we will not allocate guest
// RAM in those regions (via the e820 memory map), we still want to map these regions into the
// VMAR as they are not devices and we do not wish to trap on guest access to them.
fn architecture_dependent_non_ram_vmar_regions() -> Vec<MemoryRegion> {
    if cfg!(target_arch = "x86_64") {
        vec![
            MemoryRegion::new_from_range(0x0, 32 * ONE_KIB),
            MemoryRegion::new_from_range(512 * ONE_KIB, ONE_MIB),
        ]
    } else {
        vec![]
    }
}

#[derive(Debug, Clone, PartialEq)]
struct MemoryRegion {
    // Base address of a region of guest physical address space.
    base: GuestAddress,
    // Size of a region of guest physical address space in bytes.
    size: GuestUsize,
    // Optional short human readable tag for debugging this memory region.
    tag: Option<String>,
}

impl MemoryRegion {
    const fn new(base: GuestAddress, size: GuestUsize) -> Self {
        assert!(base < ALL_REMAINING_MEMORY_RANGE);
        MemoryRegion { base, size, tag: None }
    }

    fn new_tagged(base: GuestAddress, size: GuestUsize, tag: String) -> Self {
        assert!(base < ALL_REMAINING_MEMORY_RANGE);
        MemoryRegion { base, size, tag: Some(tag) }
    }

    // Helper function to create a memory region from a specific start and end address.
    const fn new_from_range(start: GuestAddress, end: GuestAddress) -> Self {
        assert!(start < ALL_REMAINING_MEMORY_RANGE);
        MemoryRegion::new(start, end - start)
    }

    // Helper function to create a memory region containing all remaining guest address space.
    const fn new_until_end(start: GuestAddress) -> Self {
        MemoryRegion::new(start, ALL_REMAINING_MEMORY_RANGE)
    }

    // Returns true if this memory region has any overlap with another memory region.
    const fn has_overlap(&self, other: &MemoryRegion) -> bool {
        let (begin, end) = if MemoryRegion::compare_min_by_base(self, other) {
            (self, other)
        } else {
            (other, self)
        };
        begin.base + begin.size > end.base
    }

    // Returns true if lhs is less than rhs, determined by the lower base address.
    const fn compare_min_by_base(lhs: &MemoryRegion, rhs: &MemoryRegion) -> bool {
        lhs.base < rhs.base
    }
}

struct GuestMemoryRegion<'a> {
    iter: std::iter::Peekable<std::slice::Iter<'a, MemoryRegion>>,
    first_region: bool,
}

impl<'a> GuestMemoryRegion<'a> {
    fn new(restrictions: &'a [MemoryRegion]) -> Self {
        GuestMemoryRegion { iter: restrictions.iter().peekable(), first_region: true }
    }
}

impl<'a> Iterator for GuestMemoryRegion<'a> {
    type Item = MemoryRegion;

    // Gets a region of address space which is eligible to contain RAM. If this returns None,
    // there are no remaining unrestricted regions.
    fn next(&mut self) -> Option<Self::Item> {
        let unaligned = if self.first_region {
            self.first_region = false;
            if let Some(restriction) = self.iter.peek() {
                if restriction.base != 0 {
                    MemoryRegion::new(0, restriction.base)
                } else {
                    return self.next();
                }
            } else {
                // Special case where there's no restrictions. Currently this isn't true for any
                // production architecture due to the need to assign dynamic device addresses.
                MemoryRegion::new(0, ALL_REMAINING_MEMORY_RANGE)
            }
        } else {
            let restriction = self.iter.next().expect(
                "there must be at least one remaining restriction or the last region would have \
                    contained all available guest memory",
            );
            if restriction.size == ALL_REMAINING_MEMORY_RANGE {
                // No remaining valid guest memory regions.
                return None;
            }

            // The unrestricted region being constructed extends from the end of the current
            // restriction to the start of the next restriction, or if this is the last restriction
            // it extends to an arbitrarily large address.
            let unrestricted_base = restriction.base + restriction.size;
            let unrestricted_size = if let Some(next_restriction) = self.iter.peek() {
                next_restriction.base - unrestricted_base
            } else {
                ALL_REMAINING_MEMORY_RANGE
            };

            MemoryRegion::new(unrestricted_base, unrestricted_size)
        };

        page_align_memory_region(unaligned).or_else(|| self.next())
    }
}

pub struct Memory<H: Hypervisor> {
    hypervisor: H,
    vmo: zx::Vmo,
    host_vmar: H::AddressSpaceHandle,
    host_vmar_offset: GuestAddress,
    mmap: Option<GuestMemoryMmap>,
    next_dynamic_device_address: GuestAddress,
    ram_regions: Vec<MemoryRegion>,
    pluggable_ram_regions: Vec<MemoryRegion>,
    device_ranges: Vec<MemoryRegion>,
}

impl<H: Hypervisor> std::ops::Drop for Memory<H> {
    fn drop(&mut self) {
        // A note on safety:
        //  - The guest must be stopped before this object is destroyed.
        //  - All devices accessing this physical memory should be stopped before this object is
        //    is destroyed.
        //
        // The VMO underlying the guest physical memory may still be valid if the handle was
        // passed to other device components, but traps accessing device memory via the VMM
        // host address space will fault.
        unsafe {
            self.hypervisor
                .vmar_destroy(&self.host_vmar)
                .expect("failed to destroy host process child VMAR");
        }
    }
}

impl<H: Hypervisor> Memory<H> {
    pub fn new_from_config(guest_config: &GuestConfig, hypervisor: H) -> Result<Self, MemoryError> {
        Self::new(guest_config.guest_memory.ok_or(MemoryError::NoMemoryRequested)?, hypervisor)
    }

    fn new(guest_memory_bytes: GuestUsize, hypervisor: H) -> Result<Self, MemoryError> {
        let guest_memory_bytes = page_align_memory(guest_memory_bytes, "total_guest_memory");

        let ram_regions = generate_guest_ram_regions(guest_memory_bytes, &RESTRICTED_RANGES)?;
        // TODO(fxbug.dev/102872): Support pluggable memory.
        let pluggable_ram_regions = Vec::new();

        let physical_address_size =
            get_guest_physical_address_space_size(&ram_regions, &pluggable_ram_regions);

        let vmo = hypervisor
            .allocate_memory(physical_address_size)
            .map_err(|status| MemoryError::FailedToAllocateMemory { status })?;

        let (host_vmar, host_vmar_offset) = hypervisor
            .vmar_allocate(physical_address_size)
            .map_err(|status| MemoryError::FailedToAllocateVmar { status })?;

        Ok(Memory {
            hypervisor,
            vmo,
            host_vmar,
            host_vmar_offset: host_vmar_offset.try_into().expect("usize should fit into u64"),
            mmap: None,
            next_dynamic_device_address: FIRST_DYNAMIC_DEVICE_ADDR,
            ram_regions,
            pluggable_ram_regions,
            device_ranges: Vec::new(),
        })
    }

    // Allocate a device memory range, and register it to allow checking for overlaps with
    // manually registered ranges.
    pub fn allocate_dynamic_device_address(
        &mut self,
        size: GuestUsize,
        tag: String,
    ) -> Result<GuestAddress, MemoryError> {
        let size = page_align_memory(size, tag.as_str());
        let address = self.next_dynamic_device_address;
        self.next_dynamic_device_address += size;
        self.add_device_range(address, size, tag)?;

        Ok(address)
    }

    // Register a device memory range, checking for overlaps with other devices and guest memory.
    pub fn add_device_range(
        &mut self,
        start: GuestAddress,
        size: GuestUsize,
        tag: String,
    ) -> Result<(), MemoryError> {
        if size == 0 {
            return Err(MemoryError::ZeroDeviceSize { tag });
        }

        let region = MemoryRegion::new_tagged(start, size, tag);
        if let Some(conflict) = self.device_ranges.iter().find(|&x| x.has_overlap(&region)) {
            return Err(MemoryError::AddressConflict {
                tag1: conflict.tag.as_ref().expect("address regions must be tagged").clone(),
                tag2: region.tag.expect("address regions must be tagged"),
            });
        }

        if let Some(conflict) = self.ram_regions.iter().find(|&x| x.has_overlap(&region)) {
            return Err(MemoryError::AddressConflict {
                tag1: conflict.tag.as_ref().expect("address regions must be tagged").clone(),
                tag2: region.tag.expect("address regions must be tagged"),
            });
        }

        self.device_ranges.push(region);
        Ok(())
    }

    // Map the generated memory regions into the local process address space.
    pub fn map_into_host(&self) -> Result<(), MemoryError> {
        // TODO(fxbug.dev/102872): Create a vm-memory::GuestMemoryMmap.
        assert!(self.mmap.is_none());

        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::PERM_WRITE
            | zx::VmarFlags::SPECIFIC
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
        self.map_into_vmar(&self.host_vmar, flags)
    }

    // Map the generated memory regions into the guest address space.
    pub fn map_into_guest(&self, guest_vmar: &H::AddressSpaceHandle) -> Result<(), MemoryError> {
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::PERM_WRITE
            | zx::VmarFlags::PERM_EXECUTE
            | zx::VmarFlags::SPECIFIC
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
        self.map_into_vmar(guest_vmar, flags)
    }

    // Helper function to map non-device regions into a VMAR.
    fn map_into_vmar(
        &self,
        vmar: &H::AddressSpaceHandle,
        flags: zx::VmarFlags,
    ) -> Result<(), MemoryError> {
        let additional_regions = architecture_dependent_non_ram_vmar_regions();
        let regions = self
            .ram_regions
            .iter()
            .chain(self.pluggable_ram_regions.iter())
            .chain(additional_regions.iter());

        for region in regions {
            if let Err(status) = self.hypervisor.vmar_map(
                vmar,
                region.base.try_into().expect("u64 should fit into usize"),
                &self.vmo,
                region.base,
                region.size.try_into().expect("u64 should fit into usize"),
                flags,
            ) {
                return Err(MemoryError::FailedToMapMemoryRegion { status });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hypervisor::testing::{MockBehavior, MockHypervisor, MockMemoryMapping},
    };

    #[fuchsia::test]
    async fn failed_to_allocate_guest_memory_vmo() {
        let hypervisor = MockHypervisor::new();
        hypervisor.on_allocate_memory(MockBehavior::ReturnError(zx::Status::NO_RESOURCES));

        let mut config = GuestConfig::EMPTY;
        config.guest_memory = Some(*PAGE_SIZE);

        let memory = Memory::new_from_config(&config, hypervisor.clone());
        assert_eq!(
            MemoryError::FailedToAllocateMemory { status: zx::Status::NO_RESOURCES },
            memory.err().unwrap()
        );
    }

    #[fuchsia::test]
    async fn read_write_vmar_mapping() {
        let hypervisor = MockHypervisor::new();
        let memory = Memory::new(1 << 32, hypervisor.clone()).unwrap();

        // Both the VMO and VMAR should equal all of guest physical memory.
        let info = memory.host_vmar.borrow().inner.info().unwrap();
        let last_region = memory.ram_regions.last().unwrap();
        assert_eq!(info.len as u64, last_region.base + last_region.size);
        assert_eq!(memory.vmo.get_size().unwrap(), last_region.base + last_region.size);

        memory.map_into_host().unwrap();

        // Write a known string into the first 6 bytes of each mapped memory region.
        let expected = "GOOGLE".as_bytes().to_vec();
        for region in memory.ram_regions.iter() {
            let slice = unsafe {
                let base = memory.host_vmar_offset as *mut u8;
                let offset = base.add(region.base as usize);
                std::slice::from_raw_parts_mut(offset, 6)
            };
            slice.copy_from_slice(&expected);
        }

        // Read the string for each region from the VMO.
        for region in memory.ram_regions.iter() {
            let mut actual = [0u8; 6];
            memory.vmo.read(&mut actual, region.base).unwrap();
            assert_eq!(actual, expected.as_slice());
        }

        // Write a known string into the first 6 bytes of each region via the VMO.
        let expected = "ELGOOG".as_bytes().to_vec();
        for region in memory.ram_regions.iter() {
            memory.vmo.write(&expected, region.base).unwrap();
        }

        // Read the string from the mapped memory regions.
        for region in memory.ram_regions.iter() {
            let actual = unsafe {
                let base = memory.host_vmar_offset as *mut u8;
                let offset = base.add(region.base as usize);
                std::slice::from_raw_parts(offset, 6)
            };
            assert_eq!(actual, expected.as_slice());
        }

        // Check the mapped values stored via the mock.
        let additional = architecture_dependent_non_ram_vmar_regions();
        let expected: Vec<_> = memory
            .ram_regions
            .iter()
            .chain(additional.iter())
            .map(|elem| MockMemoryMapping {
                vmar_offset: elem.base.try_into().expect("u64 should fit into usize"),
                vmo_offset: elem.base,
                length: elem.size.try_into().expect("u64 should fit into usize"),
                flags: zx::VmarFlags::PERM_READ
                    | zx::VmarFlags::PERM_WRITE
                    | zx::VmarFlags::SPECIFIC
                    | zx::VmarFlags::REQUIRE_NON_RESIZABLE,
            })
            .collect();
        assert_eq!(memory.host_vmar.borrow().mappings, expected);
    }

    #[fuchsia::test]
    async fn page_align_total_guest_memory() {
        // Round up to the nearest page.
        assert_eq!(page_align_memory(1, "test"), *PAGE_SIZE);
        assert_eq!(page_align_memory(*PAGE_SIZE - 1, "test"), *PAGE_SIZE);
        assert_eq!(page_align_memory(*PAGE_SIZE, "test"), *PAGE_SIZE);
        assert_eq!(page_align_memory(*PAGE_SIZE + 1, "test"), *PAGE_SIZE * 2);
    }

    #[fuchsia::test]
    async fn page_align_guest_memory_region() {
        // Page aligned.
        let actual = page_align_memory_region(MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE)).unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE);
        assert_eq!(actual, expected);

        // End is not page aligned, so round it down.
        let actual = page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE,
            *PAGE_SIZE * 3 + *PAGE_SIZE / 2,
        ))
        .unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE * 3);
        assert_eq!(actual, expected);

        // Start is not page aligned, so round it up (note that the second field is size, not the
        // ending address which is why it will also change).
        let actual = page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE / 2,
            *PAGE_SIZE * 3 + *PAGE_SIZE / 2,
        ))
        .unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE * 3);
        assert_eq!(actual, expected);

        // After page aligning this is a zero length region, and is thus dropped.
        assert!(
            page_align_memory_region(MemoryRegion::new(*PAGE_SIZE / 2, *PAGE_SIZE / 2)).is_none()
        );

        // After page aligning this would be a negative length region, and is thus dropped.
        assert!(
            page_align_memory_region(MemoryRegion::new(*PAGE_SIZE / 2, *PAGE_SIZE / 4)).is_none()
        );
    }

    #[fuchsia::test]
    async fn page_aligning_guest_memory_regions_gives_correct_total_memory() {
        // Restrict memory between page 2 1/2 and page 4 1/2. This should result in guest memory
        // placed in pages [0, 1], and pages [5, 7] (giving 5 pages in total, the target).
        let target_memory = *PAGE_SIZE * 5;
        let restrictions = [MemoryRegion::new(*PAGE_SIZE * 2 + *PAGE_SIZE / 2, *PAGE_SIZE * 2)];

        let actual = generate_guest_ram_regions(target_memory, &restrictions).unwrap();
        let expected = [
            MemoryRegion::new(0, *PAGE_SIZE * 2),
            MemoryRegion::new(*PAGE_SIZE * 5, *PAGE_SIZE * 3),
        ];

        assert!(actual
            .iter()
            .zip(expected.iter())
            .all(|(a, b)| a.base == b.base && a.size == b.size));
    }

    #[fuchsia::test]
    async fn regions_smaller_than_a_page_are_dropped() {
        // Restrict the lower half of page 0, and all of page 1. The region of page 0 1/2 to page 1
        // is less than a page, so this results in a single 5 page region starting at end of the
        // second restriction.
        let target_memory = *PAGE_SIZE * 5;
        let restrictions =
            [MemoryRegion::new(0, *PAGE_SIZE / 2), MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE)];

        let actual = generate_guest_ram_regions(target_memory, &restrictions).unwrap();
        let expected = [MemoryRegion::new(*PAGE_SIZE * 2, *PAGE_SIZE * 5)];

        assert!(actual
            .iter()
            .zip(expected.iter())
            .all(|(a, b)| a.base == b.base && a.size == b.size));
    }

    #[fuchsia::test]
    async fn get_guest_memory_regions() {
        // Four GiB of guest memory will extend beyond the PCI device region for x86, but not for arm64.
        let target_memory = page_align_memory(1 << 32, "test");
        let actual = generate_guest_ram_regions(target_memory, &RESTRICTED_RANGES).unwrap();

        #[cfg(target_arch = "x86_64")]
        let expected = [
            MemoryRegion::new(0x8000, 0x78000), // 32 KiB to 512 KiB
            MemoryRegion::new(0x100000, 0xf8000000 - 0x100000), // 1 MiB to start of the PCI device region
            MemoryRegion::new(0x100000000, target_memory - (0xf8000000 - 0x100000) - 0x78000), // Remaining memory
        ];

        #[cfg(target_arch = "aarch64")]
        let expected = [
            MemoryRegion::new(0x0, target_memory), // All memory in one region
        ];

        assert!(actual
            .iter()
            .zip(expected.iter())
            .all(|(a, b)| a.base == b.base && a.size == b.size));
    }

    #[fuchsia::test]
    async fn too_much_guest_memory_requested_to_place_with_restrictions() {
        // The first dynamic device address restriction extends to +INF, so requesting enough
        // memory to overlap with the dynamic device range will always fail.
        let target_memory = page_align_memory(FIRST_DYNAMIC_DEVICE_ADDR + 0x1000, "test");
        let err = generate_guest_ram_regions(target_memory, &RESTRICTED_RANGES).unwrap_err();

        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&MemoryError::FailedToPlaceGuestMemory { actual: 0, target: 0 })
        );
    }

    #[fuchsia::test]
    async fn page_align_allocated_device_memory_range() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, hypervisor.clone()).unwrap();

        let device1 =
            memory.allocate_dynamic_device_address(*PAGE_SIZE / 2, "a".to_string()).unwrap();
        let device2 = memory.allocate_dynamic_device_address(*PAGE_SIZE, "b".to_string()).unwrap();

        // Device 1 didn't request a full page, but device 2 will still be paged aligned.
        assert_eq!(device1 + *PAGE_SIZE, device2);
        assert_eq!(device1, page_align_memory(device1, "test"));
        assert_eq!(device2, page_align_memory(device2, "test"));
    }

    #[fuchsia::test]
    async fn device_memory_not_overlapping() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, hypervisor.clone()).unwrap();

        let devices = 0xc000000;
        memory.add_device_range(devices, 0x1000, "a".to_string()).unwrap();
        memory.add_device_range(devices - 0x1000, 0x500, "b".to_string()).unwrap();
        memory.add_device_range(devices - 0x500, 0x500, "c".to_string()).unwrap();
        memory.add_device_range(devices + 0x1000, 0x1000, "d".to_string()).unwrap();
    }

    #[fuchsia::test]
    async fn device_memory_overlap() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, hypervisor.clone()).unwrap();

        let device_addr = memory.allocate_dynamic_device_address(0x1000, "a".to_string()).unwrap();

        // One byte overlap.
        assert_eq!(
            memory.add_device_range(device_addr + 0xFFF, 0x1000, "b".to_string()).err().unwrap(),
            MemoryError::AddressConflict { tag1: "a".to_string(), tag2: "b".to_string() }
        );

        // Overlapping range was not registered, so this range (which would overlap with the
        // previous overlapping range but not the first range) can be added.
        memory.add_device_range(device_addr + 0x1000, 0x1000, "c".to_string()).unwrap();
    }

    #[fuchsia::test]
    async fn device_memory_invalid_size() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, hypervisor.clone()).unwrap();

        assert_eq!(
            memory.add_device_range(0x10000, 0x0, "a".to_string()).err().unwrap(),
            MemoryError::ZeroDeviceSize { tag: "a".to_string() }
        );

        assert_eq!(
            memory.allocate_dynamic_device_address(0x0, "b".to_string()).err().unwrap(),
            MemoryError::ZeroDeviceSize { tag: "b".to_string() }
        );
    }

    #[fuchsia::test]
    async fn device_memory_has_guest_memory_overlap() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(1 << 30, hypervisor.clone()).unwrap();

        assert_eq!(
            memory
                .add_device_range(memory.ram_regions[0].base, 0x1000, "a".to_string())
                .err()
                .unwrap(),
            MemoryError::AddressConflict { tag1: "RAM".to_string(), tag2: "a".to_string() }
        );
    }
}
