// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use vm_device::{
    bus::{PioAddress, PioAddressOffset},
    MutDevicePio,
};

enum MemoryMode {
    ReadWrite,
}

impl MemoryMode {
    pub fn is_writable(&self) -> bool {
        match self {
            MemoryMode::ReadWrite => true,
        }
    }
}

/// `MemoryDevice` is a simple device that reads and writes bytes to a backing vector.
pub struct MemoryDevice(Vec<u8>, MemoryMode);

// Some definitions are only used on x64 right now, but there's nothing platform
// specific about them.
#[allow(dead_code)]
impl MemoryDevice {
    pub fn ram_bytes(size: u16) -> Self {
        MemoryDevice(vec![0; size as usize], MemoryMode::ReadWrite)
    }

    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.0.as_mut_slice()
    }
}

impl MutDevicePio for MemoryDevice {
    fn pio_read(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &mut [u8]) {
        let offset = offset as usize;
        assert!(offset + data.len() <= self.0.len());
        data.copy_from_slice(&self.0[offset..offset + data.len()]);
    }

    fn pio_write(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &[u8]) {
        if self.1.is_writable() {
            let offset = offset as usize;
            assert!(offset + data.len() <= self.0.len());
            self.0[offset..offset + data.len()].copy_from_slice(data);
        }
    }
}
