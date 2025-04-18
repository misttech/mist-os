// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

mod array;
mod buffer;
mod hashmap;
mod lock;
mod ring_buffer;
mod vmar;

pub use ring_buffer::{RingBuffer, RingBufferWakeupPolicy, RINGBUF_SIGNAL};

use ebpf::{BpfValue, EbpfBufferPtr, MapReference, MapSchema};
use fidl_fuchsia_ebpf as febpf;
use inspect_stubs::track_stub;
use linux_uapi::{
    bpf_map_type, bpf_map_type_BPF_MAP_TYPE_ARRAY, bpf_map_type_BPF_MAP_TYPE_ARRAY_OF_MAPS,
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
    bpf_map_type_BPF_MAP_TYPE_XSKMAP, BPF_EXIST, BPF_NOEXIST,
};
use std::fmt::Debug;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use zx::HandleBased;

/// Counter for map identifiers.
static MAP_IDS: AtomicU32 = AtomicU32::new(1);
fn new_map_id() -> u32 {
    MAP_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Eq, PartialEq)]
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

    // Invalid VMO was passed for a shared map.
    InvalidVmo,

    // An internal issue, e.g. failed to allocate VMO.
    Internal,
}

pub trait MapImpl: Send + Sync + Debug {
    fn lookup<'a>(&'a self, key: &[u8]) -> Option<MapValueRef<'a>>;
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
        let map_impl = create_map_impl(&schema, None)?;
        Ok(PinnedMap(Arc::pin(Self { id: new_map_id(), schema, flags, map_impl })))
    }

    pub fn new_shared(shared: febpf::Map) -> Result<PinnedMap, MapError> {
        let febpf::Map { schema: Some(fidl_schema), vmo: Some(vmo), .. } = shared else {
            return Err(MapError::InvalidParam);
        };

        let schema = MapSchema {
            map_type: fidl_map_type_to_bpf_map_type(fidl_schema.type_),
            key_size: fidl_schema.key_size,
            value_size: fidl_schema.value_size,
            max_entries: fidl_schema.max_entries,
        };

        let map_impl = create_map_impl(&schema, Some(vmo))?;
        Ok(PinnedMap(Arc::pin(Self { id: new_map_id(), schema, flags: 0, map_impl })))
    }

    pub fn share(&self) -> Result<febpf::Map, MapError> {
        let mut result = febpf::Map::default();
        result.schema = Some(febpf::MapSchema {
            type_: bpf_map_type_to_fidl_map_type(self.schema.map_type),
            key_size: self.schema.key_size,
            value_size: self.schema.value_size,
            max_entries: self.schema.max_entries,
        });
        result.vmo = Some(
            self.map_impl
                .vmo()
                .and_then(|vmo| (*vmo).duplicate_handle(zx::Rights::SAME_RIGHTS).ok())
                .ok_or(MapError::Internal)?,
        );
        Ok(result)
    }

    pub fn lookup<'a>(&'a self, key: &[u8]) -> Option<MapValueRef<'a>> {
        self.map_impl.lookup(key)
    }

    pub fn load(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut result = self.lookup(key)?.ptr().load();

        // Remove padding if any.
        result.resize(self.schema.value_size as usize, 0);

        Some(result)
    }

    pub fn update(&self, key: MapKey, value: &[u8], flags: u64) -> Result<(), MapError> {
        if flags & (BPF_EXIST as u64) > 0 && flags & (BPF_NOEXIST as u64) > 0 {
            return Err(MapError::InvalidParam);
        }

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

pub enum MapValueRef<'a> {
    PlainRef(EbpfBufferPtr<'a>),
    HashMapRef(hashmap::HashMapEntryRef<'a>),
}

impl<'a> MapValueRef<'a> {
    fn new(buf: EbpfBufferPtr<'a>) -> Self {
        Self::PlainRef(buf)
    }

    fn new_from_hashmap(hash_map_ref: hashmap::HashMapEntryRef<'a>) -> Self {
        Self::HashMapRef(hash_map_ref)
    }

    pub fn is_ref_counted(&self) -> bool {
        matches!(&self, MapValueRef::HashMapRef(_))
    }

    pub fn ptr(&self) -> EbpfBufferPtr<'a> {
        match self {
            MapValueRef::PlainRef(buf) => *buf,
            MapValueRef::HashMapRef(hash_map_ref) => hash_map_ref.ptr(),
        }
    }
}

fn create_map_impl(
    schema: &MapSchema,
    vmo: Option<zx::Vmo>,
) -> Result<Pin<Box<dyn MapImpl>>, MapError> {
    match schema.map_type {
        bpf_map_type_BPF_MAP_TYPE_ARRAY => Ok(Box::pin(array::Array::new(schema, vmo)?)),
        bpf_map_type_BPF_MAP_TYPE_HASH => Ok(Box::pin(hashmap::HashMap::new(schema, vmo)?)),
        bpf_map_type_BPF_MAP_TYPE_RINGBUF => Ok(ring_buffer::RingBuffer::new(schema, vmo)?),

        // These types are in use, but not yet implemented. Incorrectly use Array or Hash for
        // these
        bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_DEVMAP_HASH");
            Ok(Box::pin(hashmap::HashMap::new(schema, vmo)?))
        }
        bpf_map_type_BPF_MAP_TYPE_LPM_TRIE => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_LPM_TRIE");
            Ok(Box::pin(hashmap::HashMap::new(schema, vmo)?))
        }
        bpf_map_type_BPF_MAP_TYPE_PERCPU_HASH => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_HASH");
            Ok(Box::pin(hashmap::HashMap::new(schema, vmo)?))
        }
        bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_PERCPU_ARRAY");
            Ok(Box::pin(array::Array::new(schema, vmo)?))
        }
        bpf_map_type_BPF_MAP_TYPE_SK_STORAGE => {
            track_stub!(TODO("https://fxbug.dev/323847465"), "BPF_MAP_TYPE_SK_STORAGE");
            Ok(Box::pin(array::Array::new(&MapSchema { max_entries: 1, ..*schema }, vmo)?))
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

fn bpf_map_type_to_fidl_map_type(map_type: bpf_map_type) -> febpf::MapType {
    match map_type {
        bpf_map_type_BPF_MAP_TYPE_ARRAY => febpf::MapType::Array,
        bpf_map_type_BPF_MAP_TYPE_HASH => febpf::MapType::HashMap,
        bpf_map_type_BPF_MAP_TYPE_RINGBUF => febpf::MapType::RingBuffer,
        bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY => febpf::MapType::PercpuArray,
        bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH => febpf::MapType::DevmapHash,
        bpf_map_type_BPF_MAP_TYPE_LPM_TRIE => febpf::MapType::LpmTrie,
        _ =>
        // Other map types are rejected in `create_map_impl()`.
        {
            unreachable!("unsupported map type")
        }
    }
}

fn fidl_map_type_to_bpf_map_type(map_type: febpf::MapType) -> bpf_map_type {
    match map_type {
        febpf::MapType::Array => bpf_map_type_BPF_MAP_TYPE_ARRAY,
        febpf::MapType::HashMap => bpf_map_type_BPF_MAP_TYPE_HASH,
        febpf::MapType::RingBuffer => bpf_map_type_BPF_MAP_TYPE_RINGBUF,
        febpf::MapType::PercpuArray => bpf_map_type_BPF_MAP_TYPE_PERCPU_ARRAY,
        febpf::MapType::DevmapHash => bpf_map_type_BPF_MAP_TYPE_DEVMAP_HASH,
        febpf::MapType::LpmTrie => bpf_map_type_BPF_MAP_TYPE_LPM_TRIE,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn test_sharing_array() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_ARRAY,
            key_size: 4,
            value_size: 4,
            max_entries: 10,
        };

        // Create two array maps sharing the content.
        let map1 = Map::new(schema, 0).unwrap();
        let map2 = Map::new_shared(map1.share().unwrap()).unwrap();

        // Set a value in one map and check that it's updated in the other.
        let key = vec![0, 0, 0, 0];
        let value = [0, 1, 2, 3];
        map1.update(MapKey::from_vec(key.clone()), &value, 0).unwrap();
        assert_eq!(&map2.load(&key).unwrap(), &value);
    }

    #[fuchsia::test]
    fn test_sharing_hash_map() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_HASH,
            key_size: 4,
            value_size: 4,
            max_entries: 10,
        };

        // Create two array maps sharing the content.
        let map1 = Map::new(schema, 0).unwrap();
        let map2 = Map::new_shared(map1.share().unwrap()).unwrap();

        // Set a value in one map and check that it's updated in the other.
        let key = vec![0, 0, 0, 0];
        let value = [0, 1, 2, 3];
        map1.update(MapKey::from_vec(key.clone()), &value, 0).unwrap();
        assert_eq!(&map2.load(&key).unwrap(), &value);
    }

    #[fuchsia::test]
    fn test_hash_map() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_HASH,
            key_size: 5,
            value_size: 25,
            max_entries: 10000,
        };

        let get_key = |i| {
            MapKey::from_vec(vec![
                (i & 0xffusize) as u8,
                0,
                ((i >> 4) & 0xffusize) as u8,
                0,
                ((i >> 8) & 0xffusize) as u8,
            ])
        };
        let get_value = |i, v| format!("--{:010} {:010}--", i, v).into_bytes();

        let map = Map::new(schema, 0).unwrap();

        for i in 0..10000 {
            assert!(map.update(get_key(i), &get_value(i, 0), 0).is_ok());
        }

        // Should fail to add another entry when the map is full.
        assert_eq!(map.update(get_key(10001), &get_value(10001, 1), 0), Err(MapError::SizeLimit));

        for i in 0..10000 {
            assert_eq!(map.load(&get_key(i)), Some(get_value(i, 0)));
        }

        // Update some elements.
        for i in 8000..9000 {
            assert!(map.update(get_key(i), &get_value(i, 1), 0).is_ok());
        }
        for i in 8000..9000 {
            assert_eq!(map.load(&get_key(i)), Some(get_value(i, 1)));
        }

        // Delete half of the entries.
        for i in 5000..10000 {
            assert!(map.delete(&get_key(i)).is_ok());
        }
        for i in 5000..10000 {
            assert_eq!(map.load(&get_key(i)), None);
        }

        // Replace removed entries with new ones
        for i in 10000..15000 {
            assert!(map.update(get_key(i), &get_value(i, 2), 0).is_ok());
        }

        for i in 0..5000 {
            assert_eq!(map.load(&get_key(i)), Some(get_value(i, 0)));
        }
        for i in 10000..15000 {
            assert_eq!(map.load(&get_key(i)), Some(get_value(i, 2)));
        }
    }

    #[fuchsia::test]
    fn test_hash_map_update_direct() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_HASH,
            key_size: 5,
            value_size: 11,
            max_entries: 10,
        };

        let map = Map::new(schema, 0).unwrap();
        let key = MapKey::from_vec("12345".to_string().into_bytes());
        let value = (0..11).collect::<Vec<u8>>();
        assert!(map.update(key.clone(), &value, 0).is_ok());

        // Access a value directly the way eBPF programs do.
        let value_ref = map.lookup(&key).unwrap();
        unsafe {
            *value_ref.ptr().get_ptr::<u32>(0).unwrap().deref_mut() = 0xabacadae;
        }

        assert_eq!(map.load(&key), Some(vec![0xae, 0xad, 0xac, 0xab, 4, 5, 6, 7, 8, 9, 10]));
    }

    #[fuchsia::test]
    fn test_hash_map_ref_counting() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_HASH,
            key_size: 5,
            value_size: 11,
            max_entries: 2,
        };

        let map = Map::new(schema, 0).unwrap();
        let key = MapKey::from_vec("12345".to_string().into_bytes());
        let key2 = MapKey::from_vec("24122".to_string().into_bytes());
        let value = (0..11).collect::<Vec<u8>>();
        assert!(map.update(key.clone(), &value, 0).is_ok());
        assert!(map.update(key2.clone(), &value, 0).is_ok());

        let value_ref = map.lookup(&key).unwrap();

        // Delete an element. The corresponding data entry should not be
        // released until `value_ref` is dropped.
        assert!(map.delete(&key).is_ok());
        assert_eq!(map.update(key.clone(), &value, 0), Err(MapError::SizeLimit));
        drop(value_ref);
        assert!(map.update(key.clone(), &value, 0).is_ok());
    }

    #[fuchsia::test]
    fn test_ringbug_sharing() {
        let schema = MapSchema {
            map_type: bpf_map_type_BPF_MAP_TYPE_RINGBUF,
            key_size: 0,
            value_size: 0,
            max_entries: 4096 * 2,
        };

        let map = Map::new(schema, 0).unwrap();
        map.ringbuf_reserve(8000, 0).expect("ringbuf_reserve failed");

        let map2 = Map::new_shared(map.share().unwrap()).unwrap();

        // Expected to fail since there is no space left.
        map2.ringbuf_reserve(2000, 0).expect_err("ringbuf_reserve expected to fail");
    }
}
