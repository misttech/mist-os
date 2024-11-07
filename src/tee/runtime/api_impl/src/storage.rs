// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/360942417): Remove.
#![allow(unused_variables)]

use fuchsia_sync::{Mutex, MutexGuard};
use std::sync::Arc;
use tee_internal::{
    Attribute, AttributeId, BufferOrValue, Error, HandleFlags, MemRef, ObjectEnumHandle,
    ObjectHandle, ObjectInfo, Result as TeeResult, Storage, Type, Usage, ValueFields, Whence,
    OBJECT_ID_MAX_LEN,
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

// A collection of object attributes.
//
// TODO(https://fxbug.dev/360942417): Implement me!
#[derive(Clone, Copy)]
struct AttributeSet {}

impl AttributeSet {
    fn new() -> AttributeSet {
        Self {}
    }

    fn get(&self, id: AttributeId) -> Option<Attribute> {
        None
    }
}

// TODO(https://fxbug.dev/360942417): Implement me!
struct PersistentObject {
    type_: Type,
    usage: Usage,
    attributes: AttributeSet,
}

// A handle's view into a persistent object.
//
// TODO(https://fxbug.dev/360942417):Implement me!
struct PersistentObjectView {
    object: Arc<Mutex<PersistentObject>>,
}

impl PersistentObjectView {
    fn get_info(&self) -> ObjectInfo {
        unimplemented!();
    }

    // See read_object_data().
    fn read_data<'a>(&self, buffer: &mut [u8]) -> TeeResult<&'a [u8]> {
        unimplemented!();
    }

    // See write_object_data().
    fn write_data(&self, data: &[u8]) -> TeeResult {
        unimplemented!();
    }

    // See truncate_object_data().
    fn truncate_data(&self, size: usize) -> TeeResult {
        unimplemented!();
    }

    // See seek_object_data().
    fn seek_data(&self, offset: isize, whence: Whence) -> TeeResult {
        unimplemented!();
    }
}

// A class abstraction implementing the persistent storage interface.
//
// TODO(https://fxbug.dev/360942417): Implement me!
struct PersistentObjects {}

impl PersistentObjects {
    fn new() -> Self {
        Self {}
    }

    fn is_empty(&self) -> bool {
        unimplemented!();
    }

    fn create(
        &self,
        id: &[u8],
        type_: Type,
        usage: Usage,
        flags: HandleFlags,
        attributes: AttributeSet,
        initial_data: &[u8],
    ) -> TeeResult<ObjectHandle> {
        assert!(id.len() <= OBJECT_ID_MAX_LEN);
        unimplemented!();
    }

    // See open_persistent_object().
    fn open(&self, id: &[u8], flags: HandleFlags) -> TeeResult<ObjectHandle> {
        assert!(id.len() <= OBJECT_ID_MAX_LEN);
        unimplemented!();
    }

    fn close(&self, handle: ObjectHandle) {
        unimplemented!();
    }

    // See close_and_delete_persistent_object(). Although unlike that function,
    // this one returns Error::AccessDenied if `handle` was not opened with
    // DATA_ACCESS_WRITE_META.
    fn close_and_delete(&self, handle: ObjectHandle) -> TeeResult {
        unimplemented!();
    }

    // See rename_persistent_object(). Although unlike that function, this one
    // returns Error::AccessDenied if `handle` was not opened with
    // DATA_ACCESS_WRITE_META.
    fn rename(&self, handle: ObjectHandle, new_id: &[u8]) -> TeeResult {
        assert!(new_id.len() <= OBJECT_ID_MAX_LEN);
        unimplemented!();
    }

    // Returns a locked view into the open object corresponding to `handle`,
    // panicking if the handle is invalid.
    fn get(&self, handle: ObjectHandle) -> MutexGuard<'_, PersistentObjectView> {
        unimplemented!();
    }

    // See allocate_persistent_object_enumerator().
    fn allocate_enumerator(&self) -> ObjectEnumHandle {
        ObjectEnumHandle::from_value(0)
    }

    // See free_persistent_object_enumerator().
    fn free_enumerator(&self, enumerator: ObjectEnumHandle) -> () {
        unimplemented!();
    }

    // See reset_persistent_object_enumerator().
    fn reset_enumerator(&self, enumerator: ObjectEnumHandle) -> () {
        unimplemented!();
    }

    // See get_next_persistent_object().
    fn get_next_object<'a>(
        &self,
        enumerator: ObjectEnumHandle,
        id_buffer: &'a mut [u8],
    ) -> TeeResult<(ObjectInfo, &'a [u8])> {
        unimplemented!();
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
    persistent_objects().get(object).get_info()
}

/// Restricts the usage of an open object handle.
///
/// Panics if `object` is not a valid handle.
pub fn restrict_object_usage(object: ObjectHandle, usage: Usage) {
    if is_transient_handle(object) {
        unimplemented!();
    }

    let objects = persistent_objects();
    let view = objects.get(object);
    let mut obj = view.object.lock();
    obj.usage = obj.usage.intersection(usage);
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

    const NOT_FOUND: GetObjectBufferAttributeError =
        GetObjectBufferAttributeError { error: Error::ItemNotFound, actual_size: 0 };

    let attr = if is_transient_handle(object) {
        unimplemented!();
    } else {
        persistent_objects().get(object).object.lock().attributes.get(attribute_id).ok_or(NOT_FOUND)
    }?;

    let bytes = attr.as_memory_reference().as_slice();
    if buffer.len() < bytes.len() {
        Err(GetObjectBufferAttributeError { error: Error::ShortBuffer, actual_size: bytes.len() })
    } else {
        let written = &mut buffer[..bytes.len()];
        written.copy_from_slice(bytes);
        Ok(written)
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

    let attr = if is_transient_handle(object) {
        unimplemented!();
    } else {
        persistent_objects()
            .get(object)
            .object
            .lock()
            .attributes
            .get(attribute_id)
            .ok_or(Error::ItemNotFound)
    }?;
    Ok(attr.as_value().clone())
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

    let (_, _, _) = if is_transient_handle(src) {
        unimplemented!()
    } else {
        let objects = persistent_objects();
        let view = objects.get(src);
        let obj = view.object.lock();
        (obj.type_, obj.usage, obj.attributes)
    };
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
///     and `flags` contains DATA_ACCESS_READ;
///   - The object is currently open with DATA_ACCESS_READ_SHARE, but `flags`
///     contains DATA_ACCESS_READ and not DATA_ACCESS_READ_SHARE;
///   - The object is currently open without DATA_ACCESS_WRITE_SHARE abd
///     `flags` contains DATA_ACCESS_WRITE;
///   - The object is currently open with DATA_ACCESS_WRITE_SHARE, but `flags`
///     contains DATA_ACCESS_WRITE and not DATA_ACCESS_WRITE_SHARE.
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
    let (type_, usage, attrs) = if attribute_src.is_null() {
        (Type::Data, Usage::default(), AttributeSet::new())
    } else if is_persistent_handle(attribute_src) {
        let view = objects.get(attribute_src);
        let obj = view.object.lock();
        (obj.type_, obj.usage, obj.attributes)
    } else {
        unimplemented!();
    };
    objects.create(id, type_, usage, flags, attrs, initial_data)
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
    if storage == Storage::Private && !persistent_objects().is_empty() {
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
    persistent_objects().get(object).read_data(buffer)
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
    persistent_objects().get(object).write_data(buffer)
}

/// Truncates or zero-extends the object's data stream to provided size.
/// This does not affect any handle's data position.
///
/// Returns Error::Overflow if `size` is larger than DATA_MAX_POSITION.
///
/// Panics if `object` is invalid or does not have write access.
pub fn truncate_object_data(object: ObjectHandle, size: usize) -> TeeResult {
    assert!(is_persistent_handle(object));
    persistent_objects().get(object).truncate_data(size)
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
    persistent_objects().get(object).seek_data(offset, whence)
}
