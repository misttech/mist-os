// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, AsHandleRef, HandleBased};
pub use heapdump_vmo::resources_table_v1::ResourceKey;
use heapdump_vmo::resources_table_v1::ResourcesTableWriter;
use std::sync::atomic::{fence, Ordering::Release};

/// We cap the size of our backing VMO at 2 GiB, then preallocate it and map it entirely.
/// Actual memory for each page will only be committed when we first write to that page.
const VMO_SIZE: usize = 1 << 31;

// SAFETY: The provided buffer is nul-terminated.
const VMO_NAME: &std::ffi::CStr =
    unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"heapdump-resources\0") };

/// Stores immutable resources in a dedicated VMO.
pub struct ResourcesTable {
    vmo: zx::Vmo,
    writer: ResourcesTableWriter,
}

impl Default for ResourcesTable {
    fn default() -> ResourcesTable {
        let vmo = zx::Vmo::create(VMO_SIZE as u64).expect("failed to create resources VMO");
        vmo.set_name(VMO_NAME).expect("failed to set VMO name");

        let writer = ResourcesTableWriter::new(&vmo).expect("failed to create writer");
        ResourcesTable { vmo, writer }
    }
}

impl ResourcesTable {
    /// Duplicates the handle to the underlying VMO.
    pub fn share_vmo(&self) -> zx::Vmo {
        self.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("failed to share resources VMO")
    }

    /// Inserts a compressed stack trace, or finds a previously-inserted copy.
    pub fn intern_stack_trace(&mut self, compressed_stack_trace: &[u8]) -> ResourceKey {
        let (resource_key, inserted) = self
            .writer
            .intern_compressed_stack_trace(compressed_stack_trace)
            .expect("failed to insert stack trace");

        // If we have just inserted a new stack trace, ensure it's committed to the underlying VMO.
        if inserted {
            fence(Release);
        }

        resource_key
    }

    /// Inserts information about a thread.
    pub fn insert_thread_info(
        &mut self,
        koid: zx::Koid,
        name: &[u8; zx::sys::ZX_MAX_NAME_LEN],
    ) -> ResourceKey {
        self.writer.insert_thread_info(koid.raw_koid(), name).expect("failed to insert thread info")
    }
}
