// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef};
use vmo_types::allocations_table_v1::AllocationsTableWriter;

/// We cap the size of our backing VMO at 2 GiB, then preallocate it and map it entirely.
/// Actual memory for each page will only be committed when we first write to that page.
const VMO_SIZE: usize = 1 << 31;

// SAFETY: The provided buffer is nul-terminated.
const VMO_NAME: &std::ffi::CStr =
    unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"heapdump-allocations\0") };

/// Tracks live allocations by storing their metadata in a dedicated VMO.
pub struct AllocationsTable {
    #[allow(dead_code)] // TODO(fdurso): To be removed once VMO sharing is implemented.
    vmo: zx::Vmo,
    writer: AllocationsTableWriter,
}

impl Default for AllocationsTable {
    fn default() -> AllocationsTable {
        let vmo = zx::Vmo::create(VMO_SIZE as u64).expect("failed to create allocations VMO");
        vmo.set_name(VMO_NAME).expect("failed to set VMO name");

        let writer = AllocationsTableWriter::new(&vmo).expect("failed to create writer");
        AllocationsTable { vmo, writer }
    }
}

impl AllocationsTable {
    pub fn record_allocation(&mut self, address: u64, size: u64) {
        let inserted = self.writer.insert_allocation(address, size).expect("out of space");
        assert!(inserted, "Block 0x{:x} was already allocated", address);
    }

    pub fn forget_allocation(&mut self, address: u64) -> u64 {
        if let Some(size) = self.writer.erase_allocation(address) {
            size
        } else {
            panic!("Block 0x{:x} was not allocated", address);
        }
    }
}
