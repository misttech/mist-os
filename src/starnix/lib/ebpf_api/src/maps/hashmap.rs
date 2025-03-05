// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{compute_map_storage_size, new_pinned_buffer, MapError, MapImpl, MapKey, PinnedBuffer};
use dense_map::DenseMap;
use ebpf::MapSchema;
use fuchsia_sync::Mutex;
use linux_uapi::{BPF_EXIST, BPF_NOEXIST};
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

#[derive(Debug)]
struct HashMapState {
    max_entries: usize,
    value_size: usize,
    index_map: BTreeMap<MapKey, usize>,
    data: PinnedBuffer,
    free_list: DenseMap<()>,
}

impl HashMapState {
    fn len(&self) -> usize {
        self.index_map.len()
    }

    fn get_value_by_index(&mut self, index: usize) -> &mut [u8] {
        let base = index * self.value_size;
        let limit = base + self.value_size;
        &mut self.data[base..limit]
    }

    fn get(&mut self, key: &[u8]) -> Option<&'_ mut [u8]> {
        self.index_map.get(key).map(|i| *i).map(|i| self.get_value_by_index(i))
    }

    pub fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        let next_entry = match key {
            Some(key) if self.index_map.contains_key(key) => {
                self.index_map.range::<[u8], _>((Bound::Excluded(key), Bound::Unbounded)).next()
            }
            _ => self.index_map.iter().next(),
        };
        let key = next_entry.ok_or(MapError::InvalidKey)?.0;
        Ok(MapKey::from_slice(&key[..]))
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), MapError> {
        if let Some(index) = self.index_map.remove(key) {
            self.free_list.remove(index);
            Ok(())
        } else {
            Err(MapError::InvalidKey)
        }
    }

    fn update(&mut self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        let map_is_full = self.len() >= self.max_entries as usize;
        let data_index;
        match self.index_map.entry(key) {
            Entry::Vacant(entry) => {
                if map_is_full {
                    return Err(MapError::SizeLimit);
                }
                if flags == BPF_EXIST as u64 {
                    return Err(MapError::InvalidKey);
                }
                data_index = self.free_list.push(());
                entry.insert(data_index);
            }
            Entry::Occupied(entry) => {
                if flags == BPF_NOEXIST as u64 {
                    return Err(MapError::EntryExists);
                }
                data_index = *entry.get();
            }
        }
        self.get_value_by_index(data_index).copy_from_slice(value);
        Ok(())
    }
}

#[derive(Debug)]
pub struct HashMap {
    state: Mutex<HashMapState>,
}

impl HashMap {
    pub fn new(schema: &MapSchema) -> Result<Self, MapError> {
        let buffer_size = compute_map_storage_size(schema)? as usize;
        let data = new_pinned_buffer(buffer_size);
        Ok(Self {
            state: Mutex::new(HashMapState {
                max_entries: schema.max_entries as usize,
                value_size: schema.value_size as usize,
                index_map: Default::default(),
                data,
                free_list: Default::default(),
            }),
        })
    }
}

impl MapImpl for HashMap {
    fn get_raw(&self, key: &[u8]) -> Option<*mut u8> {
        self.state.lock().get(key).map(|s| s.as_mut_ptr())
    }

    fn lookup(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.state.lock().get(key).map(|s| s.to_vec())
    }

    fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        self.state.lock().update(key, value, flags)
    }

    fn delete(&self, key: &[u8]) -> Result<(), MapError> {
        self.state.lock().delete(key)
    }

    fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        self.state.lock().get_next_key(key)
    }

    fn vmo(&self) -> Option<Arc<zx::Vmo>> {
        // Hash maps cannot be memory-mapped.
        None
    }
}
