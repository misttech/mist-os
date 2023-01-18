// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::address::GuestPhysicalAddress,
    crate::hypervisor::Hypervisor,
    crate::memory::Memory,
    crate::vcpu::Vcpu,
    fidl_fuchsia_virtualization::{GuestConfig, GuestError},
    fuchsia_zircon::{self as zx},
    std::sync::Arc,
};

#[cfg(target_arch = "aarch64")]
const MAX_SUPPORTED_VCPUS: u32 = 8;
#[cfg(target_arch = "x86_64")]
const MAX_SUPPORTED_VCPUS: u32 = 64;

#[derive(Copy, Clone, Debug, Default)]
pub struct BootParams {
    /// The physical address of the first instruction to execute when the boot vcpu starts.
    entry: GuestPhysicalAddress,

    /// A data argument to provide to the vcpu when it starts. The specific register depends on the
    /// target architecture:
    ///
    ///     x64: `rsi`
    ///   arm64: `x0`
    boot_ptr: u64,
}

#[allow(dead_code)]
pub struct VirtualMachine<H: Hypervisor> {
    hypervisor: H,
    guest: Arc<H::GuestHandle>,
    guest_physical_address_space: H::AddressSpaceHandle,
    memory: Memory<H>,
    vcpus: [Option<Vcpu<H>>; MAX_SUPPORTED_VCPUS as usize],
    boot_params: BootParams,
}

impl<H: Hypervisor> VirtualMachine<H> {
    pub fn new(hypervisor: H, config: GuestConfig) -> Result<VirtualMachine<H>, GuestError> {
        let (guest, guest_physical_address_space) = hypervisor.guest_create().map_err(|e| {
            tracing::error!("Failed to create zx::Guest: {}", e);
            GuestError::InternalError
        })?;

        let memory = Memory::new_from_config(&config, hypervisor.clone()).map_err(|e| {
            tracing::error!("Failed to create guest memory: {}", e);
            e.into()
        })?;
        memory.map_into_host().map_err(|e| e.into())?;
        memory.map_into_guest(&guest_physical_address_space).map_err(|e| e.into())?;

        Ok(VirtualMachine {
            hypervisor,
            guest: Arc::new(guest),
            guest_physical_address_space,
            memory,
            // Default::default() is only derived for arrays up to 32 elements. This is just a
            // work-around to generate an empty (all None) array of Option without using any unsafe
            // code.
            vcpus: [(); MAX_SUPPORTED_VCPUS as usize].map(|_| Option::<Vcpu<H>>::default()),
            boot_params: Default::default(),
        })
    }

    #[cfg(test)]
    pub fn guest(&self) -> Arc<H::GuestHandle> {
        self.guest.clone()
    }

    async fn start_vcpu(
        &mut self,
        id: u32,
        entry: GuestPhysicalAddress,
        boot_ptr: u64,
    ) -> zx::Status {
        let id = id as usize;
        if id >= self.vcpus.len() {
            tracing::error!(
                "Failed to start vcpu-{} up to {} vcpus are supported",
                id,
                self.vcpus.len()
            );
            return zx::Status::OUT_OF_RANGE;
        }

        if !self.vcpus[0].is_some() && id != 0 {
            tracing::error!("vcpu-0 must be started before other vcpus");
            return zx::Status::BAD_STATE;
        }

        if self.vcpus[id].is_some() {
            // The guest might make multiple requests to start a particular VCPU. On
            // x86, the guest should send two START_UP IPIs but we initialize the VCPU
            // on the first. So, we ignore subsequent requests.
            return zx::Status::OK;
        }
        self.vcpus[id] = Some(Vcpu::new(
            self.hypervisor.clone(),
            self.guest.clone(),
            id as u32,
            entry,
            boot_ptr,
        ));
        self.vcpus[id].as_mut().unwrap().start().await
    }

    /// Set the [BootParams] for the vm.
    ///
    /// The boot params will control the entry point of the initial/boot cpu based on the target OS that
    /// is being loaded.
    #[cfg(test)]
    pub fn set_boot_params(&mut self, params: BootParams) {
        self.boot_params = params;
    }

    pub async fn start_primary_vcpu(&mut self) -> Result<(), GuestError> {
        let BootParams { entry, boot_ptr } = self.boot_params;
        match self.start_vcpu(0, entry, boot_ptr).await {
            zx::Status::OK => Ok(()),
            status => {
                tracing::error!("Unexpected error starting primary vcpu: {}", status);
                Err(GuestError::InternalError)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hypervisor::testing::{MockBehavior, MockHypervisor},
        fuchsia_zircon as zx,
    };

    #[fuchsia::test]
    fn test_create_virtual_machine_hypervisor_failure() {
        let hypervisor = MockHypervisor::new();
        hypervisor.on_guest_create(MockBehavior::ReturnError(zx::Status::NO_RESOURCES));
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

    struct TestFixture {
        _hypervisor: MockHypervisor,
        vm: VirtualMachine<MockHypervisor>,
    }

    impl TestFixture {
        pub const DEFAULT_MEMORY: u64 = 4 * 1024 * 1024 * 1024;
        pub const DEFAULT_BOOT_PTR: u64 = 0xfade;
        pub const DEFAULT_ENTRY: GuestPhysicalAddress = 0xabcd;

        pub fn new() -> Self {
            Self::with_config(GuestConfig {
                guest_memory: Some(Self::DEFAULT_MEMORY),
                ..GuestConfig::EMPTY
            })
        }

        pub fn with_config(config: GuestConfig) -> Self {
            let hypervisor = MockHypervisor::new();
            let mut vm =
                VirtualMachine::new(hypervisor.clone(), config).expect("Failed to create VM");
            vm.set_boot_params(BootParams {
                entry: Self::DEFAULT_ENTRY,
                boot_ptr: Self::DEFAULT_BOOT_PTR,
            });
            Self { _hypervisor: hypervisor, vm }
        }

        pub fn vm(&self) -> &VirtualMachine<MockHypervisor> {
            &self.vm
        }

        pub fn vm_mut(&mut self) -> &mut VirtualMachine<MockHypervisor> {
            &mut self.vm
        }
    }

    #[fuchsia::test]
    fn test_create_virtual_machine() {
        let hypervisor = MockHypervisor::new();
        let mut config = GuestConfig::EMPTY;
        config.guest_memory = Some(4 * 1024 * 1024 * 1024);

        let vm = VirtualMachine::new(hypervisor.clone(), config);

        assert!(vm.is_ok());
    }

    #[fuchsia::test]
    async fn test_start_vcpu() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        let status = test.vm_mut().start_primary_vcpu().await;
        assert!(status.is_ok());

        assert_eq!(guest.vcpus().len(), 1);
        assert_eq!(guest.vcpus()[0].initial_ip(), TestFixture::DEFAULT_ENTRY);
    }

    #[fuchsia::test]
    async fn test_start_vcpu_duplicate_id() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        // First start_vcpu should succeed.
        let status = test
            .vm_mut()
            .start_vcpu(0, TestFixture::DEFAULT_ENTRY, TestFixture::DEFAULT_BOOT_PTR)
            .await;
        assert_eq!(status, zx::Status::OK);
        assert_eq!(guest.vcpus().len(), 1);

        // Second start_vcpu should still pass, but no more vcpus should have been created by the hypervisor.
        let status = test
            .vm_mut()
            .start_vcpu(0, TestFixture::DEFAULT_ENTRY, TestFixture::DEFAULT_BOOT_PTR)
            .await;
        assert_eq!(status, zx::Status::OK);
        assert_eq!(guest.vcpus().len(), 1);
    }

    #[fuchsia::test]
    async fn test_start_secondary_vcpu_first() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        // Start non-0 vcpu first. 0 is always the boot vcpu and we require this be the first vcpu that is started.
        let status = test
            .vm_mut()
            .start_vcpu(1, TestFixture::DEFAULT_ENTRY, TestFixture::DEFAULT_BOOT_PTR)
            .await;
        assert_eq!(status, zx::Status::BAD_STATE);
        assert_eq!(guest.vcpus().len(), 0);
    }

    #[fuchsia::test]
    async fn test_start_all_vcpus_in_order() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        let mut vcpu_count = 0;
        for vcpu_id in 0..MAX_SUPPORTED_VCPUS {
            let status = test
                .vm_mut()
                .start_vcpu(vcpu_id, TestFixture::DEFAULT_ENTRY, TestFixture::DEFAULT_BOOT_PTR)
                .await;
            assert_eq!(status, zx::Status::OK);
            vcpu_count += 1;
            assert_eq!(guest.vcpus().len(), vcpu_count);
        }
    }

    #[fuchsia::test]
    async fn test_start_all_vcpus_reverse_order() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        // Start the primary vcpu; this _must_ come first.
        let result = test.vm_mut().start_primary_vcpu().await;
        assert!(result.is_ok());
        assert_eq!(guest.vcpus().len(), 1);

        // Test that we have no dependency on the order that the secondary vcpus are started by
        // starting them with in reverse order.
        let mut vcpu_count = 1;
        for vcpu_id in (1..MAX_SUPPORTED_VCPUS).rev() {
            let status = test
                .vm_mut()
                .start_vcpu(vcpu_id, TestFixture::DEFAULT_ENTRY, TestFixture::DEFAULT_BOOT_PTR)
                .await;
            assert_eq!(status, zx::Status::OK);
            vcpu_count += 1;
            assert_eq!(guest.vcpus().len(), vcpu_count);
        }
    }

    #[fuchsia::test]
    async fn test_start_invalid_vcpu_id() {
        let mut test = TestFixture::new();
        let guest = test.vm().guest();
        assert_eq!(guest.vcpus().len(), 0);

        // Start the primary vcpu; this _must_ come first.
        let result = test.vm_mut().start_primary_vcpu().await;
        assert!(result.is_ok());
        assert_eq!(guest.vcpus().len(), 1);

        // Start a vcpu out of range of [MAX_SUPPORTED_VCPUS].
        let status = test
            .vm_mut()
            .start_vcpu(
                MAX_SUPPORTED_VCPUS,
                TestFixture::DEFAULT_ENTRY,
                TestFixture::DEFAULT_BOOT_PTR,
            )
            .await;
        assert_eq!(status, zx::Status::OUT_OF_RANGE);
        assert_eq!(guest.vcpus().len(), 1);
    }
}
