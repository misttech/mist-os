// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::hypervisor::Hypervisor,
    crate::memory::GuestMemory,
    fidl_fuchsia_virtualization::{GuestConfig, GuestError},
};

#[allow(dead_code)]
pub struct VirtualMachine<H: Hypervisor> {
    hypervisor: H,
    guest: H::GuestHandle,
    guest_physical_address_space: H::AddressSpaceHandle,
    memory: GuestMemory,
}

impl<H: Hypervisor> VirtualMachine<H> {
    pub fn new(hypervisor: H, config: GuestConfig) -> Result<Self, GuestError> {
        let (guest, guest_physical_address_space) = hypervisor.guest_create().map_err(|e| {
            tracing::error!("Failed to create zx::Guest: {}", e);
            GuestError::InternalError
        })?;
        let memory = GuestMemory::allocate_from_config(&config, &hypervisor)?;
        Ok(VirtualMachine { hypervisor, guest, guest_physical_address_space, memory })
    }

    pub fn start_primary_vcpu(&self) -> Result<(), GuestError> {
        // TODO: implement
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::hypervisor::testing::MockHypervisor, fuchsia_zircon as zx};

    #[fuchsia::test]
    fn test_create_virtual_machine_hypervisor_failure() {
        let hypervisor = MockHypervisor::new();
        hypervisor.on_guest_create(|| Err(zx::Status::NO_RESOURCES));
        let config = GuestConfig::EMPTY;

        let vm = VirtualMachine::new(hypervisor.clone(), config);

        assert!(vm.is_err());
        assert_eq!(vm.err().unwrap(), GuestError::InternalError);
    }

    #[fuchsia::test]
    fn test_create_virtual_machine_no_memory_in_config() {
        let hypervisor = MockHypervisor::new();
        let config = GuestConfig::EMPTY;

        let vm = VirtualMachine::new(hypervisor.clone(), config);

        assert!(vm.is_err());
        assert_eq!(vm.err().unwrap(), GuestError::BadConfig);
    }

    #[fuchsia::test]
    fn test_create_virtual_machine() {
        let hypervisor = MockHypervisor::new();
        let mut config = GuestConfig::EMPTY;
        config.guest_memory = Some(4 * 1024 * 1024 * 1024);

        let vm = VirtualMachine::new(hypervisor.clone(), config);

        assert!(vm.is_ok());
    }
}
