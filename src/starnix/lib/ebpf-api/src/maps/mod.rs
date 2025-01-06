// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

mod vmar;

use vmar::AllocatedVmar;

use dense_map::DenseMap;
use ebpf::MapSchema;
use fuchsia_sync::Mutex;
use inspect_stubs::track_stub;
use linux_uapi::{
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
    bpf_map_type_BPF_MAP_TYPE_XSKMAP, BPF_EXIST, BPF_NOEXIST, BPF_RB_FORCE_WAKEUP,
    BPF_RB_NO_WAKEUP, BPF_RINGBUF_BUSY_BIT, BPF_RINGBUF_DISCARD_BIT, BPF_RINGBUF_HDR_SZ,
};
use static_assertions::const_assert;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ops::{Bound, Deref, DerefMut, Range};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use zx::AsHandleRef;

static PAGE_SIZE: LazyLock<usize> = LazyLock::new(|| zx::system_get_page_size() as usize);

/// Counter for map identifiers.
static MAP_IDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub enum MapError {
    // Equivalent of EINVAL.
    InvalidParam,

    // No entry with the specified key,
    InvalidKey,

    // Entry already exists..
    EntryExists,

    // Map size limit has been reached.
    SizeLimit,

    // Cannot allocate memory.
    NoMemory,

    // An internal issue, e.g. failed to allocate VMO.
    Internal,
}

/// A BPF map. This is a hashtable that can be accessed both by BPF programs and userspace.
#[derive(Debug)]
pub struct Map {
    pub id: u32,
    pub schema: MapSchema,
    pub flags: u32,

    // This field should be private to this module.
    entries: Mutex<MapStore>,
}

/// Maps are normally kept pinned in memory since linked eBPF programs store direct pointers to
/// the maps they depend on.
pub type PinnedMap = Pin<Arc<Map>>;

// Avoid allocation for eBPF keys smaller than 16 bytes.
pub type MapKey = smallvec::SmallVec<[u8; 16]>;

impl Map {
    pub fn new(schema: MapSchema, flags: u32) -> Result<Self, MapError> {
        let id = MAP_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed).into();
        let store = MapStore::new(&schema)?;
        Ok(Self { id, schema, flags, entries: Mutex::new(store) })
    }

    pub fn get_raw(&self, key: &[u8]) -> Option<*mut u8> {
        let mut entries = self.entries.lock();
        Self::get(&mut entries, &self.schema, key).map(|s| s.as_mut_ptr())
    }

    pub fn lookup(&self, key: &[u8]) -> Result<Vec<u8>, MapError> {
        let mut entries = self.entries.lock();
        if let Some(value) = Self::get(&mut entries, &self.schema, key) {
            Ok(value.to_vec())
        } else {
            Err(MapError::InvalidKey)
        }
    }

    pub fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        let mut entries = self.entries.lock();
        match entries.deref_mut() {
            MapStore::Hash(ref mut storage) => {
                storage.update(&self.schema, key, &value, flags)?;
            }
            MapStore::Array(ref mut entries) => {
                let index = array_key_to_index(&key);
                if index >= self.schema.max_entries {
                    return Err(MapError::SizeLimit);
                }
                if flags == BPF_NOEXIST as u64 {
                    return Err(MapError::EntryExists);
                }
                entries[array_range_for_index(self.schema.value_size, index)]
                    .copy_from_slice(&value);
            }
            MapStore::RingBuffer(_) => return Err(MapError::InvalidParam),
        }
        Ok(())
    }

    pub fn delete(&self, key: &[u8]) -> Result<(), MapError> {
        let mut entries = self.entries.lock();
        match entries.deref_mut() {
            MapStore::Hash(ref mut entries) => {
                if !entries.remove(key) {
                    return Err(MapError::InvalidKey);
                }
            }
            MapStore::Array(_) => {
                // From https://man7.org/linux/man-pages/man2/bpf.2.html:
                //
                //  map_delete_elem() fails with the error EINVAL, since
                //  elements cannot be deleted.
                return Err(MapError::InvalidParam);
            }
            MapStore::RingBuffer(_) => return Err(MapError::InvalidParam),
        }
        Ok(())
    }

    pub fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        let entries = self.entries.lock();
        match entries.deref() {
            MapStore::Hash(ref entries) => entries.get_next_key(key),
            MapStore::Array(_) => {
                let next_index = if let Some(key) = key { array_key_to_index(&key) + 1 } else { 0 };
                if next_index >= self.schema.max_entries {
                    return Err(MapError::InvalidKey);
                }
                Ok(MapKey::from_slice(&next_index.to_ne_bytes()))
            }
            MapStore::RingBuffer(_) => Err(MapError::InvalidParam),
        }
    }

    pub fn vmo(&self) -> Option<Arc<zx::Vmo>> {
        // TODO(https://fxbug.dev/378507648): Arrays should be mappable as well.
        self.entries.lock().as_ringbuf().map(|rb| rb.vmo().clone())
    }

    pub fn can_read(&self) -> bool {
        match *self.entries.lock() {
            MapStore::RingBuffer(ref mut ringbuf) => ringbuf.can_read(),
            _ => false,
        }
    }

    pub fn ringbuf_reserve(&self, size: u32, flags: u64) -> Result<usize, MapError> {
        if flags != 0 {
            return Err(MapError::InvalidParam);
        }
        self.entries.lock().as_ringbuf().ok_or_else(|| MapError::InvalidParam)?.reserve(size)
    }

    /// Submit the data.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned by a previous call to `ringbuf_reserve`
    /// on a map that has not been dropped, otherwise the behaviour is UB.
    pub unsafe fn ringbuf_submit(addr: u64, flags: RingBufferWakeupPolicy) {
        let addr = addr as usize;
        let (ringbuf_storage, header) = Self::ringbuf_get_storage_and_header(addr);
        ringbuf_storage.commit(header, flags, false);
    }

    /// Discard the data.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned by a previous call to `ringbuf_reserve`
    /// on a map that has not been dropped, otherwise the behaviour is UB.
    pub unsafe fn ringbuf_discard(addr: u64, flags: RingBufferWakeupPolicy) {
        let addr = addr as usize;
        let (ringbuf_storage, header) = Self::ringbuf_get_storage_and_header(addr);
        ringbuf_storage.commit(header, flags, true);
    }

    fn get<'a>(entries: &'a mut MapStore, schema: &MapSchema, key: &[u8]) -> Option<&'a mut [u8]> {
        match entries {
            MapStore::Hash(ref mut entries) => entries.get(&schema, key),
            MapStore::Array(ref mut entries) => {
                let index = array_key_to_index(&key);
                if index >= schema.max_entries {
                    return None;
                }
                Some(&mut entries[array_range_for_index(schema.value_size, index)])
            }
            MapStore::RingBuffer(_) => None,
        }
    }

    /// Get the `RingBufferStorage` and the `RingBufferRecordHeader` associated with `addr`.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned from a previous call to `ringbuf_reserve` on a `Map` that
    /// has not been dropped and is kept alive as long as the returned value are used.
    unsafe fn ringbuf_get_storage_and_header(
        addr: usize,
    ) -> (&'static RingBufferStorage, &'static RingBufferRecordHeader) {
        let page_size = *PAGE_SIZE;
        // addr is the data section. First access the header.
        let header = &*((addr - std::mem::size_of::<RingBufferRecordHeader>())
            as *const RingBufferRecordHeader);
        let addr_page = addr / page_size;
        let mapping_start_page = addr_page - header.page_count as usize;
        let mapping_start_address = mapping_start_page * page_size;
        let ringbuf_storage = &*(mapping_start_address as *const &RingBufferStorage);
        (ringbuf_storage, header)
    }
}

type PinnedBuffer = Pin<Box<[u8]>>;

/// The underlying storage for a BPF map.
///
/// We will eventually need to implement a wide variety of backing stores.
#[derive(Debug)]
enum MapStore {
    Array(PinnedBuffer),
    Hash(HashStorage),
    RingBuffer(Pin<Box<RingBufferStorage>>),
}

impl MapStore {
    pub fn new(schema: &MapSchema) -> Result<Self, MapError> {
        match schema.map_type {
            bpf_map_type_BPF_MAP_TYPE_ARRAY => {
                // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
                //   The key is an array index, and must be exactly four
                //   bytes.
                if schema.key_size != 4 {
                    return Err(MapError::InvalidParam);
                }
                let buffer_size = compute_map_storage_size(schema)?;
                Ok(MapStore::Array(new_pinned_buffer(buffer_size)))
            }
            bpf_map_type_BPF_MAP_TYPE_HASH => Ok(MapStore::Hash(HashStorage::new(&schema)?)),

            // These types are in use, but not yet implemented. Incorrectly use Array or Hash for
            // these
            bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP_HASH");
                Ok(MapStore::Hash(HashStorage::new(&schema)?))
            }
            bpf_map_type_BPF_MAP_TYPE_RINGBUF => {
                if schema.key_size != 0 || schema.value_size != 0 {
                    return Err(MapError::InvalidParam);
                }
                Ok(MapStore::RingBuffer(RingBufferStorage::new(schema.max_entries as usize)?))
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_ARRAY");
                // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
                //   The key is an array index, and must be exactly four
                //   bytes.
                if schema.key_size != 4 {
                    return Err(MapError::InvalidParam);
                }
                let buffer_size = compute_map_storage_size(schema)?;
                Ok(MapStore::Array(new_pinned_buffer(buffer_size)))
            }

            // Unimplemented types
            bpf_map_type_BPF_MAP_TYPE_UNSPEC => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_UNSPEC");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_PROG_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PROG_ARRAY");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_PERF_EVENT_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERF_EVENT_ARRAY");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_HASH");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_STACK_TRACE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STACK_TRACE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_CGROUP_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGROUP_ARRAY");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_LRU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LRU_HASH");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_LRU_PERCPU_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LRU_PERCPU_HASH");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_LPM_TRIE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LPM_TRIE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_ARRAY_OF_MAPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_ARRAY_OF_MAPS");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_HASH_OF_MAPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_HASH_OF_MAPS");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_DEVMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_SOCKMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SOCKMAP");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_CPUMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CPUMAP");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_XSKMAP => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_XSKMAP");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_SOCKHASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SOCKHASH");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_CGROUP_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGROUP_STORAGE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_REUSEPORT_SOCKARRAY => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "BPF_MAP_TYPE_REUSEPORT_SOCKARRAY"
                );
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE"
                );
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_QUEUE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_QUEUE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_STACK => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STACK");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_SK_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SK_STORAGE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_STRUCT_OPS => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_STRUCT_OPS");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_INODE_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_INODE_STORAGE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_TASK_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_TASK_STORAGE");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_BLOOM_FILTER => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_BLOOM_FILTER");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_USER_RINGBUF => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_USER_RINGBUF");
                Err(MapError::InvalidParam)
            }
            bpf_map_type_BPF_MAP_TYPE_CGRP_STORAGE => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_CGRP_STORAGE");
                Err(MapError::InvalidParam)
            }
            _ => {
                track_stub!(
                    TODO("https://fxbug.dev/323847465"),
                    "unknown bpf map type",
                    schema.map_type
                );
                Err(MapError::InvalidParam)
            }
        }
    }

    fn as_ringbuf(&mut self) -> Option<&mut RingBufferStorage> {
        match self {
            Self::RingBuffer(rb) => Some(rb),
            _ => None,
        }
    }
}

pub fn compute_map_storage_size(schema: &MapSchema) -> Result<usize, MapError> {
    schema
        .value_size
        .checked_mul(schema.max_entries)
        .map(|v| v as usize)
        .ok_or_else(|| MapError::NoMemory)
}

#[derive(Debug)]
struct HashStorage {
    index_map: BTreeMap<MapKey, usize>,
    data: PinnedBuffer,
    free_list: DenseMap<()>,
}

impl HashStorage {
    fn new(schema: &MapSchema) -> Result<Self, MapError> {
        let buffer_size = compute_map_storage_size(schema)?;
        let data = new_pinned_buffer(buffer_size);
        Ok(Self { index_map: Default::default(), data, free_list: Default::default() })
    }

    fn len(&self) -> usize {
        self.index_map.len()
    }

    fn get(&mut self, schema: &MapSchema, key: &[u8]) -> Option<&'_ mut [u8]> {
        if let Some(index) = self.index_map.get(key) {
            Some(&mut self.data[array_range_for_index(schema.value_size, *index as u32)])
        } else {
            None
        }
    }

    pub fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        let next_entry = match key {
            Some(key) if self.index_map.contains_key(key) => {
                self.index_map.range::<[u8], _>((Bound::Excluded(key), Bound::Unbounded)).next()
            }
            _ => self.index_map.iter().next(),
        };
        let key = next_entry.ok_or_else(|| MapError::InvalidKey)?.0;
        Ok(MapKey::from_slice(&key[..]))
    }

    fn remove(&mut self, key: &[u8]) -> bool {
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
        key: MapKey,
        value: &[u8],
        flags: u64,
    ) -> Result<(), MapError> {
        let map_is_full = self.len() >= schema.max_entries as usize;
        match self.index_map.entry(key) {
            Entry::Vacant(entry) => {
                if map_is_full {
                    return Err(MapError::SizeLimit);
                }
                if flags == BPF_EXIST as u64 {
                    return Err(MapError::InvalidKey);
                }
                let data_index = self.free_list.push(());
                entry.insert(data_index);
                self.data[array_range_for_index(schema.value_size, data_index as u32)]
                    .copy_from_slice(value);
            }
            Entry::Occupied(entry) => {
                if flags == BPF_NOEXIST as u64 {
                    return Err(MapError::EntryExists);
                }
                self.data[array_range_for_index(schema.value_size, *entry.get() as u32)]
                    .copy_from_slice(value);
            }
        }
        Ok(())
    }
}

// Signal used on ring buffer VMOs to indicate that the buffer has
// incoming data.
pub const RINGBUF_SIGNAL: zx::Signals = zx::Signals::USER_0;

#[derive(Debug)]
struct RingBufferStorage {
    /// VMO used to store the map content. Reference-counted to make it possible to share the
    /// handle with Starnix kernel, particularly for the case when a process needs to wait for
    /// signals from the VMO (see RINGBUF_SIGNAL).
    vmo: Arc<zx::Vmo>,
    /// The mask corresponding to the size of the ring buffer. This is used to map back the
    /// position in the ringbuffer (that are always growing) to their actual position in the memory
    /// object.
    mask: u32,
    /// The never decreasing position of the read head of the ring buffer. This is updated
    /// exclusively from userspace.
    consumer_position: &'static AtomicU32,
    /// The never decreasing position of the writing head of the ring buffer. This is updated
    /// exclusively from the kernel.
    producer_position: &'static AtomicU32,
    /// Pointer to the start of the data of the ring buffer.
    data: usize,
    /// The specific memory address space used to map the ring buffer. This is the last field in
    /// the struct so that all the data that conceptually points to it is destroyed before the
    /// memory is unmapped.
    _vmar: AllocatedVmar,
}

impl RingBufferStorage {
    /// Build a new storage of a ring buffer. `size` must be a non zero multiple of the page size
    /// and a power of 2.
    ///
    /// This will create a mapping in the kernel user space with the following layout:
    ///
    /// |T| |C| |P| |D| |D|
    ///
    /// where:
    /// - T is 1 page containing at its 0 index a pointer to the `RingBufferStorage` itself.
    /// - C is 1 page containing at its 0 index a atomic u32 for the consumer position
    /// - P is 1 page containing at its 0 index a atomic u32 for the producer position
    /// - D is size bytes and is the content of the ring buffer.
    ///
    /// The returns value is a `Pin<Box>`, because the structure is self referencing and is
    /// required never to move in memory.
    fn new(size: usize) -> Result<Pin<Box<Self>>, MapError> {
        let page_size = *PAGE_SIZE;
        // Size must be a power of 2 and a multiple of page_size.
        if size == 0 || size % page_size != 0 || size & (size - 1) != 0 {
            return Err(MapError::InvalidParam);
        }
        let mask: u32 = (size - 1).try_into().map_err(|_| MapError::InvalidParam)?;
        // Add the 2 control pages
        let vmo_size = 2 * page_size + size;
        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();
        // SAFETY
        //
        // The returned value and all pointer to the allocated memory will be part of `Self` and
        // all pointers will be dropped before the vmar. This ensures the deallocated memory will
        // not be used after it has been freed.
        let (vmar, base) = unsafe {
            AllocatedVmar::allocate(
                &kernel_root_vmar,
                0,
                // Allocate for one technical page, the 2 control pages and twice the size.
                page_size + vmo_size + size,
                zx::VmarFlags::CAN_MAP_SPECIFIC
                    | zx::VmarFlags::CAN_MAP_READ
                    | zx::VmarFlags::CAN_MAP_WRITE,
            )
            .map_err(|_| MapError::Internal)?
        };
        let technical_vmo = zx::Vmo::create(page_size as u64).map_err(|_| MapError::Internal)?;
        technical_vmo.set_name(&zx::Name::new_lossy("starnix:bpf")).unwrap();
        vmar.map(
            0,
            &technical_vmo,
            0,
            page_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;

        let vmo = zx::Vmo::create(vmo_size as u64).map_err(|_| MapError::Internal)?;
        vmo.set_name(&zx::Name::new_lossy("starnix:bpf")).unwrap();
        vmar.map(
            page_size,
            &vmo,
            0,
            vmo_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;
        vmar.map(
            page_size + vmo_size,
            &vmo,
            0,
            size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;

        // SAFETY
        //
        // This is safe as long as the vmar mapping stays alive. This will be ensured by the
        // `RingBufferStorage` itself.
        let storage_position = unsafe { &mut *((base) as *mut *const Self) };
        let consumer_position = unsafe { &*((base + page_size) as *const AtomicU32) };
        let producer_position = unsafe { &*((base + 2 * page_size) as *const AtomicU32) };
        let data = base + 3 * page_size;
        let storage = Box::pin(Self {
            vmo: Arc::new(vmo),
            mask,
            consumer_position,
            producer_position,
            data,
            _vmar: vmar,
        });
        // Store the pointer to the storage to the start of the technical vmo. This is required to
        // access the storage from the bpf methods that only get a pointer to the reserved memory.
        // This is safe as the returned referenced is Pinned.
        *storage_position = storage.deref();
        Ok(storage)
    }

    /// Reserve `size` bytes on the ringbuffer.
    fn reserve(&mut self, size: u32) -> Result<usize, MapError> {
        //  The top two bits are used as special flags.
        if size & (BPF_RINGBUF_BUSY_BIT | BPF_RINGBUF_DISCARD_BIT) > 0 {
            return Err(MapError::InvalidParam);
        }
        let consumer_position = self.consumer_position.load(Ordering::Acquire);
        let producer_position = self.producer_position.load(Ordering::Acquire);
        let max_size = self.mask + 1;
        // Available size on the ringbuffer.
        let consumed_size = producer_position
            .checked_sub(consumer_position)
            .ok_or_else(|| MapError::InvalidParam)?;
        let available_size =
            max_size.checked_sub(consumed_size).ok_or_else(|| MapError::InvalidParam)?;

        const HEADER_ALIGNMENT: u32 = std::mem::size_of::<u64>() as u32;

        // Total size of the message to write. This is the requested size + the header, rounded up
        // to align the next header.
        let total_size: u32 = (size + BPF_RINGBUF_HDR_SZ + HEADER_ALIGNMENT - 1) / HEADER_ALIGNMENT
            * HEADER_ALIGNMENT;

        if total_size > available_size {
            return Err(MapError::SizeLimit);
        }
        let data_position = self.data_position(producer_position + BPF_RINGBUF_HDR_SZ);
        let data_length = size | BPF_RINGBUF_BUSY_BIT;
        let page_count = ((data_position - self.data) / *PAGE_SIZE + 3)
            .try_into()
            .map_err(|_| MapError::SizeLimit)?;
        let header = self.header_mut(producer_position);
        *header.length.get_mut() = data_length;
        header.page_count = page_count;
        self.producer_position.store(producer_position + total_size, Ordering::Release);
        Ok(data_position)
    }

    /// Commit the section of the ringbuffer represented by the `header`. This only consist in
    /// updating the header length with the correct state bits and signaling the map fd.
    fn commit(
        &self,
        header: &RingBufferRecordHeader,
        flags: RingBufferWakeupPolicy,
        discard: bool,
    ) {
        let mut new_length = header.length.load(Ordering::Acquire) & !BPF_RINGBUF_BUSY_BIT;
        if discard {
            new_length |= BPF_RINGBUF_DISCARD_BIT;
        }
        header.length.store(new_length, Ordering::Release);

        // Send a signal either if it is forced, or it is the default and the committed entry is
        // the next one the client will consume.
        if flags == RingBufferWakeupPolicy::ForceWakeup
            || (flags == RingBufferWakeupPolicy::DefaultWakeup
                && self.is_consumer_position(header as *const RingBufferRecordHeader as usize))
        {
            self.vmo
                .as_handle_ref()
                .signal(zx::Signals::empty(), RINGBUF_SIGNAL)
                .expect("Failed to set signal or a ring buffer VMO");
        }
    }

    /// The pointer into `data` that corresponds to `position`.
    fn data_position(&self, position: u32) -> usize {
        self.data + ((position & self.mask) as usize)
    }

    fn is_consumer_position(&self, addr: usize) -> bool {
        let Some(position) = addr.checked_sub(self.data) else {
            return false;
        };
        let position = position as u32;
        let consumer_position = self.consumer_position.load(Ordering::Acquire) & self.mask;
        position == consumer_position
    }

    /// Access the memory at `position` as a `RingBufferRecordHeader`.
    fn header_mut(&mut self, position: u32) -> &mut RingBufferRecordHeader {
        // SAFETY
        //
        // Reading / writing to the header is safe because the access is exclusive thanks to the
        // mutable reference to `self` and userspace has only a read only access to this memory.
        unsafe { &mut *(self.data_position(position) as *mut RingBufferRecordHeader) }
    }

    pub fn vmo(&self) -> &Arc<zx::Vmo> {
        &self.vmo
    }

    // Return true if POLLIN should be signaled for this ring buffer.
    pub fn can_read(&mut self) -> bool {
        let consumer_position = self.consumer_position.load(Ordering::Acquire);
        let producer_position = self.producer_position.load(Ordering::Acquire);

        // Read the header at the consumer position, and check that the entry is not busy.
        consumer_position < producer_position
            && ((*self.header_mut(producer_position).length.get_mut()) & BPF_RINGBUF_BUSY_BIT == 0)
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingBufferWakeupPolicy {
    DefaultWakeup = 0,
    NoWakeup = BPF_RB_NO_WAKEUP,
    ForceWakeup = BPF_RB_FORCE_WAKEUP,
}

impl From<u32> for RingBufferWakeupPolicy {
    fn from(v: u32) -> Self {
        match v {
            BPF_RB_NO_WAKEUP => Self::NoWakeup,
            BPF_RB_FORCE_WAKEUP => Self::ForceWakeup,
            // If flags is invalid, use the default value. This is necessary to prevent userspace
            // leaking ringbuf value by calling into the kernel with an incorrect flag value.
            _ => Self::DefaultWakeup,
        }
    }
}

#[repr(C)]
#[repr(align(8))]
#[derive(Debug)]
struct RingBufferRecordHeader {
    length: AtomicU32,
    page_count: u32,
}

const_assert!(std::mem::size_of::<RingBufferRecordHeader>() == BPF_RINGBUF_HDR_SZ as usize);

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

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn test_ring_buffer_wakeup_policy() {
        assert_eq!(RingBufferWakeupPolicy::from(0), RingBufferWakeupPolicy::DefaultWakeup);
        assert_eq!(
            RingBufferWakeupPolicy::from(BPF_RB_NO_WAKEUP),
            RingBufferWakeupPolicy::NoWakeup
        );
        assert_eq!(
            RingBufferWakeupPolicy::from(BPF_RB_FORCE_WAKEUP),
            RingBufferWakeupPolicy::ForceWakeup
        );
        assert_eq!(RingBufferWakeupPolicy::from(42), RingBufferWakeupPolicy::DefaultWakeup);
    }
}
