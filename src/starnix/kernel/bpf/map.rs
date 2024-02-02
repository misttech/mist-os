// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::{bpf::fs::BpfObject, mm::MemoryAccessor, task::CurrentTask};
use starnix_sync::{BpfMapEntries, Locked, OrderedMutex, Unlocked};
use starnix_uapi::{
    bpf_map_type_BPF_MAP_TYPE_ARRAY, errno, error, errors::Errno, user_address::UserAddress,
    BPF_EXIST, BPF_NOEXIST,
};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    ops::{Bound, Deref, DerefMut},
};
use ubpf::MapSchema;

/// The underlying storage for a BPF map.
///
/// We will eventually need to implement a wide variety of backing stores.
pub enum MapStore {
    Hash(BTreeMap<Vec<u8>, Vec<u8>>),
    Array(Vec<u8>),
}

/// A BPF map. This is a hashtable that can be accessed both by BPF programs and userspace.
pub struct Map {
    pub schema: MapSchema,
    pub flags: u32,

    // This field should be private to this module.
    pub entries: OrderedMutex<MapStore, BpfMapEntries>,
}
impl BpfObject for Map {}

impl MapStore {
    pub fn new(schema: &MapSchema) -> Result<Self, Errno> {
        if schema.map_type == bpf_map_type_BPF_MAP_TYPE_ARRAY {
            // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
            //   The key is an array index, and must be exactly four
            //   bytes.
            if schema.key_size != 4 {
                return error!(EINVAL);
            }
            Ok(MapStore::Array(vec![0u8; (schema.value_size * schema.max_entries) as usize]))
        } else {
            Ok(MapStore::Hash(Default::default()))
        }
    }
}

fn key_to_index(key: &[u8]) -> u32 {
    u32::from_ne_bytes(key.try_into().expect("incorrect key length"))
}

impl Map {
    pub fn lookup(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        key: Vec<u8>,
        user_value: UserAddress,
    ) -> Result<(), Errno> {
        let entries = self.entries.lock(locked);
        match entries.deref() {
            MapStore::Hash(ref entries) => {
                let Some(value) = entries.get(&key) else {
                    return error!(ENOENT);
                };
                current_task.write_memory(user_value, value)?;
            }
            MapStore::Array(entries) => {
                let index = key_to_index(&key);
                if index >= self.schema.max_entries {
                    return error!(ENOENT);
                }
                let value = &entries[self.schema.array_range_for_index(index)];
                current_task.write_memory(user_value, value)?;
            }
        }
        Ok(())
    }

    pub fn update(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        key: Vec<u8>,
        value: Vec<u8>,
        flags: u64,
    ) -> Result<(), Errno> {
        let mut entries = self.entries.lock(locked);
        match entries.deref_mut() {
            MapStore::Hash(ref mut entries) => {
                let map_is_full = entries.len() >= self.schema.max_entries as usize;
                match entries.entry(key) {
                    Entry::Vacant(entry) => {
                        if map_is_full {
                            return error!(E2BIG);
                        }
                        if flags == BPF_EXIST as u64 {
                            return error!(ENOENT);
                        }
                        entry.insert(value);
                    }
                    Entry::Occupied(mut entry) => {
                        if flags == BPF_NOEXIST as u64 {
                            return error!(EEXIST);
                        }
                        entry.insert(value);
                    }
                }
            }
            MapStore::Array(ref mut entries) => {
                let index = key_to_index(&key);
                if index >= self.schema.max_entries {
                    return error!(E2BIG);
                }
                if flags == BPF_NOEXIST as u64 {
                    return error!(EEXIST);
                }
                entries[self.schema.array_range_for_index(index)].copy_from_slice(&value);
            }
        }
        Ok(())
    }

    pub fn delete(&self, locked: &mut Locked<'_, Unlocked>, key: Vec<u8>) -> Result<(), Errno> {
        let mut entries = self.entries.lock(locked);
        match entries.deref_mut() {
            MapStore::Hash(ref mut entries) => {
                if entries.remove(&key).is_none() {
                    return error!(ENOENT);
                }
            }
            MapStore::Array(_) => {
                // From https://man7.org/linux/man-pages/man2/bpf.2.html:
                //
                //  map_delete_elem() fails with the error EINVAL, since
                //  elements cannot be deleted.
                return error!(EINVAL);
            }
        }
        Ok(())
    }

    pub fn get_next_key(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        key: Option<Vec<u8>>,
        user_next_key: UserAddress,
    ) -> Result<(), Errno> {
        let entries = self.entries.lock(locked);
        match entries.deref() {
            MapStore::Hash(ref entries) => {
                let next_entry = match key {
                    Some(key) if entries.contains_key(&key) => {
                        entries.range((Bound::Excluded(key), Bound::Unbounded)).next()
                    }
                    _ => entries.iter().next(),
                };
                let (next_key, _next_value) = next_entry.ok_or_else(|| errno!(ENOENT))?;
                current_task.write_memory(user_next_key, next_key)?;
            }
            MapStore::Array(_) => {
                let next_index = if let Some(key) = key { key_to_index(&key) + 1 } else { 0 };
                if next_index >= self.schema.max_entries {
                    return error!(ENOENT);
                }
                current_task.write_memory(user_next_key, &next_index.to_ne_bytes())?;
            }
        }
        Ok(())
    }
}
