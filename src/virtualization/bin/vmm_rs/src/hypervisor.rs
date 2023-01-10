// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl_fuchsia_kernel::{HypervisorResourceMarker, VmexResourceMarker},
    fuchsia_component::client::connect_to_protocol,
    fuchsia_zircon as zx,
    futures::join,
    std::sync::Arc,
};

/// The [Hypervisor] trait exists to abstract the low-level touch points with the hypervisor.
///
/// This trait is primarily designed to facilitate testing of code that interfaces with the
/// hypervisor and is not currently designed to be portable to non-Fuchsia hypervisors, so there
/// are some Fuchsia-specific types in the hypervisor interface.
///
/// We require [Send] + [Sync] + [Clone] because we need to share the [Hypervisor] with Vcpu
/// threads.
pub trait Hypervisor: Send + Sync + Clone {
    /// A hypervisor-specific type to represent a hypervisor guest.
    type GuestHandle;
    /// A hypervisor-specific type to represent an address space. For example, the guest-physical
    /// address space.
    type AddressSpaceHandle;
    /// A hypervisor-specific type to represent a hypervisor guest.
    type VcpuHandle;

    /// Creates a new guest and returns a handle to that guest and a handle to the address space
    /// for guest-physical addresses.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_guest_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/guest_create) syscall.
    fn guest_create(&self) -> Result<(Self::GuestHandle, Self::AddressSpaceHandle), zx::Status>;

    /// Allocates a [zx::Vmo] that can be mapped into the address space for a guest.
    fn allocate_memory(&self, bytes: u64) -> Result<zx::Vmo, zx::Status>;

    /// Creates a new vcpu for a guest.
    ///
    /// # Arguments:
    ///
    /// * `guest` - A guest that was created by a previous call to [Hypervisor::guest_create].
    /// * `entry` - The guest-physical address that this vcpu should begin execution at.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_create) syscall.
    fn vcpu_create(
        &self,
        guest: &Self::GuestHandle,
        entry: usize,
    ) -> Result<Self::VcpuHandle, zx::Status>;
}

struct FuchsiaHypervisorResources {
    hypervisor_resource: zx::Resource,
    vmex_resource: zx::Resource,
}

#[derive(Clone)]
pub struct FuchsiaHypervisor {
    inner: Arc<FuchsiaHypervisorResources>,
}

impl FuchsiaHypervisor {
    pub async fn new() -> Result<Self, Error> {
        // These system resources are needed to access the hypervisor.
        let (hypervisor, vmex) = join!(
            connect_to_protocol::<HypervisorResourceMarker>()?.get(),
            connect_to_protocol::<VmexResourceMarker>()?.get()
        );
        Ok(Self {
            inner: Arc::new(FuchsiaHypervisorResources {
                hypervisor_resource: hypervisor?,
                vmex_resource: vmex?,
            }),
        })
    }
}

impl Hypervisor for FuchsiaHypervisor {
    type GuestHandle = zx::Guest;
    type AddressSpaceHandle = zx::Vmar;
    type VcpuHandle = zx::Vcpu;

    fn guest_create(&self) -> Result<(Self::GuestHandle, Self::AddressSpaceHandle), zx::Status> {
        Ok(zx::Guest::normal(&self.inner.hypervisor_resource)?)
    }

    fn allocate_memory(&self, bytes: u64) -> Result<zx::Vmo, zx::Status> {
        let vmo = zx::Vmo::create(bytes)?;
        Ok(vmo.replace_as_executable(&self.inner.vmex_resource)?)
    }

    fn vcpu_create(
        &self,
        guest: &Self::GuestHandle,
        entry: usize,
    ) -> Result<Self::VcpuHandle, zx::Status> {
        Ok(zx::Vcpu::create(guest, entry)?)
    }
}

#[cfg(test)]
pub mod testing {
    use {super::*, parking_lot::Mutex, std::sync::Arc};

    #[derive(Clone)]
    pub struct MockAddressSpace {}

    impl MockAddressSpace {
        pub fn new() -> Self {
            Self {}
        }
    }

    pub struct MockGuest {
        address_space: MockAddressSpace,
    }

    impl MockGuest {
        pub fn new() -> Self {
            Self { address_space: MockAddressSpace::new() }
        }

        pub fn address_space(&self) -> MockAddressSpace {
            self.address_space.clone()
        }
    }

    /// Allow tests to control the behavior of [MockHypervisor] methods.
    #[derive(Clone, Debug, Default)]
    pub enum MockBehavior {
        /// Call the mock method, which is the default behavior.
        #[default]
        CallMock,

        /// Return an error from the operation.
        ReturnError(zx::Status),
    }

    impl MockBehavior {
        pub fn return_error(&self) -> Option<zx::Status> {
            if let MockBehavior::ReturnError(status) = self {
                Some(*status)
            } else {
                None
            }
        }
    }

    #[derive(Default)]
    struct MockHypervisorState {
        on_guest_create: MockBehavior,
        on_allocate_memory: MockBehavior,
    }

    #[derive(Clone)]
    pub struct MockHypervisor {
        inner: Arc<Mutex<MockHypervisorState>>,
    }

    /// The default callable used to implement [Hypervisor::guest_create] for [MockHypervisor].
    ///
    /// This simply returns Ok with a [MockGuest] and [MockAddressSpace].
    pub fn mock_guest_create() -> Result<(MockGuest, MockAddressSpace), zx::Status> {
        let guest = MockGuest::new();
        let address_space = guest.address_space();
        Ok((guest, address_space))
    }

    /// The default callable used to implement [Hypervisor::allocate_memory] for [MockHypervisor].
    ///
    /// This wraps [zx::Vmo::create].
    pub fn mock_allocate_memory(bytes: u64) -> Result<zx::Vmo, zx::Status> {
        zx::Vmo::create(bytes)
    }

    impl MockHypervisor {
        pub fn new() -> Self {
            Self { inner: Arc::new(Mutex::new(Default::default())) }
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::allocate_memory] is called.
        pub fn on_allocate_memory(&self, behavior: MockBehavior) {
            self.inner.lock().on_allocate_memory = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::guest_create] is called.
        pub fn on_guest_create(&self, behavior: MockBehavior) {
            self.inner.lock().on_guest_create = behavior;
        }
    }

    impl Hypervisor for MockHypervisor {
        type GuestHandle = MockGuest;
        type AddressSpaceHandle = MockAddressSpace;
        // TODO(https://fxbug.dev/102872): Create a vcpu mock.
        type VcpuHandle = ();

        fn guest_create(
            &self,
        ) -> Result<(Self::GuestHandle, Self::AddressSpaceHandle), zx::Status> {
            if let Some(status) = self.inner.lock().on_guest_create.return_error() {
                return Err(status);
            }
            mock_guest_create()
        }

        fn allocate_memory(&self, bytes: u64) -> Result<zx::Vmo, zx::Status> {
            if let Some(status) = self.inner.lock().on_allocate_memory.return_error() {
                return Err(status);
            }
            mock_allocate_memory(bytes)
        }

        fn vcpu_create(
            &self,
            _guest: &Self::GuestHandle,
            _entry: usize,
        ) -> Result<Self::VcpuHandle, zx::Status> {
            Ok(())
        }
    }
}
