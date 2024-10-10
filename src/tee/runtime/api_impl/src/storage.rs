// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/360942417): Remove.
#![allow(unused_variables)]

// TODO(https://fxbug.dev/360942417): Remove.
use std::unimplemented;

use tee_internal::{
    Attribute, AttributeId, HandleFlags, ObjectEnumHandle, ObjectHandle, ObjectInfo,
    Result as TeeResult, Storage, Type, Usage, ValueFields, Whence,
};

pub(crate) fn on_entrypoint_creation() {
    // TODO(https://fxbug.dev/360942417): Object-related setup (e.g., persistent object protocol
    // connection).
}

pub(crate) fn on_entrypoint_destruction() {
    // TODO(https://fxbug.dev/360942417): Object-related teardown (e.g., transient object
    // destruction).
}

pub fn get_object_handle1(object: ObjectHandle) -> TeeResult<ObjectInfo> {
    unimplemented!()
}

pub fn restrict_object_usage1(object: ObjectHandle, usage: Usage) -> TeeResult {
    unimplemented!()
}

pub fn get_object_buffer_attribute(
    object: ObjectHandle,
    attribute_id: AttributeId,
    buffer: &mut [u8],
) -> TeeResult {
    unimplemented!()
}

pub fn get_object_value_attribute(
    object: ObjectHandle,
    attribute_id: AttributeId,
) -> TeeResult<ValueFields> {
    unimplemented!()
}

pub fn close_object(object: ObjectHandle) {
    unimplemented!()
}

pub fn allocate_transient_object(object_type: Type, max_size: u32) -> TeeResult<ObjectHandle> {
    unimplemented!()
}

pub fn free_transient_object(object: ObjectHandle) {
    unimplemented!()
}

pub fn reset_transient_object(object: ObjectHandle) {
    unimplemented!()
}

pub fn populate_transient_object(object: ObjectHandle, attrs: &[Attribute]) -> TeeResult {
    unimplemented!()
}

pub fn init_ref_attribute(attribute_id: AttributeId, buffer: &[u8]) -> Attribute {
    unimplemented!()
}

pub fn init_value_attribute(attribute_id: AttributeId, value: ValueFields) -> Attribute {
    unimplemented!()
}

pub fn copy_object_attributes1(src: ObjectHandle, dest: ObjectHandle) -> TeeResult {
    unimplemented!()
}

pub fn generate_key(object: ObjectHandle, key_size: u32, params: &[Attribute]) -> TeeResult {
    unimplemented!()
}

pub fn open_persistent_object(
    storage: Storage,
    id: &[u8],
    flags: HandleFlags,
) -> TeeResult<ObjectHandle> {
    unimplemented!()
}

pub fn create_persistent_object(
    storage: Storage,
    id: &[u8],
    flags: HandleFlags,
    attributes: ObjectHandle,
    initial_data: &[u8],
) -> TeeResult<ObjectHandle> {
    unimplemented!()
}

pub fn close_and_delete_peristent_object1(object: ObjectHandle) -> TeeResult {
    unimplemented!()
}

pub fn rename_persistent_object(object: ObjectHandle, new_id: &[u8]) -> TeeResult {
    unimplemented!()
}

pub fn allocate_persistent_object_enumerator() -> TeeResult<ObjectEnumHandle> {
    unimplemented!()
}

pub fn free_persistent_object_enumerator(enumerator: ObjectEnumHandle) {
    unimplemented!()
}

pub fn reset_persistent_object_enumerator(enumerator: ObjectEnumHandle) {
    unimplemented!()
}

pub fn start_persistent_object_enumerator(
    enumerator: ObjectEnumHandle,
    storage: Storage,
) -> TeeResult {
    unimplemented!()
}

pub fn get_next_persistent_object(
    enumerator: ObjectEnumHandle,
    id: &mut [u8],
) -> TeeResult<ObjectInfo> {
    unimplemented!()
}

pub fn read_object_data(object: ObjectHandle, buffer: &mut [u8]) -> TeeResult<usize> {
    unimplemented!()
}

pub fn write_object_data(object: ObjectHandle, buffer: &[u8]) -> TeeResult {
    unimplemented!()
}

pub fn truncate_object_data(object: ObjectHandle, size: usize) -> TeeResult {
    unimplemented!()
}

pub fn seek_data_object(object: ObjectHandle, offset: usize, whence: Whence) -> TeeResult {
    unimplemented!()
}
