// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl_fuchsia_kernel::{HypervisorResourceMarker, VmexResourceMarker},
    fuchsia_component::client::connect_to_protocol,
    fuchsia_zircon as zx,
    futures::join,
    std::fmt::Debug,
    std::marker::PhantomData,
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
pub trait Hypervisor: Send + Sync + Clone + 'static {
    /// A hypervisor-specific type to represent a hypervisor guest.
    ///
    /// We require [Send] + [Sync] on the [GuestHandle] so that we can pass this to the vcpu threads.
    type GuestHandle: Debug + Send + Sync;

    /// A hypervisor-specific type to represent an address space. For example, the guest-physical
    /// address space.
    type AddressSpaceHandle: Debug;

    /// A hypervisor-specific type to represent a vcpu.
    ///
    /// Most vcpu operations must occur on the same thread that created the vcpu. As a result we
    /// _do_not_ add [Send] + [Sync] here; a [VcpuHandle] should only be used on the thread that
    /// created that vcpu.
    type VcpuHandle: Debug;

    /// A hypervisor-specific type to represent a vcpu controller.
    ///
    /// This is a distinct type from [VcpuHandle] to model the set of vcpu operations that may
    /// occur on a thread other than the one that owns the vcpu. We require [Send] here because
    /// we need to be able to return this handle from the vcpu thread to the monitor thread.
    type VcpuControlHandle: Debug + Send;

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
    ) -> Result<(Self::VcpuHandle, Self::VcpuControlHandle), zx::Status>;

    /// Reads the architectural registers for a vcpu.
    ///
    /// # Arguments:
    ///
    /// * `vcpu` - The [VcpuHandle] of the vcpu to read state.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_read_state](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_read_state)
    /// syscall.
    fn vcpu_read_state(
        &self,
        vcpu: &Self::VcpuHandle,
    ) -> Result<zx::sys::zx_vcpu_state_t, zx::Status>;

    /// Writes the architectural registers for a vcpu.
    ///
    /// # Arguments:
    ///
    /// * `vcpu` - The [VcpuHandle] of the vcpu to write state.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_write_state](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_write_state)
    /// syscall.
    fn vcpu_write_state(
        &self,
        vcpu: &Self::VcpuHandle,
        state: &zx::sys::zx_vcpu_state_t,
    ) -> Result<(), zx::Status>;

    /// Resumes execution of a vcpu within the guest.
    ///
    /// This is a blocking operation; the vcpu will execute within the guest context until a vm-
    /// exit is triggered. The returned [zx::Packet] will contain details about what caused the
    /// vmexit.
    ///
    /// # Arguments:
    ///
    /// * `vcpu` - The [VcpuHandle] of the vcpu to execute.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_enter](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_enter)
    /// syscall.
    fn vcpu_enter(&self, vcpu: &Self::VcpuHandle) -> Result<zx::Packet, zx::Status>;

    /// Kick a vcpu, causing it to return from zx_vcpu_enter.
    ///
    /// Allows a thread blocked on zx::Vcpu::enter to return with zx::Status::CANCELLED.
    ///
    /// # Arguments:
    ///
    /// * `vcpu` - The [VcpuControlHandle] of the vcpu to execute.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_kick](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_kick)
    /// syscall.
    fn vcpu_kick(&self, vcpu: &Self::VcpuControlHandle) -> Result<(), zx::Status>;
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

// We add the `PhantomData<*const ()>` here to remove the default [Send] impl for this handle
// because we only want to use this handle from the vcpu thread.
//
// Once stable, this can be replaced with:
//
//   `impl !Send for ZirconVcpuHandle {}`
#[derive(Debug)]
pub struct ZirconVcpuHandle(Arc<zx::Vcpu>, PhantomData<*const ()>);

#[derive(Debug)]
pub struct ZirconVcpuControlHandle(Arc<zx::Vcpu>);

impl Hypervisor for FuchsiaHypervisor {
    type GuestHandle = zx::Guest;
    type AddressSpaceHandle = zx::Vmar;
    type VcpuHandle = ZirconVcpuHandle;
    type VcpuControlHandle = ZirconVcpuControlHandle;

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
    ) -> Result<(Self::VcpuHandle, Self::VcpuControlHandle), zx::Status> {
        let vcpu = Arc::new(zx::Vcpu::create(guest, entry)?);
        Ok((ZirconVcpuHandle(vcpu.clone(), PhantomData), ZirconVcpuControlHandle(vcpu)))
    }
    fn vcpu_read_state(
        &self,
        vcpu: &Self::VcpuHandle,
    ) -> Result<zx::sys::zx_vcpu_state_t, zx::Status> {
        vcpu.0.read_state()
    }
    fn vcpu_write_state(
        &self,
        vcpu: &Self::VcpuHandle,
        state: &zx::sys::zx_vcpu_state_t,
    ) -> Result<(), zx::Status> {
        vcpu.0.write_state(state)
    }
    fn vcpu_enter(&self, vcpu: &Self::VcpuHandle) -> Result<zx::Packet, zx::Status> {
        vcpu.0.enter()
    }
    fn vcpu_kick(&self, vcpu: &Self::VcpuControlHandle) -> Result<(), zx::Status> {
        vcpu.0.kick()
    }
}

#[cfg(test)]
pub mod testing {
    use {
        super::*,
        parking_lot::Mutex,
        std::cell::RefCell,
        std::sync::{mpsc, Arc},
    };

    #[derive(Clone, Debug)]
    pub struct MockAddressSpace {}

    impl MockAddressSpace {
        pub fn new() -> Self {
            Self {}
        }
    }

    #[derive(Debug)]
    pub struct MockGuest {
        address_space: MockAddressSpace,
        vcpus: Arc<Mutex<Vec<MockVcpuController>>>,
    }

    impl MockGuest {
        pub fn new() -> Self {
            Self { address_space: MockAddressSpace::new(), vcpus: Arc::new(Mutex::new(Vec::new())) }
        }

        pub fn address_space(&self) -> MockAddressSpace {
            self.address_space.clone()
        }

        pub fn vcpus(&self) -> Vec<MockVcpuController> {
            self.vcpus.lock().clone()
        }
    }

    /// The [MockVcpuController] allows tests to control the behavior of a [MockVcpu] while it is
    /// blocked on [Hypervisor::vcpu_enter].
    ///
    /// Within a test, [MockHypervisor::vcpu_enter] is implemented using a [std::sync::mpsc::channel].
    /// The [MockVcpuController] allows tests to send messages to the [MockVcpu] control when and how
    /// it returns from [MockHypervisor::vcpu_enter].
    #[derive(Clone, Debug)]
    pub struct MockVcpuController {
        // Sending endpoint of the mpsc channel. The other endpoint will be read as part of the implementation of
        // [MockHypervisor::vcpu_enter].
        packet_sender: mpsc::Sender<Result<zx::Packet, zx::Status>>,
        // The initial instruction pointer that the [MockVcpu] was created with.
        initial_ip: usize,
    }

    impl MockVcpuController {
        /// Sends a status, causing [MockHypervisor::vcpu_enter] to return an [Err] with the provided
        /// [zx::Status] value.
        pub fn send_status(
            &self,
            status: zx::Status,
        ) -> Result<(), mpsc::SendError<Result<zx::Packet, zx::Status>>> {
            assert!(status != zx::Status::OK);
            self.packet_sender.send(Err(status))
        }

        pub fn initial_ip(&self) -> usize {
            self.initial_ip
        }
    }

    #[derive(Debug)]
    pub struct MockVcpu {
        state: RefCell<zx::sys::zx_vcpu_state_t>,
        packet_receiver: mpsc::Receiver<Result<zx::Packet, zx::Status>>,
    }

    impl MockVcpu {
        pub fn new(initial_ip: usize) -> (MockVcpu, MockVcpuController) {
            let (packet_sender, packet_receiver) = mpsc::channel();
            (
                MockVcpu { state: RefCell::new(Default::default()), packet_receiver },
                MockVcpuController { packet_sender, initial_ip },
            )
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
        on_vcpu_create: MockBehavior,
        on_vcpu_read_state: MockBehavior,
        on_vcpu_write_state: MockBehavior,
        on_vcpu_enter: MockBehavior,
        on_vcpu_kick: MockBehavior,
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

    /// The default callable used to implement [Hypervisor::vcpu_create] for [MockHypervisor].
    pub fn mock_vcpu_create(
        guest: &MockGuest,
        entry: usize,
    ) -> Result<(MockVcpu, MockVcpuController), zx::Status> {
        let (vcpu, controller) = MockVcpu::new(entry);
        guest.vcpus.lock().push(controller.clone());
        Ok((vcpu, controller))
    }

    /// The default callable used to implement [Hypervisor::vcpu_read_state] for [MockHypervisor].
    pub fn mock_vcpu_read_state(vcpu: &MockVcpu) -> Result<zx::sys::zx_vcpu_state_t, zx::Status> {
        Ok(*vcpu.state.borrow())
    }

    /// The default callable used to implement [Hypervisor::vcpu_write_state] for [MockHypervisor].
    pub fn mock_vcpu_write_state(
        vcpu: &MockVcpu,
        state: &zx::sys::zx_vcpu_state_t,
    ) -> Result<(), zx::Status> {
        *vcpu.state.borrow_mut() = *state;
        Ok(())
    }

    /// The default callable used to implement [Hypervisor::vcpu_enter] for [MockHypervisor].
    pub fn mock_vcpu_enter(vcpu: &MockVcpu) -> Result<zx::Packet, zx::Status> {
        // Block-on the packet receiver until the test sends a packet or status return. This allows
        // the test to simulate VM exits using an mpsc channel.
        vcpu.packet_receiver.recv().unwrap()
    }

    /// The default callable used to implement [Hypervisor::vcpu_kick] for [MockHypervisor].
    pub fn mock_vcpu_kick(vcpu: &MockVcpuController) -> Result<(), zx::Status> {
        // zx_vcpu_kick is defined to cause the current or next invocation of zx_vcpu_enter to return
        // with ZX_ERR_CANCELLED.
        //
        // This implementation is imperfect since it'll only be visible in the order that is written
        // to the mpsc channel. The assumption is this will be OK for tests.
        vcpu.packet_sender.send(Err(zx::Status::CANCELED)).map_err(|_e| {
            tracing::error!("Unable to kick vcpu, the receiver has already shutdown.");
            zx::Status::CONNECTION_RESET
        })
    }

    #[allow(dead_code)]
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

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_create] is called.
        pub fn on_vcpu_create(&self, behavior: MockBehavior) {
            self.inner.lock().on_vcpu_create = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_read_state] is called.
        pub fn on_vcpu_read_state(&self, behavior: MockBehavior) {
            self.inner.lock().on_vcpu_read_state = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_write_state] is called.
        pub fn on_vcpu_write_state(&self, behavior: MockBehavior) {
            self.inner.lock().on_vcpu_write_state = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_enter] is called.
        pub fn on_vcpu_enter(&self, behavior: MockBehavior) {
            self.inner.lock().on_vcpu_enter = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_kick] is called.
        pub fn on_vcpu_kick(&self, behavior: MockBehavior) {
            self.inner.lock().on_vcpu_kick = behavior;
        }
    }

    impl Hypervisor for MockHypervisor {
        type GuestHandle = MockGuest;
        type AddressSpaceHandle = MockAddressSpace;
        type VcpuHandle = MockVcpu;
        type VcpuControlHandle = MockVcpuController;

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
            guest: &Self::GuestHandle,
            entry: usize,
        ) -> Result<(Self::VcpuHandle, Self::VcpuControlHandle), zx::Status> {
            if let Some(status) = self.inner.lock().on_vcpu_create.return_error() {
                return Err(status);
            }
            mock_vcpu_create(guest, entry)
        }
        fn vcpu_read_state(
            &self,
            vcpu: &Self::VcpuHandle,
        ) -> Result<zx::sys::zx_vcpu_state_t, zx::Status> {
            if let Some(status) = self.inner.lock().on_vcpu_read_state.return_error() {
                return Err(status);
            }
            mock_vcpu_read_state(vcpu)
        }
        fn vcpu_write_state(
            &self,
            vcpu: &Self::VcpuHandle,
            state: &zx::sys::zx_vcpu_state_t,
        ) -> Result<(), zx::Status> {
            if let Some(status) = self.inner.lock().on_vcpu_write_state.return_error() {
                return Err(status);
            }
            mock_vcpu_write_state(vcpu, state)
        }
        fn vcpu_enter(&self, vcpu: &Self::VcpuHandle) -> Result<zx::Packet, zx::Status> {
            if let Some(status) = self.inner.lock().on_vcpu_enter.return_error() {
                return Err(status);
            }
            mock_vcpu_enter(vcpu)
        }
        fn vcpu_kick(&self, vcpu: &Self::VcpuControlHandle) -> Result<(), zx::Status> {
            if let Some(status) = self.inner.lock().on_vcpu_kick.return_error() {
                return Err(status);
            }
            mock_vcpu_kick(vcpu)
        }
    }
}
