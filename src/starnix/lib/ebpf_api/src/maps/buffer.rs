// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::MapError;
use ebpf::EbpfBufferPtr;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

#[derive(Debug)]
pub(super) struct MapBuffer {
    vmo: Arc<zx::Vmo>,
    addr: usize,
    size: usize,
}

fn round_up_to_page_size(size: usize) -> usize {
    let page_size = *MapBuffer::PAGE_SIZE;
    (size + page_size - 1) & !(page_size - 1)
}

/// Memory buffer used to store data for eBPF maps. The data is stored in a VMO
/// which may be shared with other processes. Read and write operations are
/// allowed from any thread. They are performed as a sequence of 64-bit atomic
/// reads/writes without any ordering guarantees.
impl MapBuffer {
    pub const PAGE_SIZE: LazyLock<usize> = LazyLock::new(|| zx::system_get_page_size() as usize);

    /// Alignment required for `read()` and `write()` operations.
    pub const ALIGNMENT: usize = EbpfBufferPtr::ALIGNMENT;

    pub fn new(size: usize, vmo: Option<zx::Vmo>) -> Result<Self, MapError> {
        let vmo_size = round_up_to_page_size(size);
        let vmo = match vmo {
            Some(vmo) => {
                let actual_vmo_size = vmo.get_size().map_err(|_| MapError::InvalidVmo)? as usize;
                if vmo_size != actual_vmo_size {
                    return Err(MapError::InvalidVmo);
                }
                vmo
            }
            None => zx::Vmo::create(vmo_size as u64).map_err(|e| match e {
                zx::Status::NO_MEMORY | zx::Status::OUT_OF_RANGE => MapError::NoMemory,
                _ => MapError::Internal,
            })?,
        };

        let addr = fuchsia_runtime::vmar_root_self()
            .map(0, &vmo, 0, vmo_size, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
            .map_err(|_| MapError::InvalidVmo)?;

        Ok(Self { vmo: Arc::new(vmo), addr, size })
    }

    pub fn vmo(&self) -> &Arc<zx::Vmo> {
        &self.vmo
    }

    // Returns pointer to the mapped contents of the buffer.
    pub fn ptr<'a>(&'a self) -> EbpfBufferPtr<'a> {
        // SAFETY: the memory is mapped at `addr` for the lifetime of the
        // buffer.
        unsafe { EbpfBufferPtr::new(self.addr as *mut u8, self.size) }
    }

    pub fn round_up_to_alignment(value_size: usize) -> Option<usize> {
        Some(value_size.checked_add(Self::ALIGNMENT - 1)? & !(Self::ALIGNMENT - 1))
    }
}

impl Drop for MapBuffer {
    fn drop(&mut self) {
        // SAFETY: Maps owning the buffer are expected to be dropped only
        // after all references to the data stored in the Map are dropped as
        // well. This is ensured by `ebpf::EbpfProgram` keeping strong
        // references to every map used by the program.
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(self.addr, round_up_to_page_size(self.size))
                .expect("Failed to unmap VMO.");
        }
    }
}
