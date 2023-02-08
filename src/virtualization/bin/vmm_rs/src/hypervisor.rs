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
    /// The full set of errors can be found in the documentation for the
    /// [zx_guest_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/guest_create) syscall.
    fn guest_create(&self) -> Result<(Self::GuestHandle, Self::AddressSpaceHandle), zx::Status>;

    /// Sets a trap in the guest's port-io space.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found in the documentation for the
    /// [zx_guest_set_trap](https://fuchsia.dev/fuchsia-src/reference/syscalls/guest_set_trap)
    /// syscall.
    fn guest_set_io_trap(
        &self,
        guest: &Self::GuestHandle,
        addr: u16,
        size: u16,
        key: u64,
    ) -> Result<(), zx::Status>;

    /// Allocates a [zx::Vmo] that can be mapped into the address space for a guest.
    fn allocate_memory(&self, bytes: u64) -> Result<zx::Vmo, zx::Status>;

    /// Create a VMAR parented to the local process's root VMAR.
    ///
    /// # Arguments:
    ///
    /// * `size` - The size of the new VMAR in bytes.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found in the documentation for the
    /// [zx_vmar_allocate](https://fuchsia.dev/fuchsia-src/reference/syscalls/vmar_allocate)
    /// syscall.
    fn vmar_allocate(&self, size: u64) -> Result<(Self::AddressSpaceHandle, usize), zx::Status>;

    /// Map a region of a VMO into the given VMAR. The VMAR must have the CAN_MAP_SPECIFIC
    /// permission set so that the offset into the VMAR and the offset into the VMO is the same.
    ///
    /// # Arguments:
    ///
    /// * `vmar`        - A VMAR (either from vmar_allocate or guest_create)
    /// * `vmar_offset` - The offset into the given VMAR, in bytes.
    /// * `vmo`         - The VMO backing the guest physical memory.
    /// * `vmo_offset`  - The offset into the given VMO, in bytes.
    /// * `length`      - The length of this region, in bytes.
    /// * `flags`       - Permissions for this mapped region.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found in the documentation for the
    /// [zx_vmar_map](https://fuchsia.dev/fuchsia-src/reference/syscalls/vmar_map) syscall.
    fn vmar_map(
        &self,
        vmar: &Self::AddressSpaceHandle,
        vmar_offset: usize,
        vmo: &zx::Vmo,
        vmo_offset: u64,
        length: usize,
        flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status>;

    /// Destroy a VMAR. This should be called on child VMARs created via vmar_allocate. For
    /// information about safety, see the comments in /src/lib/zircon/rust/src/vmar.rs::Vmar::unmap.
    ///
    /// # Arguments:
    ///
    /// * `vmar` - An allocated child VMAR (not the root VMAR returned from guest_create).
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found in the documentation for the
    /// [zx_vmar_destroy](https://fuchsia.dev/fuchsia-src/reference/syscalls/vmar_destroy)
    /// syscall.
    unsafe fn vmar_destroy(&self, vmar: &Self::AddressSpaceHandle) -> Result<(), zx::Status>;

    /// Creates a new vcpu for a guest.
    ///
    /// # Arguments:
    ///
    /// * `guest` - A guest that was created by a previous call to [Hypervisor::guest_create].
    /// * `entry` - The guest-physical address that this vcpu should begin execution at.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found in the documentation for the
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

    /// Writes the result of a port-read instruction to the vcpu.
    ///
    /// # Arguments:
    ///
    /// * `vcpu` - The [VcpuHandle] of the vcpu to write state.
    /// * `value` - The value of the emulated port-io access. This _must_ be 1,
    ///   2, or 4 bytes in length.
    ///
    /// # Errors:
    ///
    /// The full set of errors can be found on the documentation for the
    /// [zx_vcpu_write_state](https://fuchsia.dev/fuchsia-src/reference/syscalls/vcpu_write_state)
    /// syscall.
    fn vcpu_write_io(&self, vcpu: &Self::VcpuHandle, value: &[u8]) -> Result<(), zx::Status>;

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

    fn vmar_allocate(size: u64) -> Result<(zx::Vmar, usize), zx::Status> {
        let vmar_flags = zx::VmarFlags::CAN_MAP_READ
            | zx::VmarFlags::CAN_MAP_WRITE
            | zx::VmarFlags::CAN_MAP_SPECIFIC;
        fuchsia_runtime::vmar_root_self().allocate(
            0,
            size.try_into().expect("u64 should fit into usize"),
            vmar_flags,
        )
    }

    fn vmar_map(
        vmar: &zx::Vmar,
        vmar_offset: usize,
        vmo: &zx::Vmo,
        vmo_offset: u64,
        length: usize,
        flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status> {
        vmar.map(vmar_offset, vmo, vmo_offset, length, flags)
    }

    unsafe fn vmar_destroy(vmar: &zx::Vmar) -> Result<(), zx::Status> {
        vmar.destroy()
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
    fn guest_set_io_trap(
        &self,
        guest: &Self::GuestHandle,
        addr: u16,
        size: u16,
        key: u64,
    ) -> Result<(), zx::Status> {
        guest.set_io_trap(addr, size, key)
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
    fn vcpu_write_io(&self, vcpu: &Self::VcpuHandle, value: &[u8]) -> Result<(), zx::Status> {
        // len should only be a supported IO access size. These are the only sizes that the
        // hypervisor will generate traps for so we don't expect any other values.
        assert!(value.len() == 1 || value.len() == 2 || value.len() == 4);
        let mut raw =
            zx::sys::zx_vcpu_io_t { access_size: value.len() as u8, ..Default::default() };
        raw.data.copy_from_slice(value);
        vcpu.0.write_io(&raw)
    }
    fn vcpu_enter(&self, vcpu: &Self::VcpuHandle) -> Result<zx::Packet, zx::Status> {
        vcpu.0.enter()
    }
    fn vcpu_kick(&self, vcpu: &Self::VcpuControlHandle) -> Result<(), zx::Status> {
        vcpu.0.kick()
    }

    fn vmar_allocate(&self, size: u64) -> Result<(Self::AddressSpaceHandle, usize), zx::Status> {
        FuchsiaHypervisor::vmar_allocate(size)
    }

    fn vmar_map(
        &self,
        vmar: &Self::AddressSpaceHandle,
        vmar_offset: usize,
        vmo: &zx::Vmo,
        vmo_offset: u64,
        length: usize,
        flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status> {
        FuchsiaHypervisor::vmar_map(vmar, vmar_offset, vmo, vmo_offset, length, flags)
    }

    unsafe fn vmar_destroy(&self, vmar: &Self::AddressSpaceHandle) -> Result<(), zx::Status> {
        FuchsiaHypervisor::vmar_destroy(vmar)
    }
}

#[cfg(test)]
pub mod testing {
    use {
        super::*,
        std::{
            cell::RefCell,
            rc::Rc,
            sync::{mpsc, Arc, Condvar, Mutex},
        },
    };

    #[derive(Clone, Debug, PartialEq)]
    pub struct MockMemoryMapping {
        pub vmar_offset: usize,
        pub vmo_offset: u64,
        pub length: usize,
        pub flags: zx::VmarFlags,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum MockAddressSpaceType {
        FullMock,
        RealVmar,
        Destroyed,
    }

    #[derive(Debug)]
    pub struct MockAddressSpace {
        pub inner: zx::Vmar,
        pub mappings: Vec<MockMemoryMapping>,
        pub mock_type: MockAddressSpaceType,
    }

    impl MockAddressSpace {
        // TODO(fxbug.dev/102872): Allow setting the behavior to use the mock or the real VMAR.
        pub fn new_with_invalid_handle() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                inner: zx::Handle::invalid().into(),
                mappings: Vec::new(),
                mock_type: MockAddressSpaceType::FullMock,
            }))
        }

        pub fn new_with_valid_handle(size: u64) -> Result<(Rc<RefCell<Self>>, usize), zx::Status> {
            let (inner, address) = FuchsiaHypervisor::vmar_allocate(size)?;
            Ok((
                Rc::new(RefCell::new(Self {
                    inner,
                    mappings: Vec::new(),
                    mock_type: MockAddressSpaceType::RealVmar,
                })),
                address,
            ))
        }
    }

    #[derive(Debug)]
    pub struct MockGuest {
        vcpus: Arc<Mutex<Vec<MockVcpuController>>>,
    }

    impl MockGuest {
        pub fn new() -> Self {
            Self { vcpus: Arc::new(Mutex::new(Vec::new())) }
        }

        pub fn vcpus(&self) -> Vec<MockVcpuController> {
            self.vcpus.lock().unwrap().clone()
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

        // A counter that tracks the number of times the vcpu thread has called `vcpu_enter`. Tests can use this
        // to wait on the count increasing to determine when the vcpu has completed handling a vm-exit.
        entry_counter: Arc<VcpuEntryCounter>,
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

        pub fn send_packet(
            &self,
            packet: zx::Packet,
        ) -> Result<(), mpsc::SendError<Result<zx::Packet, zx::Status>>> {
            self.packet_sender.send(Ok(packet))
        }

        pub fn initial_ip(&self) -> usize {
            self.initial_ip
        }

        pub fn entry_counter(&self) -> Arc<VcpuEntryCounter> {
            Arc::clone(&self.entry_counter)
        }
    }

    /// This is a simple wait-able counter that tests can use to block until a VCPU has
    /// called `Hypervisor::vcpu_enter` a certain number of times.
    ///
    /// This is useful to determine when the vcpu has completed handling a vm-exit.
    #[derive(Debug)]
    pub struct VcpuEntryCounter {
        count: Mutex<usize>,
        signal: Condvar,
    }

    impl VcpuEntryCounter {
        pub fn new() -> Arc<Self> {
            Arc::new(Self { count: Mutex::new(0), signal: Condvar::new() })
        }

        /// Increments the counter, returning the new value.
        pub fn increment(&self) -> usize {
            let mut count = self.count.lock().unwrap();
            *count = *count + 1;
            self.signal.notify_all();
            *count
        }

        pub fn wait_for_count(&self, target_count: usize) {
            let mut count = self.count.lock().unwrap();
            while *count < target_count {
                count = self.signal.wait(count).unwrap();
            }
        }

        pub fn get_count(&self) -> usize {
            *self.count.lock().unwrap()
        }
    }

    #[derive(Debug)]
    pub struct MockVcpu {
        state: RefCell<zx::sys::zx_vcpu_state_t>,
        packet_receiver: mpsc::Receiver<Result<zx::Packet, zx::Status>>,
        entry_counter: Arc<VcpuEntryCounter>,
    }

    impl MockVcpu {
        pub fn new(initial_ip: usize) -> (MockVcpu, MockVcpuController) {
            let (packet_sender, packet_receiver) = mpsc::channel();
            let entry_counter = VcpuEntryCounter::new();
            (
                MockVcpu {
                    state: RefCell::new(Default::default()),
                    packet_receiver,
                    entry_counter: entry_counter.clone(),
                },
                MockVcpuController { packet_sender, initial_ip, entry_counter },
            )
        }

        #[allow(dead_code)]
        pub fn update_registers<F: Fn(&mut zx::sys::zx_vcpu_state_t)>(&self, f: F) {
            f(&mut *self.state.borrow_mut());
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
        on_vcpu_write_io: MockBehavior,
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
    pub fn mock_guest_create() -> Result<(MockGuest, Rc<RefCell<MockAddressSpace>>), zx::Status> {
        let guest = MockGuest::new();
        let address_space = MockAddressSpace::new_with_invalid_handle();
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
        guest.vcpus.lock().unwrap().push(controller.clone());
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
    /// The default callable used to implement [Hypervisor::vcpu_write_state] for [MockHypervisor].
    #[cfg(target_arch = "x86_64")]
    pub fn mock_vcpu_write_io(vcpu: &MockVcpu, value: &[u8]) -> Result<(), zx::Status> {
        // Handle this by updating the rax register within the current vcpu state.
        let rax = match value.len() {
            1 => value[0] as u64,
            2 => u16::from_ne_bytes(value.try_into().unwrap()) as u64,
            4 => u32::from_ne_bytes(value.try_into().unwrap()) as u64,
            _ => return Err(zx::Status::INVALID_ARGS),
        };
        let mask = u64::MAX << (value.len() << 3);
        vcpu.update_registers(|state| {
            state.rax = (state.rax & mask) | rax;
        });
        Ok(())
    }
    #[cfg(target_arch = "aarch64")]
    pub fn mock_vcpu_write_io(_vcpu: &MockVcpu, _value: &[u8]) -> Result<(), zx::Status> {
        // No io instructions on arm64.
        unimplemented!();
    }

    /// The default callable used to implement [Hypervisor::vcpu_enter] for [MockHypervisor].
    pub fn mock_vcpu_enter(vcpu: &MockVcpu) -> Result<zx::Packet, zx::Status> {
        // Block-on the packet receiver until the test sends a packet or status return. This allows
        // the test to simulate VM exits using an mpsc channel.
        vcpu.entry_counter.increment();
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
            self.inner.lock().unwrap().on_allocate_memory = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::guest_create] is called.
        pub fn on_guest_create(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_guest_create = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_create] is called.
        pub fn on_vcpu_create(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_create = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_read_state] is called.
        pub fn on_vcpu_read_state(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_read_state = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_write_state] is called.
        pub fn on_vcpu_write_state(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_write_state = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_write_io] is called.
        pub fn on_vcpu_write_io(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_write_io = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_enter] is called.
        pub fn on_vcpu_enter(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_enter = behavior;
        }

        /// Specify the behavior of [MockHypervisor] when [Hypervisor::vcpu_kick] is called.
        pub fn on_vcpu_kick(&self, behavior: MockBehavior) {
            self.inner.lock().unwrap().on_vcpu_kick = behavior;
        }
    }

    impl Hypervisor for MockHypervisor {
        type GuestHandle = MockGuest;
        type AddressSpaceHandle = Rc<RefCell<MockAddressSpace>>;
        type VcpuHandle = MockVcpu;
        type VcpuControlHandle = MockVcpuController;

        fn guest_create(
            &self,
        ) -> Result<(Self::GuestHandle, Self::AddressSpaceHandle), zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_guest_create.return_error() {
                return Err(status);
            }
            mock_guest_create()
        }
        fn guest_set_io_trap(
            &self,
            _guest: &Self::GuestHandle,
            _address: u16,
            _size: u16,
            _key: u64,
        ) -> Result<(), zx::Status> {
            // TODO: we could track the set of traps and provide proper errors
            // here. Otherwise a fake here is not too interesting since trap
            // packages are synthetically generated in tests anyways.
            Ok(())
        }

        fn allocate_memory(&self, bytes: u64) -> Result<zx::Vmo, zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_allocate_memory.return_error() {
                return Err(status);
            }
            mock_allocate_memory(bytes)
        }

        fn vmar_allocate(
            &self,
            size: u64,
        ) -> Result<(Self::AddressSpaceHandle, usize), zx::Status> {
            MockAddressSpace::new_with_valid_handle(size)
        }

        fn vmar_map(
            &self,
            vmar: &Self::AddressSpaceHandle,
            vmar_offset: usize,
            vmo: &zx::Vmo,
            vmo_offset: u64,
            length: usize,
            flags: zx::VmarFlags,
        ) -> Result<usize, zx::Status> {
            assert!(vmar.borrow().mock_type != MockAddressSpaceType::Destroyed);
            let location = if let MockAddressSpaceType::RealVmar = vmar.borrow().mock_type {
                FuchsiaHypervisor::vmar_map(
                    &vmar.borrow().inner,
                    vmar_offset,
                    vmo,
                    vmo_offset,
                    length,
                    flags,
                )?
            } else {
                vmar_offset
            };

            vmar.borrow_mut().mappings.push(MockMemoryMapping {
                vmar_offset,
                vmo_offset,
                length,
                flags,
            });

            Ok(location)
        }

        unsafe fn vmar_destroy(&self, vmar: &Self::AddressSpaceHandle) -> Result<(), zx::Status> {
            assert!(vmar.borrow().mock_type != MockAddressSpaceType::Destroyed);
            if let MockAddressSpaceType::RealVmar = vmar.borrow().mock_type {
                FuchsiaHypervisor::vmar_destroy(&vmar.borrow().inner)?;
            }

            vmar.borrow_mut().mock_type = MockAddressSpaceType::Destroyed;
            vmar.borrow_mut().mappings.clear();

            Ok(())
        }

        fn vcpu_create(
            &self,
            guest: &Self::GuestHandle,
            entry: usize,
        ) -> Result<(Self::VcpuHandle, Self::VcpuControlHandle), zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_create.return_error() {
                return Err(status);
            }
            mock_vcpu_create(guest, entry)
        }
        fn vcpu_read_state(
            &self,
            vcpu: &Self::VcpuHandle,
        ) -> Result<zx::sys::zx_vcpu_state_t, zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_read_state.return_error() {
                return Err(status);
            }
            mock_vcpu_read_state(vcpu)
        }
        fn vcpu_write_state(
            &self,
            vcpu: &Self::VcpuHandle,
            state: &zx::sys::zx_vcpu_state_t,
        ) -> Result<(), zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_write_state.return_error() {
                return Err(status);
            }
            mock_vcpu_write_state(vcpu, state)
        }
        fn vcpu_write_io(&self, vcpu: &Self::VcpuHandle, value: &[u8]) -> Result<(), zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_write_io.return_error() {
                return Err(status);
            }
            mock_vcpu_write_io(vcpu, value)
        }
        fn vcpu_enter(&self, vcpu: &Self::VcpuHandle) -> Result<zx::Packet, zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_enter.return_error() {
                return Err(status);
            }
            mock_vcpu_enter(vcpu)
        }
        fn vcpu_kick(&self, vcpu: &Self::VcpuControlHandle) -> Result<(), zx::Status> {
            if let Some(status) = self.inner.lock().unwrap().on_vcpu_kick.return_error() {
                return Err(status);
            }
            mock_vcpu_kick(vcpu)
        }
    }
}
