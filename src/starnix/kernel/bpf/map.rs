// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::{mm::MemoryAccessor, task::CurrentTask};
use dense_map::DenseMap;
use ebpf::MapSchema;
use starnix_logging::track_stub;
use starnix_sync::{BpfMapEntries, Locked, OrderedMutex, Unlocked};
use starnix_uapi::{
    bpf_map_type_BPF_MAP_TYPE_ARRAY, bpf_map_type_BPF_MAP_TYPE_ARRAY_OF_MAPS,
    bpf_map_type_BPF_MAP_TYPE_BLOOM_FILTER, bpf_map_type_BPF_MAP_TYPE_CGROUP_ARRAY,
    bpf_map_type_BPF_MAP_TYPE_CGROUP_STORAGE, bpf_map_type_BPF_MAP_TYPE_CGRP_STORAGE,
    bpf_map_type_BPF_MAP_TYPE_CPUMAP, bpf_map_type_BPF_MAP_TYPE_DEVMAP,
    bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH, bpf_map_type_BPF_MAP_TYPE_HASH,
    bpf_map_type_BPF_MAP_TYPE_HASH_OF_MAPS, bpf_map_type_BPF_MAP_TYPE_INODE_STORAGE,
    bpf_map_type_BPF_MAP_TYPE_LPM_TRIE, bpf_map_type_BPF_MAP_TYPE_LRU_HASH,
    bpf_map_type_BPF_MAP_TYPE_LRU_PERCPU_HASH, bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY,
    bpf_map_type_BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE, bpf_map_type_BPF_MAP_TYPE_PERCPU_HASH,
    bpf_map_type_BPF_MAP_TYPE_PERF_EVENT_ARRAY, bpf_map_type_BPF_MAP_TYPE_PROG_ARRAY,
    bpf_map_type_BPF_MAP_TYPE_QUEUE, bpf_map_type_BPF_MAP_TYPE_REUSEPORT_SOCKARRAY,
    bpf_map_type_BPF_MAP_TYPE_RINGBUF, bpf_map_type_BPF_MAP_TYPE_SK_STORAGE,
    bpf_map_type_BPF_MAP_TYPE_SOCKHASH, bpf_map_type_BPF_MAP_TYPE_SOCKMAP,
    bpf_map_type_BPF_MAP_TYPE_STACK, bpf_map_type_BPF_MAP_TYPE_STACK_TRACE,
    bpf_map_type_BPF_MAP_TYPE_STRUCT_OPS, bpf_map_type_BPF_MAP_TYPE_TASK_STORAGE,
    bpf_map_type_BPF_MAP_TYPE_UNSPEC, bpf_map_type_BPF_MAP_TYPE_USER_RINGBUF,
    bpf_map_type_BPF_MAP_TYPE_XSKMAP, errno, error, errors::Errno, user_address::UserAddress,
    BPF_EXIST, BPF_NOEXIST,
};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    ops::{Bound, Deref, DerefMut, Range, RangeBounds},
    pin::Pin,
};

/// A BPF map. This is a hashtable that can be accessed both by BPF programs and userspace.
pub struct Map {
    pub schema: MapSchema,
    pub flags: u32,

    // This field should be private to this module.
    entries: OrderedMutex<MapStore, BpfMapEntries>,
}

impl Map {
    pub fn new(schema: MapSchema, flags: u32) -> Result<Self, Errno> {
        let store = MapStore::new(&schema)?;
        Ok(Self { schema, flags, entries: OrderedMutex::new(store) })
    }

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
                let Some(value) = entries.get(&self.schema, &key) else {
                    return error!(ENOENT);
                };
                current_task.write_memory(user_value, value)?;
            }
            MapStore::Array(entries) => {
                let index = array_key_to_index(&key);
                if index >= self.schema.max_entries {
                    return error!(ENOENT);
                }
                let value = &entries[array_range_for_index(self.schema.value_size, index)];
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
            MapStore::Hash(ref mut storage) => {
                storage.update(&self.schema, key, &value, flags)?;
            }
            MapStore::Array(ref mut entries) => {
                let index = array_key_to_index(&key);
                if index >= self.schema.max_entries {
                    return error!(E2BIG);
                }
                if flags == BPF_NOEXIST as u64 {
                    return error!(EEXIST);
                }
                entries[array_range_for_index(self.schema.value_size, index)]
                    .copy_from_slice(&value);
            }
        }
        Ok(())
    }

    pub fn delete(&self, locked: &mut Locked<'_, Unlocked>, key: Vec<u8>) -> Result<(), Errno> {
        let mut entries = self.entries.lock(locked);
        match entries.deref_mut() {
            MapStore::Hash(ref mut entries) => {
                if !entries.remove(&key) {
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
                let next_key = next_entry.ok_or_else(|| errno!(ENOENT))?;
                current_task.write_memory(user_next_key, next_key)?;
            }
            MapStore::Array(_) => {
                let next_index = if let Some(key) = key { array_key_to_index(&key) + 1 } else { 0 };
                if next_index >= self.schema.max_entries {
                    return error!(ENOENT);
                }
                current_task.write_memory(user_next_key, &next_index.to_ne_bytes())?;
            }
        }
        Ok(())
    }
}

type PinnedBuffer = Pin<Box<[u8]>>;

/// The underlying storage for a BPF map.
///
/// We will eventually need to implement a wide variety of backing stores.
enum MapStore {
    Array(PinnedBuffer),
    Hash(HashStorage),
}

impl MapStore {
    pub fn new(schema: &MapSchema) -> Result<Self, Errno> {
        match schema.map_type {
            bpf_map_type_BPF_MAP_TYPE_ARRAY => {
                // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
                //   The key is an array index, and must be exactly four
                //   bytes.
                if schema.key_size != 4 {
                    return error!(EINVAL);
                }
                let buffer_size = compute_storage_size(schema)?;
                Ok(MapStore::Array(new_pinned_buffer(buffer_size)))
            }
            bpf_map_type_BPF_MAP_TYPE_HASH => Ok(MapStore::Hash(HashStorage::new(&schema)?)),

            // These types are in use, but not yet implemented. Incorrectly use Hash for these
            bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP_HASH");
                Ok(MapStore::Hash(HashStorage::new(&schema)?))
            }
            bpf_map_type_BPF_MAP_TYPE_RINGBUF => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_RINGBUF");
                Ok(MapStore::Hash(HashStorage::new(&schema)?))
            }

            // Unimplemented types
            bpf_map_type_BPF_MAP_TYPE_UNSPEC => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_UNSPEC");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_PROG_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PROG_ARRAY");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_PERF_EVENT_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERF_EVENT_ARRAY");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_HASH");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_ARRAY");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_STACK_TRACE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STACK_TRACE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_CGROUP_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGROUP_ARRAY");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_LRU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LRU_HASH");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_LRU_PERCPU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LRU_PERCPU_HASH");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_LPM_TRIE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LPM_TRIE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_ARRAY_OF_MAPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_ARRAY_OF_MAPS");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_HASH_OF_MAPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_HASH_OF_MAPS");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_DEVMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_SOCKMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SOCKMAP");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_CPUMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CPUMAP");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_XSKMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_XSKMAP");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_SOCKHASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SOCKHASH");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_CGROUP_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGROUP_STORAGE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_REUSEPORT_SOCKARRAY => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "BPF_MAP_TYPE_REUSEPORT_SOCKARRAY"
                );
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE"
                );
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_QUEUE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_QUEUE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_STACK => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STACK");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_SK_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SK_STORAGE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_STRUCT_OPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STRUCT_OPS");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_INODE_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_INODE_STORAGE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_TASK_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_TASK_STORAGE");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_BLOOM_FILTER => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_BLOOM_FILTER");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_USER_RINGBUF => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_USER_RINGBUF");
                error!(EINVAL)
            }
            bpf_map_type_BPF_MAP_TYPE_CGRP_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGRP_STORAGE");
                error!(EINVAL)
            }
            _ => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "unknown bpf map type",
                    schema.map_type
                );
                error!(EINVAL)
            }
        }
    }
}

fn compute_storage_size(schema: &MapSchema) -> Result<usize, Errno> {
    schema
        .value_size
        .checked_mul(schema.max_entries)
        .map(|v| v as usize)
        .ok_or_else(|| errno!(EINVAL))
}

struct HashStorage {
    index_map: BTreeMap<Vec<u8>, usize>,
    data: PinnedBuffer,
    free_list: DenseMap<()>,
}

impl HashStorage {
    fn new(schema: &MapSchema) -> Result<Self, Errno> {
        let buffer_size = compute_storage_size(schema)?;
        let data = new_pinned_buffer(buffer_size);
        Ok(Self { index_map: Default::default(), data, free_list: Default::default() })
    }

    fn len(&self) -> usize {
        self.index_map.len()
    }

    fn contains_key(&self, key: &Vec<u8>) -> bool {
        self.index_map.contains_key(key)
    }

    fn get(&self, schema: &MapSchema, key: &Vec<u8>) -> Option<&'_ [u8]> {
        if let Some(index) = self.index_map.get(key) {
            Some(&self.data[array_range_for_index(schema.value_size, *index as u32)])
        } else {
            None
        }
    }

    pub fn range<R>(&self, range: R) -> impl Iterator<Item = &Vec<u8>>
    where
        R: RangeBounds<Vec<u8>>,
    {
        self.index_map.range(range).map(|(k, _)| k)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.index_map.iter().map(|(k, _)| k)
    }

    fn remove(&mut self, key: &Vec<u8>) -> bool {
        if let Some(index) = self.index_map.remove(key) {
            self.free_list.remove(index);
            true
        } else {
            false
        }
    }

    fn update(
        &mut self,
        schema: &MapSchema,
        key: Vec<u8>,
        value: &[u8],
        flags: u64,
    ) -> Result<(), Errno> {
        let map_is_full = self.len() >= schema.max_entries as usize;
        match self.index_map.entry(key) {
            Entry::Vacant(entry) => {
                if map_is_full {
                    return error!(E2BIG);
                }
                if flags == BPF_EXIST as u64 {
                    return error!(ENOENT);
                }
                let data_index = self.free_list.push(());
                entry.insert(data_index);
                self.data[array_range_for_index(schema.value_size, data_index as u32)]
                    .copy_from_slice(value);
            }
            Entry::Occupied(entry) => {
                if flags == BPF_NOEXIST as u64 {
                    return error!(EEXIST);
                }
                self.data[array_range_for_index(schema.value_size, *entry.get() as u32)]
                    .copy_from_slice(value);
            }
        }
        Ok(())
    }
}

fn new_pinned_buffer(size: usize) -> PinnedBuffer {
    vec![0u8; size].into_boxed_slice().into()
}

fn array_key_to_index(key: &[u8]) -> u32 {
    u32::from_ne_bytes(key.try_into().expect("incorrect key length"))
}

fn array_range_for_index(value_size: u32, index: u32) -> Range<usize> {
    let base = index * value_size;
    let limit = base + value_size;
    (base as usize)..(limit as usize)
}
