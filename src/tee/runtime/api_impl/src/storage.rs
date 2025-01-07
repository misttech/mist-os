// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/360942417): Remove.
#![allow(unused_variables)]

use fuchsia_sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::cmp::min;
use std::collections::btree_map::Entry as BTreeMapEntry;
use std::collections::hash_map::Entry as HashMapEntry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tee_internal::{
    Attribute, AttributeId, BufferOrValue, Error, HandleFlags, MemRef, ObjectEnumHandle,
    ObjectHandle, ObjectInfo, Result as TeeResult, Storage, Type, Usage, ValueFields, Whence,
    DATA_MAX_POSITION, OBJECT_ID_MAX_LEN,
};

static PERSISTENT_OBJECTS: Mutex<Option<Arc<PersistentObjects>>> = Mutex::new(None);

pub(crate) fn on_entrypoint_creation() {
    let mut objects = PERSISTENT_OBJECTS.lock();
    if objects.is_none() {
        *objects = Some(Arc::new(PersistentObjects::new()));
    }
}

pub(crate) fn on_entrypoint_destruction() {
    // TODO(https://fxbug.dev/360942417): Object-related teardown (e.g., transient object
    // destruction).
}

fn persistent_objects() -> Arc<PersistentObjects> {
    PERSISTENT_OBJECTS
        .lock()
        .as_ref()
        .expect("on_entrypoint_creation() should have been called")
        .clone()
}

//
// We establish the private convention that all persistent object handles are
// odd in value, while all transient object handles are even.
//

fn is_persistent_handle(object: ObjectHandle) -> bool {
    *object % 2 == 1
}

// We define the inverse of is_persistent_handle() for readability at callsites
// where we want to more directly check for transience.
fn is_transient_handle(object: ObjectHandle) -> bool {
    !is_persistent_handle(object)
}

// The "key" type that carries no information.
#[derive(Clone)]
struct NoKey {}

// Represents supported key types, in principle parameterized by
// tee_internal::Type.
//
// TODO(https://fxbug.dev/360942417): More entries and properly implement me!
#[derive(Clone)]
enum Key {
    Data(NoKey),
}

impl Key {
    fn get_type(&self) -> Type {
        match self {
            Self::Data(_) => Type::Data,
        }
    }

    fn size(&self) -> u32 {
        0
    }

    fn max_size(&self) -> u32 {
        0
    }

    fn buffer_attribute(&self, _id: AttributeId) -> Option<&Vec<u8>> {
        None
    }

    fn value_attribute(&self, _id: AttributeId) -> Option<&ValueFields> {
        None
    }
}

// The common object abstraction implemented by transient and persistent
// storage objects.
trait Object {
    fn key(&self) -> &Key;

    fn usage(&self) -> &Usage;
    fn usage_mut(&mut self) -> &mut Usage;

    fn flags(&self) -> &HandleFlags;

    fn restrict_usage(&mut self, restriction: Usage) {
        let usage = self.usage_mut();
        *usage = usage.intersection(restriction)
    }

    fn get_info(&self, data_size: usize, data_position: usize) -> ObjectInfo {
        let all_info_flags = HandleFlags::PERSISTENT
            | HandleFlags::INITIALIZED
            | HandleFlags::DATA_ACCESS_READ
            | HandleFlags::DATA_ACCESS_WRITE
            | HandleFlags::DATA_ACCESS_WRITE_META
            | HandleFlags::DATA_SHARE_READ
            | HandleFlags::DATA_SHARE_WRITE;
        let flags = self.flags().intersection(all_info_flags);
        let key_size = self.key().size();
        let object_size = if key_size > 0 { key_size } else { data_size.try_into().unwrap() };
        ObjectInfo {
            object_type: self.key().get_type(),
            max_object_size: self.key().max_size(),
            object_size,
            object_usage: *self.usage(),
            data_position: data_position,
            data_size: data_size,
            handle_flags: flags,
        }
    }
}

struct PersistentObject {
    key: Key,
    usage: Usage,
    base_flags: HandleFlags,
    data: zx::Vmo,
    data_size: usize,
    id: Vec<u8>,

    // The open handles to this object. Tracking these in this way conveniently
    // enables their invalidation in the case of object overwriting.
    handles: HashSet<ObjectHandle>,
}

impl Object for PersistentObject {
    fn key(&self) -> &Key {
        &self.key
    }

    fn usage(&self) -> &Usage {
        &self.usage
    }
    fn usage_mut(&mut self) -> &mut Usage {
        &mut self.usage
    }

    fn flags(&self) -> &HandleFlags {
        &self.base_flags
    }
}

// A handle's view into a persistent object.
struct PersistentObjectView {
    object: Arc<Mutex<PersistentObject>>,
    flags: HandleFlags,
    data_position: usize,
}

impl PersistentObjectView {
    fn get_info(&self) -> ObjectInfo {
        let obj = self.object.lock();
        obj.get_info(obj.data_size, self.data_position)
    }

    // See read_object_data().
    fn read_data<'a>(&mut self, buffer: &'a mut [u8]) -> TeeResult<&'a [u8]> {
        let obj = self.object.lock();
        let read_size = min(obj.data_size - self.data_position, buffer.len());
        let written = &mut buffer[..read_size];
        if read_size > 0 {
            obj.data.read(written, self.data_position as u64).unwrap();
        }
        self.data_position += read_size;
        Ok(written)
    }

    // See write_object_data().
    fn write_data(&mut self, data: &[u8]) -> TeeResult {
        if data.is_empty() {
            return Ok(());
        }
        let mut obj = self.object.lock();
        let write_end = self.data_position + data.len();

        if write_end > DATA_MAX_POSITION {
            return Err(Error::Overflow);
        }
        if write_end > obj.data_size {
            obj.data.set_size(write_end as u64).unwrap();
            obj.data_size = write_end;
        }
        obj.data.write(data, self.data_position as u64).unwrap();
        self.data_position = write_end;
        Ok(())
    }

    // See truncate_object_data().
    fn truncate_data(&self, size: usize) -> TeeResult {
        let mut obj = self.object.lock();

        // It's okay to set the size past the position in either direction.
        // However, the spec does not actually cover the case where the
        // provided size is is larger than DATA_MAX_POSITION. Since any
        // part of the data stream past that would be inaccessible; it
        // should be sensible and harmless to not exceed that in resizing.
        let size = min(size, DATA_MAX_POSITION);
        obj.data.set_size(size as u64).unwrap();
        obj.data_size = size;
        Ok(())
    }

    // See seek_object_data().
    fn seek_data(&mut self, offset: isize, whence: Whence) -> TeeResult {
        let start = match whence {
            Whence::DataSeekCur => self.data_position,
            Whence::DataSeekEnd => self.object.lock().data_size,
            Whence::DataSeekSet => 0,
        };
        let new_position = start.saturating_add_signed(offset);
        if new_position > DATA_MAX_POSITION {
            Err(Error::Overflow)
        } else {
            self.data_position = new_position;
            Ok(())
        }
    }
}

// The state of an object enum handle.
struct EnumState {
    // None if in the allocated/unstarted state.
    id: Option<Vec<u8>>,
}

// A B-tree since enumeration needs to deal in key (i.e., ID) ordering.
//
// Further, the key represents a separately owned copy of the ID; we do this
// instead of representing the key as an Arc<Vec<u8>> as then we would no
// longer be able to perform look-up with slices - since Borrow is not
// implemented for Arc - and would instead have to dynamically allocate a new
// key for the look-up. Better to not touch the heap when bad inputs are
// provided.
type PersistentIdMap = BTreeMap<Vec<u8>, Arc<Mutex<PersistentObject>>>;

type PersistentHandleMap = HashMap<ObjectHandle, Mutex<PersistentObjectView>>;
type PersistentEnumHandleMap = HashMap<ObjectEnumHandle, Mutex<EnumState>>;

// A class abstraction implementing the persistent storage interface.
struct PersistentObjects {
    by_id: RwLock<PersistentIdMap>,
    by_handle: RwLock<PersistentHandleMap>,
    enum_handles: RwLock<PersistentEnumHandleMap>,
    next_handle_value: AtomicU64,
    next_enum_handle_value: AtomicU64,
}

impl PersistentObjects {
    fn new() -> Self {
        Self {
            by_id: RwLock::new(PersistentIdMap::new()),
            by_handle: RwLock::new(PersistentHandleMap::new()),
            enum_handles: RwLock::new(HashMap::new()),
            next_handle_value: AtomicU64::new(1), // Always odd, per the described convention above
            next_enum_handle_value: AtomicU64::new(1),
        }
    }

    fn create(
        &self,
        key: Key,
        usage: Usage,
        flags: HandleFlags,
        id: &[u8],
        initial_data: &[u8],
    ) -> TeeResult<ObjectHandle> {
        assert!(id.len() <= OBJECT_ID_MAX_LEN);

        let data = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, initial_data.len() as u64)
            .unwrap();
        if !initial_data.is_empty() {
            data.write(initial_data, 0).unwrap();
        }

        let flags = flags.union(HandleFlags::PERSISTENT | HandleFlags::INITIALIZED);

        let obj = PersistentObject {
            key,
            usage,
            base_flags: flags,
            data,
            data_size: initial_data.len(),
            id: Vec::from(id),
            handles: HashSet::new(),
        };

        let (mut by_handle, mut by_id) = self.lock_for_handle_and_id_writes();

        let obj_ref = match by_id.get(id) {
            // If there's already an object with that ID, then
            // DATA_FLAG_OVERWRITE permits overwriting. This results in
            // existing handles being invalidated.
            Some(obj_ref) => {
                if !flags.contains(HandleFlags::DATA_FLAG_OVERWRITE) {
                    return Err(Error::AccessConflict);
                }
                {
                    let mut obj_old = obj_ref.lock();
                    for handle in obj_old.handles.iter() {
                        let removed = by_handle.remove(&handle).is_some();
                        debug_assert!(removed);
                    }
                    *obj_old = obj;
                }
                obj_ref.clone()
            }
            None => {
                let id = obj.id.clone();
                let obj_ref = Arc::new(Mutex::new(obj));
                let inserted = by_id.insert(id, obj_ref.clone());
                debug_assert!(inserted.is_none());
                obj_ref
            }
        };
        Ok(self.open_locked(by_handle, obj_ref, flags))
    }

    // See open_persistent_object().
    fn open(&self, id: &[u8], flags: HandleFlags) -> TeeResult<ObjectHandle> {
        assert!(id.len() <= OBJECT_ID_MAX_LEN);

        let (by_handle, by_id) = self.lock_for_handle_writes_and_id_reads();

        let obj_ref = match by_id.get(id) {
            Some(obj_ref) => Ok(obj_ref),
            None => Err(Error::ItemNotFound),
        }?;

        {
            let mut obj = obj_ref.lock();

            // At any given time, the number of object references should be
            // greater than or equal to the number of handle map values + the
            // number of object ID map values, which should be equal to the #
            // of open handles to that object + 1.
            debug_assert!(Arc::strong_count(obj_ref) >= obj.handles.len() + 1);

            // If we previously closed the last handle to the object and are
            // now reopening its first active handle, overwrite the base flags
            // with the handle's. The spec doesn't dictate this, but it's hard
            // to imagine what else an implementation could or should do in
            // this case.
            if obj.handles.is_empty() {
                obj.base_flags = flags.union(HandleFlags::PERSISTENT | HandleFlags::INITIALIZED);
            } else {
                let combined = flags.union(obj.base_flags);
                let intersection = flags.intersection(obj.base_flags);

                // Check for shared read permissions.
                if flags.contains(HandleFlags::DATA_ACCESS_READ)
                    && !(intersection.contains(HandleFlags::DATA_SHARE_READ))
                {
                    return Err(Error::AccessConflict);
                }

                // Check for shared read permission consistency.
                if combined.contains(HandleFlags::DATA_SHARE_READ)
                    == intersection.contains(HandleFlags::DATA_SHARE_READ)
                {
                    return Err(Error::AccessConflict);
                }

                // Check for shared write permissions.
                if flags.contains(HandleFlags::DATA_ACCESS_WRITE)
                    && !(intersection.contains(HandleFlags::DATA_SHARE_WRITE))
                {
                    return Err(Error::AccessConflict);
                }

                // Check for shared write permission consistency.
                if combined.contains(HandleFlags::DATA_SHARE_WRITE)
                    == intersection.contains(HandleFlags::DATA_SHARE_WRITE)
                {
                    return Err(Error::AccessConflict);
                }
            }
        }

        Ok(self.open_locked(by_handle, obj_ref.clone(), flags))
    }

    // The common handle opening subroutine of create() and open(), which
    // expects the handle map to already be locked for insertion.
    fn open_locked(
        &self,
        mut by_handle: RwLockWriteGuard<'_, PersistentHandleMap>,
        object: Arc<Mutex<PersistentObject>>,
        flags: HandleFlags,
    ) -> ObjectHandle {
        let handle = self.mint_handle();
        let inserted = object.lock().handles.insert(handle);
        debug_assert!(inserted);
        let view = PersistentObjectView { object, flags, data_position: 0 };
        let inserted = by_handle.insert(handle, Mutex::new(view)).is_none();
        debug_assert!(inserted);
        handle
    }

    fn close(&self, handle: ObjectHandle) {
        let mut by_handle = self.by_handle.write();

        // Note that even if all handle map entries associated with the object
        // are removed, the reference to the object in the ID map remains,
        // keeping it alive for future open() calls.
        match by_handle.entry(handle) {
            HashMapEntry::Occupied(entry) => {
                {
                    let view = &entry.get().lock();
                    let mut obj = view.object.lock();
                    let removed = obj.handles.remove(&handle);
                    debug_assert!(removed);
                }
                let _ = entry.remove();
            }
            HashMapEntry::Vacant(_) => panic!("{handle:?} is not a valid handle"),
        }
    }

    // See close_and_delete_persistent_object(). Although unlike that function,
    // this one returns Error::AccessDenied if `handle` was not opened with
    // DATA_ACCESS_WRITE_META.
    fn close_and_delete(&self, handle: ObjectHandle) -> TeeResult {
        let (mut by_handle, mut by_id) = self.lock_for_handle_and_id_writes();
        // With both maps locked, removal of all entries with the associated
        // object handle should amount to dropping that object.
        match by_handle.entry(handle) {
            HashMapEntry::Occupied(entry) => {
                {
                    let state = &entry.get().lock();
                    if !state.flags.contains(HandleFlags::DATA_ACCESS_WRITE_META) {
                        return Err(Error::AccessDenied);
                    }
                    let obj = state.object.lock();
                    debug_assert_eq!(obj.handles.len(), 1);
                    let removed = by_id.remove(&obj.id).is_some();
                    debug_assert!(removed);
                }
                let _ = entry.remove();
                Ok(())
            }
            HashMapEntry::Vacant(_) => panic!("{handle:?} is not a valid handle"),
        }
    }

    // See rename_persistent_object(). Although unlike that function, this one
    // returns Error::AccessDenied if `handle` was not opened with
    // DATA_ACCESS_WRITE_META.
    fn rename(&self, handle: ObjectHandle, new_id: &[u8]) -> TeeResult {
        let (mut by_handle, mut by_id) = self.lock_for_handle_and_id_writes();
        match by_handle.entry(handle) {
            HashMapEntry::Occupied(handle_entry) => {
                let state = handle_entry.get().lock();
                if !state.flags.contains(HandleFlags::DATA_ACCESS_WRITE_META) {
                    return Err(Error::AccessDenied);
                }
                let new_id = Vec::from(new_id);
                match by_id.entry(new_id.clone()) {
                    BTreeMapEntry::Occupied(_) => return Err(Error::AccessConflict),
                    BTreeMapEntry::Vacant(id_entry) => {
                        let _ = id_entry.insert(state.object.clone());
                    }
                };
                let mut obj = state.object.lock();
                let removed = by_id.remove(&obj.id);
                debug_assert!(removed.is_some());
                obj.id = new_id;
                Ok(())
            }
            HashMapEntry::Vacant(_) => panic!("{handle:?} is not a valid handle"),
        }
    }

    // Given a handle, passes the associated object view into a provided
    // callback, panicking if the handle is invalid.
    fn operate<F, R>(&self, handle: ObjectHandle, callback: F) -> R
    where
        F: FnOnce(&mut PersistentObjectView) -> R,
    {
        let by_handle = self.by_handle.read();
        let view =
            by_handle.get(&handle).unwrap_or_else(|| panic!("{handle:?} is not a valid handle"));
        let mut view = view.lock();
        callback(&mut view)
    }

    // See allocate_persistent_object_enumerator().
    fn allocate_enumerator(&self) -> ObjectEnumHandle {
        let enumerator = self.mint_enumerator_handle();

        let previous = self
            .enum_handles
            .write()
            .insert(enumerator.clone(), Mutex::new(EnumState { id: None }));
        debug_assert!(previous.is_none());
        enumerator
    }

    // See free_persistent_object_enumerator().
    fn free_enumerator(&self, enumerator: ObjectEnumHandle) -> () {
        let mut enum_handles = self.enum_handles.write();
        match enum_handles.entry(enumerator) {
            HashMapEntry::Occupied(entry) => {
                let _ = entry.remove();
            }
            HashMapEntry::Vacant(_) => panic!("{enumerator:?} is not a valid enumerator handle"),
        }
    }

    // See reset_persistent_object_enumerator().
    fn reset_enumerator(&self, enumerator: ObjectEnumHandle) -> () {
        let enum_handles = self.enum_handles.read();
        match enum_handles.get(&enumerator) {
            Some(state) => {
                state.lock().id = None;
            }
            None => panic!("{enumerator:?} is not a valid enumerator handle"),
        }
    }

    // See get_next_persistent_object().
    fn get_next_object<'a>(
        &self,
        enumerator: ObjectEnumHandle,
        id_buffer: &'a mut [u8],
    ) -> TeeResult<(ObjectInfo, &'a [u8])> {
        let enum_handles = self.enum_handles.read();
        match enum_handles.get(&enumerator) {
            Some(state) => {
                let by_id = self.by_id.read();
                let mut state = state.lock();
                let next = if state.id.is_none() {
                    by_id.first_key_value()
                } else {
                    // Since we're dealing with an ID-keyed B-tree, we can
                    // straightforwardly get the first entry with an ID larger
                    // than the current.
                    let curr_id = state.id.as_ref().unwrap();
                    by_id.range((Bound::Excluded(curr_id.clone()), Bound::Unbounded)).next()
                };
                if let Some((id, obj)) = next {
                    assert!(id_buffer.len() >= id.len());
                    let written = &mut id_buffer[..id.len()];
                    written.copy_from_slice(id);
                    state.id = Some(id.clone());
                    Ok((obj.lock().get_info(/*data_size=*/ 0, /*data_position=*/ 0), written))
                } else {
                    Err(Error::ItemNotFound)
                }
            }
            None => panic!("{enumerator:?} is not a valid enumerator handle"),
        }
    }

    fn mint_handle(&self) -> ObjectHandle {
        // Per the described convention above, always odd. (Initial value is 1.)
        ObjectHandle::from_value(self.next_handle_value.fetch_add(2, Ordering::Relaxed))
    }

    fn mint_enumerator_handle(&self) -> ObjectEnumHandle {
        ObjectEnumHandle::from_value(self.next_enum_handle_value.fetch_add(1, Ordering::Relaxed))
    }

    //
    // To avoid deadlock, the following two methods establish a locking
    // discipline to be followed when needing to lock both the handle and ID
    // maps: always have ID map locking follow handle map locking. Mixing that
    // ordering with the inverse would cause deadlock, and gaining exclusive
    // access to global handle state before object consultation by ID seems
    // better hygienically.
    //

    fn lock_for_handle_and_id_writes(
        &self,
    ) -> (RwLockWriteGuard<'_, PersistentHandleMap>, RwLockWriteGuard<'_, PersistentIdMap>) {
        let by_handle = self.by_handle.write();
        let by_id = self.by_id.write();
        (by_handle, by_id)
    }

    fn lock_for_handle_writes_and_id_reads(
        &self,
    ) -> (RwLockWriteGuard<'_, PersistentHandleMap>, RwLockReadGuard<'_, PersistentIdMap>) {
        let by_handle = self.by_handle.write();
        let by_id = self.by_id.read();
        (by_handle, by_id)
    }
}

//
// Implementation
//

/// Returns info about an open object as well of the state of its handle.
///
/// Panics if `object` is not a valid handle.
pub fn get_object_info(object: ObjectHandle) -> ObjectInfo {
    if is_transient_handle(object) {
        unimplemented!();
    }
    persistent_objects().operate(object, |view| view.get_info())
}

/// Restricts the usage of an open object handle.
///
/// Panics if `object` is not a valid handle.
pub fn restrict_object_usage(object: ObjectHandle, usage: Usage) {
    if is_transient_handle(object) {
        unimplemented!();
    }

    persistent_objects().operate(object, |view| view.object.lock().restrict_usage(usage))
}

/// Encapsulates an error of get_object_buffer_attribute(), which includes the
/// actual length of the desired buffer attribute in the case where the
/// caller-provided was too small.
pub struct GetObjectBufferAttributeError {
    pub error: Error,
    pub actual_size: usize,
}

/// Returns the requested buffer-type attribute associated with the given
/// object, if any. It is written to the provided buffer and it is this
/// written subslice that is returned.
///
/// Returns a wrapped value of Error::ItemNotFound if the object does not have
/// such an attribute.
///
/// Returns a wrapped value of Error::ShortBuffer if the buffer was too small
/// to read the attribute value into, along with the length of the attribute.
///
/// Panics if `object` is not a valid handle or if `attribute_id` is not of
/// buffer type.
pub fn get_object_buffer_attribute<'a>(
    object: ObjectHandle,
    attribute_id: AttributeId,
    buffer: &'a mut [u8],
) -> Result<&'a [u8], GetObjectBufferAttributeError> {
    assert!(!attribute_id.value());

    let copy_from_key =
        |key: &Key, buffer: &'a mut [u8]| -> Result<&'a [u8], GetObjectBufferAttributeError> {
            if let Some(bytes) = key.buffer_attribute(attribute_id) {
                if buffer.len() < bytes.len() {
                    Err(GetObjectBufferAttributeError {
                        error: Error::ShortBuffer,
                        actual_size: bytes.len(),
                    })
                } else {
                    let written = &mut buffer[..bytes.len()];
                    written.copy_from_slice(bytes);
                    Ok(written)
                }
            } else {
                Err(GetObjectBufferAttributeError { error: Error::ItemNotFound, actual_size: 0 })
            }
        };

    if is_transient_handle(object) {
        unimplemented!();
    } else {
        persistent_objects().operate(object, |view| copy_from_key(&view.object.lock().key, buffer))
    }
}

/// Returns the requested value-type attribute associated with the given
/// object, if any.
///
/// Returns Error::ItemNotFound if the object does not have such an attribute.
///
/// Panics if `object` is not a valid handle or if `attribute_id` is not of
/// value type.
pub fn get_object_value_attribute(
    object: ObjectHandle,
    attribute_id: AttributeId,
) -> TeeResult<ValueFields> {
    assert!(!attribute_id.value());

    let copy_from_key = |key: &Key| {
        if let Some(value) = key.value_attribute(attribute_id) {
            Ok(value.clone())
        } else {
            Err(Error::ItemNotFound)
        }
    };

    if is_transient_handle(object) {
        unimplemented!();
    } else {
        persistent_objects().operate(object, |view| copy_from_key(&view.object.lock().key))
    }
}

/// Closes the given object handle.
///
/// Panics if `object` is neither null or a valid handle.
pub fn close_object(object: ObjectHandle) {
    if object.is_null() {
        return;
    }

    if is_transient_handle(object) {
        unimplemented!();
    }
    persistent_objects().close(object)
}

pub fn allocate_transient_object(object_type: Type, max_size: u32) -> TeeResult<ObjectHandle> {
    unimplemented!()
}

pub fn free_transient_object(object: ObjectHandle) {
    assert!(is_transient_handle(object));
    unimplemented!()
}

pub fn reset_transient_object(object: ObjectHandle) {
    assert!(is_transient_handle(object));
    unimplemented!()
}

pub fn populate_transient_object(object: ObjectHandle, attrs: &[Attribute]) -> TeeResult {
    assert!(is_transient_handle(object));
    unimplemented!()
}

pub fn init_ref_attribute(id: AttributeId, buffer: &mut [u8]) -> Attribute {
    assert!(id.memory_reference(), "Attribute ID {id:?} does not represent a memory reference");
    Attribute { id, content: BufferOrValue { memref: MemRef::from_mut_slice(buffer) } }
}

pub fn init_value_attribute(id: AttributeId, value: ValueFields) -> Attribute {
    assert!(id.value(), "Attribute ID {id:?} does not represent value fields");
    Attribute { id, content: BufferOrValue { value } }
}

pub fn copy_object_attributes(src: ObjectHandle, dest: ObjectHandle) -> TeeResult {
    assert!(is_transient_handle(dest));
    unimplemented!()
}

pub fn generate_key(object: ObjectHandle, key_size: u32, params: &[Attribute]) -> TeeResult {
    unimplemented!()
}

/// Opens a new handle to an existing persistent object.
///
/// Returns Error::ItemNotFound: if `storage` does not correspond to a valid
/// storage space, or if no object with `id` is found.
///
/// Returns Error::AccessConflict if any of the following hold:
///   - The object is currently open with DATA_ACCESS_WRITE_META;
///   - The object is currently open and `flags` contains
///     DATA_ACCESS_WRITE_META
///   - The object is currently open without DATA_ACCESS_READ_SHARE
///     and `flags` contains DATA_ACCESS_READ or DATA_ACCESS_READ_SHARE;
///   - The object is currently open with DATA_ACCESS_READ_SHARE, but `flags`
///     does not;
///   - The object is currently open without DATA_ACCESS_WRITE_SHARE and
///     `flags` contains DATA_ACCESS_WRITE or DATA_ACCESS_WRITE_SHARE;
///   - The object is currently open with DATA_ACCESS_WRITE_SHARE, but `flags`
///     does not.
pub fn open_persistent_object(
    storage: Storage,
    id: &[u8],
    flags: HandleFlags,
) -> TeeResult<ObjectHandle> {
    if storage == Storage::Private {
        persistent_objects().open(id, flags)
    } else {
        Err(Error::ItemNotFound)
    }
}

/// Creates a persistent object and returns a handle to it. The conferred type,
/// usage, and attributes are given indirectly by `attribute_src`; if
/// `attribute_src` is null then the conferred type is Data.
///
/// Returns Error::ItemNotFound: if `storage` does not correspond to a valid
/// storage spac
///
/// Returns Error::AccessConflict if the provided ID already exists but
/// `flags` does not contain DATA_FLAG_OVERWRITE.
pub fn create_persistent_object(
    storage: Storage,
    id: &[u8],
    flags: HandleFlags,
    attribute_src: ObjectHandle,
    initial_data: &[u8],
) -> TeeResult<ObjectHandle> {
    if storage != Storage::Private {
        return Err(Error::ItemNotFound);
    }

    let objects = persistent_objects();
    let (key, usage, base_flags) = if attribute_src.is_null() {
        (Key::Data(NoKey {}), Usage::default(), HandleFlags::empty())
    } else if is_persistent_handle(attribute_src) {
        persistent_objects().operate(attribute_src, |view| {
            let obj = view.object.lock();
            (obj.key.clone(), obj.usage, obj.base_flags)
        })
    } else {
        unimplemented!();
    };
    let flags = base_flags.union(flags);
    objects.create(key, usage, flags, id, initial_data)
}

/// Closes the given handle to a persistent object and deletes the object.
///
/// Panics if `object` is invalid or was not opened with
/// DATA_ACCESS_WRITE_META.
pub fn close_and_delete_persistent_object(object: ObjectHandle) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().close_and_delete(object)
}

/// Renames the object's, associating it with a new identifier.
///
/// Returns Error::AccessConflict if `new_id` is the ID of an existing
/// object.
///
/// Panics if `object` is invalid or was not opened with
/// DATA_ACCESS_WRITE_META.
pub fn rename_persistent_object(object: ObjectHandle, new_id: &[u8]) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().rename(object, new_id)
}

/// Allocates a new object enumerator and returns a handle to it.
pub fn allocate_persistent_object_enumerator() -> ObjectEnumHandle {
    persistent_objects().allocate_enumerator()
}

/// Deallocates an object enumerator.
///
/// Panics if `enumerator` is not a valid handle.
pub fn free_persistent_object_enumerator(enumerator: ObjectEnumHandle) {
    persistent_objects().free_enumerator(enumerator)
}

/// Resets an object enumerator.
///
/// Panics if `enumerator` is not a valid handle.
pub fn reset_persistent_object_enumerator(enumerator: ObjectEnumHandle) {
    persistent_objects().reset_enumerator(enumerator)
}

/// Starts an object enumerator's enumeration, or resets it if already started.
///
/// Returns Error::ItemNotFound if `storage` is unsupported or it there are no
/// objects yet created in that storage space.
///
/// Panics if `enumerator` is not a valid handle.
pub fn start_persistent_object_enumerator(
    enumerator: ObjectEnumHandle,
    storage: Storage,
) -> TeeResult {
    if storage == Storage::Private {
        reset_persistent_object_enumerator(enumerator);
        Ok(())
    } else {
        Err(Error::ItemNotFound)
    }
}

/// Returns the info and ID associated with the next object in the enumeration,
/// advancing it in the process. The returns object ID is backed by the
/// provided buffer.
///
/// Returns Error::ItemNotFound if there are no more objects left to enumerate.
///
/// Panics if `enumerator` is not a valid handle.
pub fn get_next_persistent_object<'a>(
    enumerator: ObjectEnumHandle,
    id_buffer: &'a mut [u8],
) -> TeeResult<(ObjectInfo, &'a [u8])> {
    persistent_objects().get_next_object(enumerator, id_buffer)
}

/// Tries to read as much of the object's data stream from the handle's current
/// data position as can fill the provided buffer.
///
/// Panics if `object` is invalid or does not have read access.
pub fn read_object_data<'a>(object: ObjectHandle, buffer: &'a mut [u8]) -> TeeResult<&'a [u8]> {
    assert!(is_persistent_handle(object));
    persistent_objects().operate(object, |view| view.read_data(buffer))
}

/// Writes the provided data to the object's data stream at the handle's
/// data position, advancing that position to the end of the written data.
///
/// Returns Error::AccessConflict if the object does not have write
/// access.
///
/// Returns Error::Overflow if writing the data would advance the data
/// position past DATA_MAX_POSITION.
///
/// Panics if `object` is invalid or does not have write access.
pub fn write_object_data(object: ObjectHandle, buffer: &[u8]) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().operate(object, |view| view.write_data(buffer))
}

/// Truncates or zero-extends the object's data stream to provided size.
/// This does not affect any handle's data position.
///
/// Returns Error::Overflow if `size` is larger than DATA_MAX_POSITION.
///
/// Panics if `object` is invalid or does not have write access.
pub fn truncate_object_data(object: ObjectHandle, size: usize) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().operate(object, |view| view.truncate_data(size))
}

/// Updates the handle's data positition, seeking at an offset from a
/// position given by a whence value. The new position saturates at 0.
///
/// Returns Error::Overflow if the would-be position exceeds
/// DATA_MAX_POSITION.
///
/// Panics if `object` is invalid.
pub fn seek_data_object(object: ObjectHandle, offset: isize, whence: Whence) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().operate(object, |view| view.seek_data(offset, whence))
}

// TODO(https://fxbug.dev/376093162): Add PersistentObjects testing.
