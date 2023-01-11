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
            | MemoryError::FailedToAllocateMemory { .. }
            | MemoryError::ZeroDeviceSize { .. } => GuestError::InternalError,
            MemoryError::AddressConflict { .. } => GuestError::DeviceMemoryOverlap,
        }
    }
}

// TODO(fxbug.dev/102872): Replace with the implementation from vm-memory.
struct GuestMemoryMmap;
type GuestAddress = u64;
type GuestUsize = u64;

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

        Memory::page_align_memory_region(unaligned).or_else(|| self.next())
    }
}

pub struct Memory {
    vmo: zx::Vmo,
    mmap: Option<GuestMemoryMmap>,
    next_dynamic_device_address: GuestAddress,
    ram_regions: Vec<MemoryRegion>,
    device_ranges: Vec<MemoryRegion>,
}

impl Memory {
    pub fn new_from_config<H: Hypervisor>(
        guest_config: &GuestConfig,
        hypervisor: &H,
    ) -> Result<Self, MemoryError> {
        Self::new(guest_config.guest_memory.ok_or(MemoryError::NoMemoryRequested)?, hypervisor)
    }

    fn new<H: Hypervisor>(
        guest_memory_bytes: GuestUsize,
        hypervisor: &H,
    ) -> Result<Self, MemoryError> {
        let guest_memory_bytes =
            Memory::page_align_memory(guest_memory_bytes, "total_guest_memory");

        let vmo = hypervisor
            .allocate_memory(guest_memory_bytes)
            .map_err(|status| MemoryError::FailedToAllocateMemory { status })?;

        // TODO(fxbug.dev/102872): Support pluggable memory.
        let ram_regions =
            Memory::generate_guest_ram_regions(guest_memory_bytes, &RESTRICTED_RANGES)?;

        Ok(Memory {
            vmo,
            mmap: None,
            next_dynamic_device_address: FIRST_DYNAMIC_DEVICE_ADDR,
            ram_regions,
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
        let size = Memory::page_align_memory(size, tag.as_str());
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
        assert!(self.mmap.is_none());

        // TODO(fxbug.dev/102872): Create a vm-memory::GuestMemoryMmap.
        unimplemented!();
    }

    // Map the generated memory regions into the guest address space.
    pub fn map_into_guest(&self, guest_vmar: &zx::Vmar) -> Result<(), MemoryError> {
        // TODO(fxbug.dev/102872): Map into the guest vmar.
        unimplemented!();
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
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hypervisor::testing::{MockBehavior, MockHypervisor},
    };

    #[fuchsia::test]
    async fn failed_to_allocate_guest_memory_vmo() {
        let hypervisor = MockHypervisor::new();
        hypervisor.on_allocate_memory(MockBehavior::ReturnError(zx::Status::NO_RESOURCES));

        let mut config = GuestConfig::EMPTY;
        config.guest_memory = Some(*PAGE_SIZE);

        let memory = Memory::new_from_config(&config, &hypervisor);
        assert_eq!(
            MemoryError::FailedToAllocateMemory { status: zx::Status::NO_RESOURCES },
            memory.err().unwrap()
        );
    }

    #[fuchsia::test]
    async fn page_align_total_guest_memory() {
        // Round up to the nearest page.
        assert_eq!(Memory::page_align_memory(1, "test"), *PAGE_SIZE);
        assert_eq!(Memory::page_align_memory(*PAGE_SIZE - 1, "test"), *PAGE_SIZE);
        assert_eq!(Memory::page_align_memory(*PAGE_SIZE, "test"), *PAGE_SIZE);
        assert_eq!(Memory::page_align_memory(*PAGE_SIZE + 1, "test"), *PAGE_SIZE * 2);
    }

    #[fuchsia::test]
    async fn page_align_guest_memory_region() {
        // Page aligned.
        let actual =
            Memory::page_align_memory_region(MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE)).unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE);
        assert_eq!(actual, expected);

        // End is not page aligned, so round it down.
        let actual = Memory::page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE,
            *PAGE_SIZE * 3 + *PAGE_SIZE / 2,
        ))
        .unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE * 3);
        assert_eq!(actual, expected);

        // Start is not page aligned, so round it up (note that the second field is size, not the
        // ending address which is why it will also change).
        let actual = Memory::page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE / 2,
            *PAGE_SIZE * 3 + *PAGE_SIZE / 2,
        ))
        .unwrap();
        let expected = MemoryRegion::new(*PAGE_SIZE, *PAGE_SIZE * 3);
        assert_eq!(actual, expected);

        // After page aligning this is a zero length region, and is thus dropped.
        assert!(Memory::page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE / 2,
            *PAGE_SIZE / 2
        ))
        .is_none());

        // After page aligning this would be a negative length region, and is thus dropped.
        assert!(Memory::page_align_memory_region(MemoryRegion::new(
            *PAGE_SIZE / 2,
            *PAGE_SIZE / 4
        ))
        .is_none());
    }

    #[fuchsia::test]
    async fn page_aligning_guest_memory_regions_gives_correct_total_memory() {
        // Restrict memory between page 2 1/2 and page 4 1/2. This should result in guest memory
        // placed in pages [0, 1], and pages [5, 7] (giving 5 pages in total, the target).
        let target_memory = *PAGE_SIZE * 5;
        let restrictions = [MemoryRegion::new(*PAGE_SIZE * 2 + *PAGE_SIZE / 2, *PAGE_SIZE * 2)];

        let actual = Memory::generate_guest_ram_regions(target_memory, &restrictions).unwrap();
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

        let actual = Memory::generate_guest_ram_regions(target_memory, &restrictions).unwrap();
        let expected = [MemoryRegion::new(*PAGE_SIZE * 2, *PAGE_SIZE * 5)];

        assert!(actual
            .iter()
            .zip(expected.iter())
            .all(|(a, b)| a.base == b.base && a.size == b.size));
    }

    #[fuchsia::test]
    async fn get_guest_memory_regions() {
        // Four GiB of guest memory will extend beyond the PCI device region for x86, but not for arm64.
        let target_memory = Memory::page_align_memory(1 << 32, "test");
        let actual = Memory::generate_guest_ram_regions(target_memory, &RESTRICTED_RANGES).unwrap();

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
        let target_memory = Memory::page_align_memory(FIRST_DYNAMIC_DEVICE_ADDR + 0x1000, "test");
        let err =
            Memory::generate_guest_ram_regions(target_memory, &RESTRICTED_RANGES).unwrap_err();

        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&MemoryError::FailedToPlaceGuestMemory { actual: 0, target: 0 })
        );
    }

    #[fuchsia::test]
    async fn page_align_allocated_device_memory_range() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, &hypervisor).unwrap();

        let device1 =
            memory.allocate_dynamic_device_address(*PAGE_SIZE / 2, "a".to_string()).unwrap();
        let device2 = memory.allocate_dynamic_device_address(*PAGE_SIZE, "b".to_string()).unwrap();

        // Device 1 didn't request a full page, but device 2 will still be paged aligned.
        assert_eq!(device1 + *PAGE_SIZE, device2);
        assert_eq!(device1, Memory::page_align_memory(device1, "test"));
        assert_eq!(device2, Memory::page_align_memory(device2, "test"));
    }

    #[fuchsia::test]
    async fn device_memory_not_overlapping() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, &hypervisor).unwrap();

        let devices = 0xc000000;
        memory.add_device_range(devices, 0x1000, "a".to_string()).unwrap();
        memory.add_device_range(devices - 0x1000, 0x500, "b".to_string()).unwrap();
        memory.add_device_range(devices - 0x500, 0x500, "c".to_string()).unwrap();
        memory.add_device_range(devices + 0x1000, 0x1000, "d".to_string()).unwrap();
    }

    #[fuchsia::test]
    async fn device_memory_overlap() {
        let hypervisor = MockHypervisor::new();
        let mut memory = Memory::new(0x1000, &hypervisor).unwrap();

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
        let mut memory = Memory::new(0x1000, &hypervisor).unwrap();

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
        let mut memory = Memory::new(1 << 30, &hypervisor).unwrap();

        assert_eq!(
            memory
                .add_device_range(memory.ram_regions[0].base, 0x1000, "a".to_string())
                .err()
                .unwrap(),
            MemoryError::AddressConflict { tag1: "RAM".to_string(), tag2: "a".to_string() }
        );
    }
}
