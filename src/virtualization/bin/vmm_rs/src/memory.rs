// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::hypervisor::Hypervisor,
    fidl_fuchsia_virtualization::{GuestConfig, GuestError},
    fuchsia_zircon::{self as zx},
};

fn page_align_memory(guest_memory: u64) -> u64 {
    let page_size = zx::system_get_page_size() as u64;
    let page_alignment = guest_memory % page_size;
    if page_alignment != 0 {
        let padding = page_size - page_alignment;
        tracing::info!(
        "The requested guest memory ({} bytes) is not a multiple of system page size ({} bytes), so increasing guest memory by {} bytes.",
        guest_memory, page_size, padding);
        guest_memory + padding
    } else {
        guest_memory
    }
}

#[allow(dead_code)]
pub struct GuestMemory {
    size: u64,
    vmo: zx::Vmo,
}

impl GuestMemory {
    pub fn allocate_from_config<H: Hypervisor>(
        guest_config: &GuestConfig,
        hypervisor: &H,
    ) -> Result<Self, GuestError> {
        Self::allocate(guest_config.guest_memory.ok_or(GuestError::BadConfig)?, hypervisor)
    }

    pub fn allocate<H: Hypervisor>(bytes: u64, hypervisor: &H) -> Result<Self, GuestError> {
        let bytes = page_align_memory(bytes);
        let vmo = hypervisor.allocate_memory(bytes).map_err(|e| {
            tracing::error!("Failed to allocate guest memory: {}", e);
            GuestError::InternalError
        })?;
        Ok(GuestMemory { size: bytes, vmo })
    }

    #[cfg(test)]
    pub fn size(&self) -> u64 {
        self.size
    }

    #[cfg(test)]
    pub fn vmo(&self) -> &zx::Vmo {
        &self.vmo
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::hypervisor::testing::MockHypervisor};

    #[fuchsia::test]
    fn test_allocation_size_matches_request() {
        let hypervisor = MockHypervisor::new();

        const ALLOCATION_SIZE: u64 = 4 * 1024 * 1024 * 1024;
        let memory = GuestMemory::allocate(ALLOCATION_SIZE, &hypervisor).unwrap();
        assert_eq!(ALLOCATION_SIZE, memory.size());
        assert_eq!(ALLOCATION_SIZE, memory.vmo().get_size().unwrap());
    }

    #[fuchsia::test]
    fn test_page_align_memory() {
        let hypervisor = MockHypervisor::new();

        let memory = GuestMemory::allocate(1, &hypervisor).unwrap();
        assert_eq!(4096, memory.size());

        let memory = GuestMemory::allocate(4095, &hypervisor).unwrap();
        assert_eq!(4096, memory.size());

        let memory = GuestMemory::allocate(4096, &hypervisor).unwrap();
        assert_eq!(4096, memory.size());

        let memory = GuestMemory::allocate(4097, &hypervisor).unwrap();
        assert_eq!(8192, memory.size());
    }

    #[fuchsia::test]
    fn test_allocation_failure() {
        let hypervisor = MockHypervisor::new();

        hypervisor.on_allocate_memory(|_| Err(zx::Status::NO_RESOURCES));

        let memory = GuestMemory::allocate(1, &hypervisor);
        assert!(memory.is_err());
        assert_eq!(GuestError::InternalError, memory.err().unwrap());
    }
}
