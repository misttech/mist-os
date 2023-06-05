// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased};
use heapdump_vmo::allocations_table_v1::{AllocationsTableWriter, ResourceKey};

/// We cap the size of our backing VMO at 2 GiB, then preallocate it and map it entirely.
/// Actual memory for each page will only be committed when we first write to that page.
const VMO_SIZE: usize = 1 << 31;

// SAFETY: The provided buffer is nul-terminated.
const VMO_NAME: &std::ffi::CStr =
    unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"heapdump-allocations\0") };

/// Tracks live allocations by storing their metadata in a dedicated VMO.
pub struct AllocationsTable {
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
    /// Duplicate the handle to the underlying VMO.
    pub fn share_vmo(&self) -> zx::Vmo {
        self.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("failed to share allocations VMO")
    }

    // Take a snapshot of the underlying VMO.
    pub fn snapshot_vmo(&self) -> zx::Vmo {
        self.vmo
            .create_child(
                zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::NO_WRITE,
                0,
                VMO_SIZE as u64,
            )
            .expect("failed to snapshot allocations VMO")
    }

    pub fn record_allocation(
        &mut self,
        address: u64,
        size: u64,
        stack_trace_key: ResourceKey,
        timestamp: i64,
    ) {
        let inserted = self
            .writer
            .insert_allocation(address, size, stack_trace_key, timestamp)
            .expect("out of space");
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
