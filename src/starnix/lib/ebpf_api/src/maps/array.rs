// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{new_pinned_buffer, MapError, MapImpl, MapKey, PinnedBuffer};
use ebpf::MapSchema;
use fuchsia_sync::Mutex;
use linux_uapi::BPF_NOEXIST;
use std::fmt::Debug;
use std::ops::Range;
use std::sync::Arc;

fn array_key_to_index(key: &[u8]) -> u32 {
    u32::from_ne_bytes(key.try_into().expect("incorrect key length"))
}

fn get_value_storage_size(value_size: usize) -> Option<usize> {
    // Ensure 8-byte alignment for all values stored in the map.
    const ALIGNMENT: usize = 8;
    Some(value_size.checked_add(ALIGNMENT - 1)? & !(ALIGNMENT - 1))
}

#[derive(Debug)]
pub struct Array {
    // TODO(https://fxbug.dev/378507648): replace with a VMO.
    buffer: Mutex<PinnedBuffer>,

    num_entries: usize,
    value_size: usize,

    // Number of bytes per element. May be greater than `value_size` to ensure
    // proper alignment for the elements.
    bytes_per_element: usize,
}

impl Array {
    const MAX_ARRAY_SIZE: usize = u32::MAX as usize;

    pub fn new(schema: &MapSchema) -> Result<Self, MapError> {
        // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
        //   The key is an array index, and must be exactly four
        //   bytes.
        if schema.key_size != 4 {
            return Err(MapError::InvalidParam);
        }

        let bytes_per_element =
            get_value_storage_size(schema.value_size as usize).ok_or(MapError::InvalidParam)?;
        let size = bytes_per_element
            .checked_mul(schema.max_entries as usize)
            .ok_or(MapError::InvalidParam)?;
        if size > Self::MAX_ARRAY_SIZE {
            return Err(MapError::NoMemory);
        }

        Ok(Array {
            buffer: Mutex::new(new_pinned_buffer(size)),
            num_entries: schema.max_entries as usize,
            value_size: schema.value_size as usize,
            bytes_per_element,
        })
    }

    fn array_key_to_range(&self, key: &[u8]) -> Option<Range<usize>> {
        let index = array_key_to_index(key) as usize;
        if index >= self.num_entries {
            return None;
        }
        let base = index * self.bytes_per_element;
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

#[cfg(test)]
mod test {
    use super::*;
    use linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY;

    // Verifies that array elements are always 8-byte aligned.
    #[fuchsia::test]
    fn test_alignment() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_ARRAY,
            key_size: 4,
            value_size: 5,
            max_entries: 10,
        };

        let array = Array::new(&schema).unwrap();
        assert_eq!(array.get_raw(&[0, 0, 0, 0]).unwrap() as usize % 8, 0);
        assert_eq!(array.get_raw(&[1, 0, 0, 0]).unwrap() as usize % 8, 0);

        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_ARRAY,
            key_size: 4,
            value_size: 10,
            max_entries: 10,
        };

        let array = Array::new(&schema).unwrap();
        assert_eq!(array.get_raw(&[0, 0, 0, 0]).unwrap() as usize % 8, 0);
        assert_eq!(array.get_raw(&[1, 0, 0, 0]).unwrap() as usize % 8, 0);
    }
}
