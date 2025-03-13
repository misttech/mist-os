// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{compute_map_storage_size, new_pinned_buffer, MapError, MapImpl, MapKey, PinnedBuffer};
use ebpf::MapSchema;
use fuchsia_sync::Mutex;
use linux_uapi::BPF_NOEXIST;
use std::fmt::Debug;
use std::ops::Range;
use std::sync::Arc;

fn array_key_to_index(key: &[u8]) -> u32 {
    u32::from_ne_bytes(key.try_into().expect("incorrect key length"))
}

#[derive(Debug)]
pub struct Array {
    // TODO(https://fxbug.dev/378507648): replace with a VMO.
    buffer: Mutex<PinnedBuffer>,

    num_entries: usize,
    value_size: usize,
}

impl Array {
    pub fn new(schema: &MapSchema) -> Result<Self, MapError> {
        // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
        //   The key is an array index, and must be exactly four
        //   bytes.
        if schema.key_size != 4 {
            return Err(MapError::InvalidParam);
        }
        let size = compute_map_storage_size(schema)? as usize;
        Ok(Array {
            buffer: Mutex::new(new_pinned_buffer(size)),
            num_entries: schema.max_entries as usize,
            value_size: schema.value_size as usize,
        })
    }

    fn array_key_to_range(&self, key: &[u8]) -> Option<Range<usize>> {
        let index = array_key_to_index(key) as usize;
        if index >= self.num_entries {
            return None;
        }
        let base = index * self.value_size;
        let limit = base + self.value_size;
        Some((base as usize)..(limit as usize))
    }
}

impl MapImpl for Array {
    fn get_raw(&self, key: &[u8]) -> Option<*mut u8> {
        Some(self.buffer.lock()[self.array_key_to_range(key)?].as_ptr() as *mut u8)
    }

    fn lookup(&self, key: &[u8]) -> Option<Vec<u8>> {
        Some(self.buffer.lock()[self.array_key_to_range(key)?].to_vec())
    }

    fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        assert!(value.len() == self.value_size);

        let range = self.array_key_to_range(&key).ok_or(MapError::SizeLimit)?;

        if flags == BPF_NOEXIST as u64 {
            return Err(MapError::EntryExists);
        }

        self.buffer.lock()[range].copy_from_slice(value);

        Ok(())
    }

    fn delete(&self, _key: &[u8]) -> Result<(), MapError> {
        // From https://man7.org/linux/man-pages/man2/bpf.2.html:
        //
        //  map_delete_elem() fails with the error EINVAL, since
        //  elements cannot be deleted.
        Err(MapError::InvalidParam)
    }

    fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        let next_index = key.map(|v| array_key_to_index(v) + 1).unwrap_or(0);
        if next_index as usize >= self.num_entries {
            return Err(MapError::InvalidKey);
        }
        Ok(MapKey::from_slice(&next_index.to_ne_bytes()))
    }

    fn vmo(&self) -> Option<Arc<zx::Vmo>> {
        // TODO(https://fxbug.dev/378507648): Store the value in VMO and return it here.
        None
    }
}
