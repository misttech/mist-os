// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::hypervisor::Hypervisor,
    fuchsia_zircon as zx,
    std::sync::{Arc, RwLock},
    vm_device::{
        bus::{PioAddress, PioRange},
        device_manager::{IoManager, PioManager},
        DevicePio,
    },
};

fn bus_error_to_status(e: vm_device::bus::Error) -> zx::Status {
    match e {
        vm_device::bus::Error::DeviceNotFound => zx::Status::NOT_FOUND,
        vm_device::bus::Error::DeviceOverlap => zx::Status::ALREADY_EXISTS,
        vm_device::bus::Error::InvalidAccessLength(_) => zx::Status::IO_INVALID,
        vm_device::bus::Error::InvalidRange => zx::Status::OUT_OF_RANGE,
    }
}

pub struct DeviceManager<H: Hypervisor> {
    io: Arc<RwLock<IoManager>>,
    hypervisor: H,
    guest: Arc<H::GuestHandle>,
}

impl<H: Hypervisor> Clone for DeviceManager<H> {
    fn clone(&self) -> Self {
        Self { io: self.io.clone(), hypervisor: self.hypervisor.clone(), guest: self.guest.clone() }
    }
}

#[allow(dead_code)]
impl<H: Hypervisor> DeviceManager<H> {
    pub fn new(hypervisor: H, guest: Arc<H::GuestHandle>) -> Self {
        Self { io: Arc::new(RwLock::new(IoManager::new())), hypervisor, guest }
    }

    pub fn register_pio<D: DevicePio + Send + Sync + 'static>(
        &self,
        range: PioRange,
        device: Arc<D>,
    ) -> Result<(), zx::Status> {
        self.io.write().unwrap().register_pio(range, device).map_err(bus_error_to_status)?;
        self.hypervisor.guest_set_io_trap(self.guest.as_ref(), range.base().0, range.size(), 0)
    }

    pub fn pio_read(&self, addr: PioAddress, data: &mut [u8]) -> Result<(), zx::Status> {
        self.io.read().unwrap().pio_read(addr, data).map_err(bus_error_to_status)
    }

    pub fn pio_write(&self, addr: PioAddress, data: &[u8]) -> Result<(), zx::Status> {
        self.io.read().unwrap().pio_write(addr, data).map_err(bus_error_to_status)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hw::MemoryDevice,
        crate::hypervisor::testing::{MockGuest, MockHypervisor},
        std::sync::{Arc, Mutex},
        vm_device::bus::{PioAddress, PioRange},
    };

    struct TestFixture {
        pub _hypervisor: MockHypervisor,
        pub _guest: Arc<MockGuest>,
        pub devices: DeviceManager<MockHypervisor>,
    }

    impl TestFixture {
        pub fn new() -> Self {
            let hypervisor = MockHypervisor::new();
            let (guest, _address_space) = hypervisor.guest_create().unwrap();
            let guest = Arc::new(guest);
            let devices = DeviceManager::new(hypervisor.clone(), guest.clone());
            Self { _hypervisor: hypervisor, _guest: guest, devices }
        }

        pub fn pio_read_one_byte(&self, addr: PioAddress) -> u8 {
            let mut byte = [0u8];
            self.devices.pio_read(addr, &mut byte).unwrap();
            byte[0]
        }

        pub fn pio_write_one_byte(&self, addr: PioAddress, v: u8) {
            let byte = [v];
            self.devices.pio_write(addr, &byte).unwrap();
        }
    }

    #[fuchsia::test]
    fn read_write_pio_device() {
        const MEMORY_SIZE: u16 = 1024;
        const BASE_ADDRESS: PioAddress = PioAddress(1024);

        // Add a Pio device to the bus.
        let test = TestFixture::new();
        let device = Arc::new(Mutex::new(MemoryDevice::ram_bytes(MEMORY_SIZE)));
        test.devices
            .register_pio(PioRange::new(BASE_ADDRESS, MEMORY_SIZE).unwrap(), device.clone())
            .unwrap();

        // Verify initial state of the memory device.
        for offset in 0..MEMORY_SIZE {
            assert_eq!(0, test.pio_read_one_byte(BASE_ADDRESS + offset));
        }

        // Now write a value to each byte and then read it back.
        for offset in 0..MEMORY_SIZE {
            test.pio_write_one_byte(BASE_ADDRESS + offset, offset.wrapping_mul(0xff) as u8);
        }
        for offset in 0..MEMORY_SIZE {
            assert_eq!(
                offset.wrapping_mul(0xff) as u8,
                test.pio_read_one_byte(BASE_ADDRESS + offset)
            );
        }
    }

    #[fuchsia::test]
    fn register_overlapping_pio_device() {
        const BASE_ADDRESS: PioAddress = PioAddress(0);
        const MEMORY_SIZE: u16 = 2;

        // Create 2 memory devices.
        let test = TestFixture::new();
        let memory1 = Arc::new(Mutex::new(MemoryDevice::ram_bytes(MEMORY_SIZE)));
        let memory2 = Arc::new(Mutex::new(MemoryDevice::ram_bytes(MEMORY_SIZE)));

        // Add the first one. This will succeed.
        test.devices
            .register_pio(PioRange::new(BASE_ADDRESS, MEMORY_SIZE).unwrap(), memory1)
            .unwrap();

        // Add the second one with 1 byte overlap with the first device. This should fail.
        let result = test.devices.register_pio(
            PioRange::new(BASE_ADDRESS + (MEMORY_SIZE - 1), MEMORY_SIZE).unwrap(),
            memory2,
        );
        assert_eq!(Err(zx::Status::ALREADY_EXISTS), result);
    }

    #[fuchsia::test]
    fn read_write_unmapped_pio_address() {
        let test = TestFixture::new();

        // Read from a port with no mapped devices.
        let mut byte = [0u8];
        let result = test.devices.pio_read(PioAddress(0), &mut byte);
        assert_eq!(Err(zx::Status::NOT_FOUND), result);
        let result = test.devices.pio_write(PioAddress(0), &byte);
        assert_eq!(Err(zx::Status::NOT_FOUND), result);
    }

    #[fuchsia::test]
    fn read_write_partially_mapped_pio() {
        const BASE_ADDRESS: PioAddress = PioAddress(0);
        const MEMORY_SIZE: u16 = 1;

        let test = TestFixture::new();
        let device = Arc::new(Mutex::new(MemoryDevice::ram_bytes(MEMORY_SIZE)));
        test.devices
            .register_pio(PioRange::new(BASE_ADDRESS, MEMORY_SIZE).unwrap(), device.clone())
            .unwrap();

        let mut byte = [0u8; 2];

        // 1-byte valid access.
        let result = test.devices.pio_read(PioAddress(0), &mut byte[..1]);
        assert_eq!(Ok(()), result);
        // 2-byte (1 valid, 1 invalid) access.
        let result = test.devices.pio_read(PioAddress(0), &mut byte[..2]);
        assert_eq!(Err(zx::Status::NOT_FOUND), result);
    }
}
