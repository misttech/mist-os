// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::mm::memory::MemoryObject;
use crate::mm::vmar::AllocatedVmar;
use crate::mm::{MemoryAccessor, ProtectionFlags, PAGE_SIZE};
use crate::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use dense_map::DenseMap;
use ebpf::MapSchema;

use starnix_lifecycle::AtomicU32Counter;
use starnix_logging::{track_stub, with_zx_name};
use starnix_sync::{BpfMapEntries, LockBefore, Locked, OrderedMutex};
use starnix_uapi::errors::Errno;
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::vfs::FdEvents;
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
    bpf_map_type_BPF_MAP_TYPE_XSKMAP, errno, error, from_status_like_fdio, BPF_EXIST, BPF_NOEXIST,
    BPF_RB_FORCE_WAKEUP, BPF_RB_NO_WAKEUP, BPF_RINGBUF_BUSY_BIT, BPF_RINGBUF_DISCARD_BIT,
    BPF_RINGBUF_HDR_SZ,
};
use static_assertions::const_assert;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ops::{Bound, Deref, DerefMut, Range, RangeBounds};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Counter for map identifiers.
static MAP_IDS: AtomicU32Counter = AtomicU32Counter::new(1);

/// A BPF map. This is a hashtable that can be accessed both by BPF programs and userspace.
#[derive(Debug)]
pub struct Map {
    pub id: u32,
    pub schema: MapSchema,
    pub flags: u32,

    // This field should be private to this module.
    entries: OrderedMutex<MapStore, BpfMapEntries>,
}

/// Maps are normally kept pinned in memory since linked eBPF programs store direct pointers to
/// the maps they depend on.
pub type PinnedMap = Pin<Arc<Map>>;

impl Map {
    pub fn new(schema: MapSchema, flags: u32) -> Result<Self, Errno> {
        let id = MAP_IDS.next();
        let store = MapStore::new(&schema)?;
        Ok(Self { id, schema, flags, entries: OrderedMutex::new(store) })
    }

    pub fn get_raw<L>(&self, locked: &mut Locked<'_, L>, key: &[u8]) -> Option<*mut u8>
    where
        L: LockBefore<BpfMapEntries>,
    {
        let mut entries = self.entries.lock(locked);
        Self::get(&mut entries, &self.schema, key).map(|s| s.as_mut_ptr())
    }

    pub fn lookup<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        key: &[u8],
        user_value: UserAddress,
    ) -> Result<(), Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
        let mut entries = self.entries.lock(locked);
        if let Some(value) = Self::get(&mut entries, &self.schema, key) {
            current_task.write_memory(user_value, value).map(|_| ())
        } else {
            error!(ENOENT)
        }
    }

    pub fn update<L>(
        &self,
        locked: &mut Locked<'_, L>,
        key: Vec<u8>,
        value: &[u8],
        flags: u64,
    ) -> Result<(), Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
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
            MapStore::RingBuffer(_) => return error!(EINVAL),
        }
        Ok(())
    }

    pub fn delete<L>(&self, locked: &mut Locked<'_, L>, key: &[u8]) -> Result<(), Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
        let mut entries = self.entries.lock(locked);
        match entries.deref_mut() {
            MapStore::Hash(ref mut entries) => {
                if !entries.remove(key) {
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
            MapStore::RingBuffer(_) => return error!(EINVAL),
        }
        Ok(())
    }

    pub fn get_next_key<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        key: Option<Vec<u8>>,
        user_next_key: UserAddress,
    ) -> Result<(), Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
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
            MapStore::RingBuffer(_) => return error!(EINVAL),
        }
        Ok(())
    }

    pub fn get_memory<L>(
        &self,
        locked: &mut Locked<'_, L>,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
        self.entries
            .lock(locked)
            .as_ringbuf()
            .ok_or_else(|| errno!(ENODEV))?
            .get_memory(length, prot)
    }

    pub fn wait_async<L>(
        &self,
        locked: &mut Locked<'_, L>,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler>
    where
        L: LockBefore<BpfMapEntries>,
    {
        self.entries.lock(locked).as_ringbuf()?.wait_async(waiter, events, handler)
    }

    pub fn query_events<L>(&self, locked: &mut Locked<'_, L>) -> Result<FdEvents, Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
        let mut entries = self.entries.lock(locked);
        match entries.deref_mut() {
            MapStore::RingBuffer(ringbuf) => ringbuf.query_events(),
            _ => Ok(FdEvents::empty()),
        }
    }

    pub fn ringbuf_reserve<L>(
        &self,
        locked: &mut Locked<'_, L>,
        size: u32,
        flags: u64,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<BpfMapEntries>,
    {
        if flags != 0 {
            return error!(EINVAL);
        }
        self.entries.lock(locked).as_ringbuf().ok_or_else(|| errno!(EINVAL))?.reserve(size)
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
        let page_size = *PAGE_SIZE as usize;
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

            // These types are in use, but not yet implemented. Incorrectly use Array or Hash for
            // these
            bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP_HASH");
                Ok(MapStore::Hash(HashStorage::new(&schema)?))
            }
            bpf_map_type_BPF_MAP_TYPE_RINGBUF => {
                if schema.key_size != 0 || schema.value_size != 0 {
                    return error!(EINVAL);
                }
                Ok(MapStore::RingBuffer(RingBufferStorage::new(schema.max_entries as usize)?))
            }
            bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => {
                track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_ARRAY");
                // From <https://man7.org/linux/man-pages/man2/bpf.2.html>:
                //   The key is an array index, and must be exactly four
                //   bytes.
                if schema.key_size != 4 {
                    return error!(EINVAL);
                }
                let buffer_size = compute_storage_size(schema)?;
                Ok(MapStore::Array(new_pinned_buffer(buffer_size)))
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

    fn as_ringbuf(&mut self) -> Option<&mut RingBufferStorage> {
        match self {
            Self::RingBuffer(rb) => Some(rb),
            _ => None,
        }
    }
}

fn compute_storage_size(schema: &MapSchema) -> Result<usize, Errno> {
    schema
        .value_size
        .checked_mul(schema.max_entries)
        .map(|v| v as usize)
        .ok_or_else(|| errno!(ENOMEM))
}

#[derive(Debug)]
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

    fn contains_key(&self, key: &[u8]) -> bool {
        self.index_map.contains_key(key)
    }

    fn get(&mut self, schema: &MapSchema, key: &[u8]) -> Option<&'_ mut [u8]> {
        if let Some(index) = self.index_map.get(key) {
            Some(&mut self.data[array_range_for_index(schema.value_size, *index as u32)])
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

#[derive(Debug)]
struct RingBufferStorage {
    memory: Arc<MemoryObject>,
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
    /// WaitQueue to notify userspace when new data is available.
    wait_queue: WaitQueue,
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
    fn new(size: usize) -> Result<Pin<Box<Self>>, Errno> {
        let page_size: usize = *PAGE_SIZE as usize;
        // Size must be a power of 2 and a multiple of PAGE_SIZE
        if size == 0 || size % page_size != 0 || size & (size - 1) != 0 {
            return error!(EINVAL);
        }
        let mask: u32 = (size - 1).try_into().map_err(|_| errno!(EINVAL))?;
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
            )?
        };
        let technical_vmo = with_zx_name(
            zx::Vmo::create(page_size as u64).map_err(|e| from_status_like_fdio!(e))?,
            b"starnix:bpf",
        );
        vmar.map(
            0,
            &technical_vmo,
            0,
            page_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|e| from_status_like_fdio!(e))?;

        let vmo = with_zx_name(
            zx::Vmo::create(vmo_size as u64).map_err(|e| from_status_like_fdio!(e))?,
            b"starnix:bpf",
        );
        vmar.map(
            page_size,
            &vmo,
            0,
            vmo_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|e| from_status_like_fdio!(e))?;
        vmar.map(
            page_size + vmo_size,
            &vmo,
            0,
            size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|e| from_status_like_fdio!(e))?;
        // SAFETY
        //
        // This is safe as long as the vmar mapping stays alive. This will be ensured by the
        // `RingBufferStorage` itself.
        let storage_position = unsafe { &mut *((base) as *mut *const Self) };
        let consumer_position = unsafe { &*((base + page_size) as *const AtomicU32) };
        let producer_position = unsafe { &*((base + 2 * page_size) as *const AtomicU32) };
        let data = base + 3 * page_size;
        let storage = Box::pin(Self {
            memory: Arc::new(MemoryObject::RingBuf(vmo)),
            mask,
            consumer_position,
            producer_position,
            data,
            wait_queue: Default::default(),
            _vmar: vmar,
        });
        // Store the pointer to the storage to the start of the technical vmo. This is required to
        // access the storage from the bpf methods that only get a pointer to the reserved memory.
        // This is safe as the returned referenced is Pinned.
        *storage_position = storage.deref();
        Ok(storage)
    }

    /// The size of the ring buffer.
    fn size(&self) -> usize {
        (self.mask + 1) as usize
    }

    /// Reserve `size` bytes on the ringbuffer.
    fn reserve(&mut self, size: u32) -> Result<usize, Errno> {
        //  The top two bits are used as special flags.
        if size & (BPF_RINGBUF_BUSY_BIT | BPF_RINGBUF_DISCARD_BIT) > 0 {
            return error!(EINVAL);
        }
        let consumer_position = self.consumer_position.load(Ordering::Acquire);
        let producer_position = self.producer_position.load(Ordering::Acquire);
        let max_size = self.mask + 1;
        // Available size on the ringbuffer.
        let consumed_size =
            producer_position.checked_sub(consumer_position).ok_or_else(|| errno!(EINVAL))?;
        let available_size = max_size.checked_sub(consumed_size).ok_or_else(|| errno!(EINVAL))?;
        // Total size of the message to write. This is the requested size + the header.
        let total_size: u32 =
            round_up_to_increment(size + BPF_RINGBUF_HDR_SZ, std::mem::size_of::<u64>())?;
        if total_size > available_size {
            return error!(ENOMEM);
        }
        let data_position = self.data_position(producer_position + BPF_RINGBUF_HDR_SZ);
        let data_length = size | BPF_RINGBUF_BUSY_BIT;
        let page_count = ((data_position - self.data) / (*PAGE_SIZE as usize) + 3)
            .try_into()
            .map_err(|_| errno!(EFBIG))?;
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
            self.wait_queue.notify_fd_events(FdEvents::POLLIN);
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

    pub fn get_memory(
        &self,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        // Because of the specific condition needed to map this object, the size must be known.
        let Some(length) = length else {
            return error!(EINVAL);
        };
        // This cannot be mapped executable.
        if prot.contains(ProtectionFlags::EXEC) {
            return error!(EPERM);
        }
        // The first page has no restriction on read/write protection, just return the memory.
        if length <= *PAGE_SIZE as usize {
            return Ok(self.memory.clone());
        }
        // Starting from the second page, this cannot be mapped writable.
        if prot.contains(ProtectionFlags::WRITE) {
            return error!(EPERM);
        }
        // This cannot be mapped outside of the 2 control pages and the 2 data sections.
        if length > 2 * (*PAGE_SIZE as usize) + 2 * self.size() {
            return error!(EINVAL);
        }
        Ok(self.memory.clone())
    }

    pub fn wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.wait_queue.wait_async_fd_events(waiter, events, handler))
    }

    pub fn query_events(&mut self) -> Result<FdEvents, Errno> {
        let consumer_position = self.consumer_position.load(Ordering::Acquire);
        let producer_position = self.producer_position.load(Ordering::Acquire);
        if consumer_position < producer_position {
            // Read the header at the consumer position, and check that the entry is not busy.
            let header = self.header_mut(producer_position);
            if *header.length.get_mut() & BPF_RINGBUF_BUSY_BIT == 0 {
                return Ok(FdEvents::POLLIN);
            }
        }
        Ok(FdEvents::empty())
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
