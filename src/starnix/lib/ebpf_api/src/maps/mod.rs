// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

mod array;
mod hashmap;
mod ring_buffer;
mod vmar;

pub use ring_buffer::{RingBuffer, RingBufferWakeupPolicy, RINGBUF_SIGNAL};

use ebpf::{BpfValue, MapReference, MapSchema};
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
    bpf_map_type_BPF_MAP_TYPE_XSKMAP,
};
use std::fmt::Debug;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

/// Counter for map identifiers.
static MAP_IDS: AtomicU32 = AtomicU32::new(1);

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

pub trait MapImpl: Send + Sync + Debug {
    fn get_raw(&self, key: &[u8]) -> Option<*mut u8>;
    fn lookup(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError>;
    fn delete(&self, key: &[u8]) -> Result<(), MapError>;
    fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError>;
    fn vmo(&self) -> Option<Arc<zx::Vmo>>;

    // Returns true if `POLLIN` is signaled for the map FD. Should be
    // overridden only for ring buffers.
    fn can_read(&self) -> Option<bool> {
        None
    }

    fn ringbuf_reserve(&self, _size: u32, _flags: u64) -> Result<usize, MapError> {
        Err(MapError::InvalidParam)
    }
}

/// A BPF map. This is a hashtable that can be accessed both by BPF programs and userspace.
#[derive(Debug)]
pub struct Map {
    pub id: u32,
    pub schema: MapSchema,
    pub flags: u32,

    // The impl because it's required for some map implementations need to be
    // pinned, particularly ring buffers.
    map_impl: Pin<Box<dyn MapImpl + Sync>>,
}

/// Maps are normally kept pinned in memory since linked eBPF programs store direct pointers to
/// the maps they depend on.
#[derive(Debug, Clone)]
pub struct PinnedMap(Pin<Arc<Map>>);

impl Deref for PinnedMap {
    type Target = Map;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl MapReference for PinnedMap {
    fn schema(&self) -> &MapSchema {
        &self.0.schema
    }

    fn as_bpf_value(&self) -> BpfValue {
        BpfValue::from(self.deref() as *const Map)
    }
}

// Avoid allocation for eBPF keys smaller than 16 bytes.
pub type MapKey = smallvec::SmallVec<[u8; 16]>;

impl Map {
    pub fn new(schema: MapSchema, flags: u32) -> Result<PinnedMap, MapError> {
        let id = MAP_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed).into();
        let map_impl = create_map_impl(&schema)?;
        Ok(PinnedMap(Arc::pin(Self { id, schema, flags, map_impl })))
    }

    pub fn get_raw(&self, key: &[u8]) -> Option<*mut u8> {
        self.map_impl.get_raw(key)
    }

    pub fn lookup(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map_impl.lookup(key)
    }

    pub fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        self.map_impl.update(key, value, flags)
    }

    pub fn delete(&self, key: &[u8]) -> Result<(), MapError> {
        self.map_impl.delete(key)
    }

    pub fn get_next_key(&self, key: Option<&[u8]>) -> Result<MapKey, MapError> {
        self.map_impl.get_next_key(key)
    }

    pub fn vmo(&self) -> Option<Arc<zx::Vmo>> {
        self.map_impl.vmo()
    }

    pub fn can_read(&self) -> Option<bool> {
        self.map_impl.can_read()
    }

    pub fn ringbuf_reserve(&self, size: u32, flags: u64) -> Result<usize, MapError> {
        self.map_impl.ringbuf_reserve(size, flags)
    }
}

type PinnedBuffer = Pin<Box<[u8]>>;

fn new_pinned_buffer(size: usize) -> PinnedBuffer {
    vec![0u8; size].into_boxed_slice().into()
}

fn create_map_impl(schema: &MapSchema) -> Result<Pin<Box<dyn MapImpl>>, MapError> {
    match schema.map_type {
        bpf_map_type_BPF_MAP_TYPE_ARRAY => Ok(Box::pin(array::Array::new(schema)?)),
        bpf_map_type_BPF_MAP_TYPE_HASH => Ok(Box::pin(hashmap::HashMap::new(schema)?)),

        // These types are in use, but not yet implemented. Incorrectly use Array or Hash for
        // these
        bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP_HASH");
            Ok(Box::pin(hashmap::HashMap::new(schema)?))
        }
        bpf_map_type_BPF_MAP_TYPE_LPM_TRIE => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LPM_TRIE");
            Ok(Box::pin(hashmap::HashMap::new(schema)?))
        }
        bpf_map_type_BPF_MAP_TYPE_RINGBUF => Ok(ring_buffer::RingBuffer::new(schema)?),
        bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_ARRAY");
            Ok(Box::pin(array::Array::new(schema)?))
        }
        bpf_map_type_BPF_MAP_TYPE_SK_STORAGE => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SK_STORAGE");
            Ok(Box::pin(array::Array::new(schema)?))
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
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_REUSEPORT_SOCKARRAY");
            Err(MapError::InvalidParam)
        }
        bpf_map_type_BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE");
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

pub fn compute_map_storage_size(schema: &MapSchema) -> Result<usize, MapError> {
    schema.value_size.checked_mul(schema.max_entries).map(|v| v as usize).ok_or(MapError::NoMemory)
}
