// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::error::Error;
use crate::writer::heap::Heap;
use crate::writer::{Inspector, StringReference};
use derivative::Derivative;
use fuchsia_sync::{Mutex, MutexGuard};
use futures::future::BoxFuture;
use inspect_format::{
    constants, utils, Array, ArrayFormat, ArraySlotKind, Block, BlockAccessorExt,
    BlockAccessorMutExt, BlockContainer, BlockIndex, BlockType, Bool, Buffer, Container, Double,
    Error as FormatError, Extent, Int, Link, LinkNodeDisposition, Name, Node, PropertyFormat,
    Reserved, StringRef, Tombstone, Uint, Unknown,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Callback used to fill inspector lazy nodes.
pub type LazyNodeContextFnArc =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> + Sync + Send>;

trait SafeOp {
    fn safe_sub(&self, other: Self) -> Self;
    fn safe_add(&self, other: Self) -> Self;
}

impl SafeOp for u64 {
    fn safe_sub(&self, other: u64) -> u64 {
        self.checked_sub(other).unwrap_or(0)
    }
    fn safe_add(&self, other: u64) -> u64 {
        self.checked_add(other).unwrap_or(u64::MAX)
    }
}

impl SafeOp for i64 {
    fn safe_sub(&self, other: i64) -> i64 {
        self.checked_sub(other).unwrap_or(i64::MIN)
    }
    fn safe_add(&self, other: i64) -> i64 {
        self.checked_add(other).unwrap_or(i64::MAX)
    }
}

impl SafeOp for f64 {
    fn safe_sub(&self, other: f64) -> f64 {
        self - other
    }
    fn safe_add(&self, other: f64) -> f64 {
        self + other
    }
}

macro_rules! locked_state_metric_fns {
    ($name:ident, $type:ident) => {
        paste::paste! {
            pub fn [<create_ $name _metric>](
                &mut self,
                name: StringReference,
                value: $type,
                parent_index: BlockIndex,
            ) -> Result<BlockIndex, Error> {
                self.inner_lock.[<create_ $name _metric>](name, value, parent_index)
            }

            pub fn [<set_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) {
                self.inner_lock.[<set_ $name _metric>](block_index, value);
            }

            pub fn [<add_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) -> $type {
                self.inner_lock.[<add_ $name _metric>](block_index, value)
            }

            pub fn [<subtract_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) -> $type {
                self.inner_lock.[<subtract_ $name _metric>](block_index, value)
            }
        }
    };
}

/// Generate create, set, add and subtract methods for a metric.
macro_rules! metric_fns {
    ($name:ident, $type:ident, $marker:ident) => {
        paste::paste! {
            fn [<create_ $name _metric>](
                &mut self,
                name: StringReference,
                value: $type,
                parent_index: BlockIndex,
            ) -> Result<BlockIndex, Error> {
                let (block_index, name_block_index) = self.allocate_reserved_value(
                    name, parent_index, constants::MIN_ORDER_SIZE)?;
                self.heap.container.block_at_unchecked_mut::<Reserved>(block_index)
                    .[<become_ $name _value>](value, name_block_index, parent_index);
                Ok(block_index)
            }

            fn [<set_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) {
                let mut block = self.heap.container.block_at_unchecked_mut::<$marker>(block_index);
                block.set(value);
            }

            fn [<add_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) -> $type {
                let mut block = self.heap.container.block_at_unchecked_mut::<$marker>(block_index);
                let current_value = block.value();
                let new_value = current_value.safe_add(value);
                block.set(new_value);
                new_value
            }

            fn [<subtract_ $name _metric>](&mut self, block_index: BlockIndex, value: $type) -> $type {
                let mut block = self.heap.container.block_at_unchecked_mut::<$marker>(block_index);
                let current_value = block.value();
                let new_value = current_value.safe_sub(value);
                block.set(new_value);
                new_value
            }
        }
    };
}
macro_rules! locked_state_array_fns {
    ($name:ident, $type:ident, $value:ident) => {
        paste::paste! {
            pub fn [<create_ $name _array>](
                &mut self,
                name: StringReference,
                slots: usize,
                array_format: ArrayFormat,
                parent_index: BlockIndex,
            ) -> Result<BlockIndex, Error> {
                self.inner_lock.[<create_ $name _array>](name, slots, array_format, parent_index)
            }

            pub fn [<set_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) {
                self.inner_lock.[<set_array_ $name _slot>](block_index, slot_index, value);
            }

            pub fn [<add_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Option<$type> {
                self.inner_lock.[<add_array_ $name _slot>](block_index, slot_index, value)
            }

            pub fn [<subtract_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Option<$type> {
                self.inner_lock.[<subtract_array_ $name _slot>](block_index, slot_index, value)
            }
        }
    };
}

macro_rules! arithmetic_array_fns {
    ($name:ident, $type:ident, $value:ident, $marker:ident) => {
        paste::paste! {
            pub fn [<create_ $name _array>](
                &mut self,
                name: StringReference,
                slots: usize,
                array_format: ArrayFormat,
                parent_index: BlockIndex,
            ) -> Result<BlockIndex, Error> {
                let block_size =
                    slots as usize * std::mem::size_of::<$type>() + constants::MIN_ORDER_SIZE;
                if block_size > constants::MAX_ORDER_SIZE {
                    return Err(Error::BlockSizeTooBig(block_size))
                }
                let (block_index, name_block_index) = self.allocate_reserved_value(
                    name, parent_index, block_size)?;
                self.heap.container
                    .block_at_unchecked_mut::<Reserved>(block_index)
                    .become_array_value::<$marker>(
                        slots, array_format, name_block_index, parent_index
                    )?;
                Ok(block_index)
            }

            pub fn [<set_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) {
                let mut block = self.heap.container
                    .block_at_unchecked_mut::<Array<$marker>>(block_index);
                block.set(slot_index, value);
            }

            pub fn [<add_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Option<$type> {
                let mut block = self.heap.container
                    .block_at_unchecked_mut::<Array<$marker>>(block_index);
                let previous_value = block.get(slot_index)?;
                let new_value = previous_value.safe_add(value);
                block.set(slot_index, new_value);
                Some(new_value)
            }

            pub fn [<subtract_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Option<$type> {
                let mut block = self.heap.container
                    .block_at_unchecked_mut::<Array<$marker>>(block_index);
                let previous_value = block.get(slot_index)?;
                let new_value = previous_value.safe_sub(value);
                block.set(slot_index, new_value);
                Some(new_value)
            }
        }
    };
}

/// In charge of performing all operations on the VMO as well as managing the lock and unlock
/// behavior.
/// `State` writes version 2 of the Inspect Format.
#[derive(Clone, Debug)]
pub struct State {
    /// The inner state that actually performs the operations.
    /// This should always be accessed by locking the mutex and then locking the header.
    // TODO(https://fxbug.dev/42128473): have a single locking mechanism implemented on top of the vmo header.
    inner: Arc<Mutex<InnerState>>,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl State {
    /// Create a |State| object wrapping the given Heap. This will cause the
    /// heap to be initialized with a header.
    pub fn create(
        heap: Heap<Container>,
        storage: Arc<<Container as BlockContainer>::ShareableData>,
    ) -> Result<Self, Error> {
        let inner = Arc::new(Mutex::new(InnerState::new(heap, storage)));
        Ok(Self { inner })
    }

    /// Locks the state mutex and inspect vmo. The state will be unlocked on drop.
    /// This can fail when the header is already locked.
    pub fn try_lock(&self) -> Result<LockedStateGuard<'_>, Error> {
        let inner_lock = self.inner.lock();
        LockedStateGuard::new(inner_lock)
    }

    /// Locks the state mutex and inspect vmo. The state will be unlocked on drop.
    /// This can fail when the header is already locked.
    pub fn begin_transaction(&self) {
        self.inner.lock().lock_header();
    }

    /// Locks the state mutex and inspect vmo. The state will be unlocked on drop.
    /// This can fail when the header is already locked.
    pub fn end_transaction(&self) {
        self.inner.lock().unlock_header();
    }

    /// Copies the bytes in the VMO into the returned vector.
    pub fn copy_vmo_bytes(&self) -> Option<Vec<u8>> {
        let state = self.inner.lock();
        if state.transaction_count > 0 {
            return None;
        }

        Some(state.heap.bytes())
    }
}

#[cfg(test)]
impl State {
    pub(crate) fn with_current_header<F, R>(&self, callback: F) -> R
    where
        F: FnOnce(&Block<&Container, inspect_format::Header>) -> R,
    {
        // A lock guard for the test, which doesn't execute its drop impl as well as that would
        // cause changes in the VMO generation count.
        let lock_guard = LockedStateGuard::without_gen_count_changes(self.inner.lock());
        let block = lock_guard.header();
        callback(&block)
    }

    #[track_caller]
    pub(crate) fn get_block<F, K>(&self, index: BlockIndex, callback: F)
    where
        K: inspect_format::BlockKind,
        F: FnOnce(&Block<&Container, K>),
    {
        let state_lock = self.try_lock().unwrap();
        callback(&state_lock.get_block::<K>(index))
    }

    #[track_caller]
    pub(crate) fn get_block_mut<F, K>(&self, index: BlockIndex, callback: F)
    where
        K: inspect_format::BlockKind,
        F: FnOnce(&mut Block<&mut Container, K>),
    {
        let mut state_lock = self.try_lock().unwrap();
        callback(&mut state_lock.get_block_mut::<K>(index))
    }
}

/// Statistics about the current inspect state.
#[derive(Debug, Eq, PartialEq)]
pub struct Stats {
    /// Number of lazy links (lazy children and values) that have been added to the state.
    pub total_dynamic_children: usize,

    /// Maximum size of the vmo backing inspect.
    pub maximum_size: usize,

    /// Current size of the vmo backing inspect.
    pub current_size: usize,

    /// Total number of allocated blocks. This includes blocks that might have already been
    /// deallocated. That is, `allocated_blocks` - `deallocated_blocks` = currently allocated.
    pub allocated_blocks: usize,

    /// Total number of deallocated blocks.
    pub deallocated_blocks: usize,

    /// Total number of failed allocations.
    pub failed_allocations: usize,
}

pub struct LockedStateGuard<'a> {
    inner_lock: MutexGuard<'a, InnerState>,
    #[cfg(test)]
    drop: bool,
}

#[cfg(target_os = "fuchsia")]
impl LockedStateGuard<'_> {
    /// Freezes the VMO, does a CoW duplication, thaws the parent, and returns the child.
    pub fn frozen_vmo_copy(&mut self) -> Result<Option<zx::Vmo>, Error> {
        self.inner_lock.frozen_vmo_copy()
    }
}

impl<'a> LockedStateGuard<'a> {
    fn new(mut inner_lock: MutexGuard<'a, InnerState>) -> Result<Self, Error> {
        if inner_lock.transaction_count == 0 {
            inner_lock.header_mut().lock();
        }
        Ok(Self {
            inner_lock,
            #[cfg(test)]
            drop: true,
        })
    }

    /// Returns statistics about the current inspect state.
    pub fn stats(&self) -> Stats {
        Stats {
            total_dynamic_children: self.inner_lock.callbacks.len(),
            current_size: self.inner_lock.heap.current_size(),
            maximum_size: self.inner_lock.heap.maximum_size(),
            allocated_blocks: self.inner_lock.heap.total_allocated_blocks(),
            deallocated_blocks: self.inner_lock.heap.total_deallocated_blocks(),
            failed_allocations: self.inner_lock.heap.failed_allocations(),
        }
    }

    /// Returns a reference to the lazy callbacks map.
    pub fn callbacks(&self) -> &HashMap<StringReference, LazyNodeContextFnArc> {
        &self.inner_lock.callbacks
    }

    /// Allocate a NODE block with the given |name| and |parent_index|.
    pub fn create_node(
        &mut self,
        name: StringReference,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_node(name, parent_index)
    }

    /// Allocate a LINK block with the given |name| and |parent_index| and keep track
    /// of the callback that will fill it.
    pub fn create_lazy_node<F>(
        &mut self,
        name: StringReference,
        parent_index: BlockIndex,
        disposition: LinkNodeDisposition,
        callback: F,
    ) -> Result<BlockIndex, Error>
    where
        F: Fn() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> + Sync + Send + 'static,
    {
        self.inner_lock.create_lazy_node(name, parent_index, disposition, callback)
    }

    pub fn free_lazy_node(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.inner_lock.free_lazy_node(index)
    }

    /// Free a *_VALUE block at the given |index|.
    pub fn free_value(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.inner_lock.free_value(index)
    }

    /// Allocate a BUFFER_VALUE block with the given |name|, |value| and |parent_index|.
    pub fn create_buffer_property(
        &mut self,
        name: StringReference,
        value: &[u8],
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_buffer_property(name, value, parent_index)
    }

    /// Allocate a BUFFER_VALUE block with the given |name|, |value| and |parent_index|, where
    /// |value| is stored as a |STRING_REFERENCE|.
    pub fn create_string(
        &mut self,
        name: StringReference,
        value: StringReference,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_string(name, value, parent_index)
    }

    pub fn reparent(
        &mut self,
        being_reparented: BlockIndex,
        new_parent: BlockIndex,
    ) -> Result<(), Error> {
        self.inner_lock.reparent(being_reparented, new_parent)
    }

    /// Free a BUFFER_VALUE block.
    pub fn free_string_or_bytes_buffer_property(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.inner_lock.free_string_or_bytes_buffer_property(index)
    }

    /// Set the |value| of a StringReference BUFFER_VALUE block.
    pub fn set_string_property(
        &mut self,
        block_index: BlockIndex,
        value: impl Into<StringReference>,
    ) -> Result<(), Error> {
        self.inner_lock.set_string_property(block_index, value)
    }

    /// Set the |value| of a non-StringReference BUFFER_VALUE block.
    pub fn set_buffer_property(
        &mut self,
        block_index: BlockIndex,
        value: &[u8],
    ) -> Result<(), Error> {
        self.inner_lock.set_buffer_property(block_index, value)
    }

    pub fn create_bool(
        &mut self,
        name: StringReference,
        value: bool,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_bool(name, value, parent_index)
    }

    pub fn set_bool(&mut self, block_index: BlockIndex, value: bool) {
        self.inner_lock.set_bool(block_index, value)
    }

    locked_state_metric_fns!(int, i64);
    locked_state_metric_fns!(uint, u64);
    locked_state_metric_fns!(double, f64);

    locked_state_array_fns!(int, i64, IntValue);
    locked_state_array_fns!(uint, u64, UintValue);
    locked_state_array_fns!(double, f64, DoubleValue);

    /// Sets all slots of the array at the given index to zero
    pub fn clear_array(
        &mut self,
        block_index: BlockIndex,
        start_slot_index: usize,
    ) -> Result<(), Error> {
        self.inner_lock.clear_array(block_index, start_slot_index)
    }

    pub fn create_string_array(
        &mut self,
        name: StringReference,
        slots: usize,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_string_array(name, slots, parent_index)
    }

    pub fn get_array_size(&self, block_index: BlockIndex) -> usize {
        self.inner_lock.get_array_size(block_index)
    }

    pub fn set_array_string_slot(
        &mut self,
        block_index: BlockIndex,
        slot_index: usize,
        value: StringReference,
    ) -> Result<(), Error> {
        self.inner_lock.set_array_string_slot(block_index, slot_index, value)
    }
}

impl Drop for LockedStateGuard<'_> {
    fn drop(&mut self) {
        #[cfg(test)]
        {
            if !self.drop {
                return;
            }
        }
        if self.inner_lock.transaction_count == 0 {
            self.inner_lock.header_mut().unlock();
        }
    }
}

#[cfg(test)]
impl<'a> LockedStateGuard<'a> {
    fn without_gen_count_changes(inner_lock: MutexGuard<'a, InnerState>) -> Self {
        Self { inner_lock, drop: false }
    }

    pub(crate) fn load_string(&self, index: BlockIndex) -> Result<String, Error> {
        self.inner_lock.load_key_string(index)
    }

    pub(crate) fn allocate_link(
        &mut self,
        name: StringReference,
        content: StringReference,
        disposition: LinkNodeDisposition,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.allocate_link(name, content, disposition, parent_index)
    }

    #[track_caller]
    pub(crate) fn get_block<K: inspect_format::BlockKind>(
        &self,
        index: BlockIndex,
    ) -> Block<&Container, K> {
        self.inner_lock.heap.container.maybe_block_at::<K>(index).unwrap()
    }

    fn header(&self) -> Block<&Container, inspect_format::Header> {
        self.get_block(BlockIndex::HEADER)
    }

    #[track_caller]
    fn get_block_mut<K: inspect_format::BlockKind>(
        &mut self,
        index: BlockIndex,
    ) -> Block<&mut Container, K> {
        self.inner_lock.heap.container.maybe_block_at_mut::<K>(index).unwrap()
    }
}

/// Wraps a heap and implements the Inspect VMO API on top of it at a low level.
#[derive(Derivative)]
#[derivative(Debug)]
struct InnerState {
    #[derivative(Debug = "ignore")]
    heap: Heap<Container>,
    #[allow(dead_code)] //  unused in host.
    storage: Arc<<Container as BlockContainer>::ShareableData>,
    next_unique_link_id: AtomicU64,
    transaction_count: usize,

    // associates a reference with it's block index
    string_reference_block_indexes: HashMap<StringReference, BlockIndex>,

    #[derivative(Debug = "ignore")]
    callbacks: HashMap<StringReference, LazyNodeContextFnArc>,
}

#[cfg(target_os = "fuchsia")]
impl InnerState {
    fn frozen_vmo_copy(&mut self) -> Result<Option<zx::Vmo>, Error> {
        if self.transaction_count > 0 {
            return Ok(None);
        }

        let old = self.header_mut().freeze();
        let ret = self
            .storage
            .create_child(
                zx::VmoChildOptions::SNAPSHOT | zx::VmoChildOptions::NO_WRITE,
                0,
                self.storage.get_size().map_err(Error::VmoSize)?,
            )
            .ok();
        self.header_mut().thaw(old);
        Ok(ret)
    }
}

impl InnerState {
    /// Creates a new inner state that performs all operations on the heap.
    pub fn new(
        heap: Heap<Container>,
        storage: Arc<<Container as BlockContainer>::ShareableData>,
    ) -> Self {
        Self {
            heap,
            storage,
            next_unique_link_id: AtomicU64::new(0),
            callbacks: HashMap::new(),
            transaction_count: 0,
            string_reference_block_indexes: HashMap::new(),
        }
    }

    #[inline]
    fn header_mut(&mut self) -> Block<&mut Container, inspect_format::Header> {
        self.heap.container.block_at_unchecked_mut(BlockIndex::HEADER)
    }

    fn lock_header(&mut self) {
        if self.transaction_count == 0 {
            self.header_mut().lock();
        }
        self.transaction_count += 1;
    }

    fn unlock_header(&mut self) {
        self.transaction_count -= 1;
        if self.transaction_count == 0 {
            self.header_mut().unlock();
        }
    }

    /// Allocate a NODE block with the given |name| and |parent_index|.
    fn create_node(
        &mut self,
        name: StringReference,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        self.heap
            .container
            .block_at_unchecked_mut::<Reserved>(block_index)
            .become_node(name_block_index, parent_index);
        Ok(block_index)
    }

    /// Allocate a LINK block with the given |name| and |parent_index| and keep track
    /// of the callback that will fill it.
    fn create_lazy_node<F>(
        &mut self,
        name: StringReference,
        parent_index: BlockIndex,
        disposition: LinkNodeDisposition,
        callback: F,
    ) -> Result<BlockIndex, Error>
    where
        F: Fn() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> + Sync + Send + 'static,
    {
        let content: StringReference = self.unique_link_name(&name).into();
        let link = self.allocate_link(name, content.clone(), disposition, parent_index)?;
        self.callbacks.insert(content, Arc::from(callback));
        Ok(link)
    }

    /// Frees a LINK block at the given |index|.
    fn free_lazy_node(&mut self, index: BlockIndex) -> Result<(), Error> {
        let content_block_index =
            self.heap.container.block_at_unchecked::<Link>(index).content_index();
        let content_block_type = self.heap.container.block_at(content_block_index).block_type();
        let content = self.load_key_string(content_block_index)?;
        self.delete_value(index)?;
        // Free the name or string reference block used for content.
        match content_block_type {
            Some(BlockType::StringReference) => {
                self.release_string_reference(content_block_index)?;
            }
            _ => {
                self.heap.free_block(content_block_index).expect("Failed to free block");
            }
        }

        self.callbacks.remove(content.as_str());
        Ok(())
    }

    fn unique_link_name(&mut self, prefix: &str) -> String {
        let id = self.next_unique_link_id.fetch_add(1, Ordering::Relaxed);
        format!("{}-{}", prefix, id)
    }

    pub(crate) fn allocate_link(
        &mut self,
        name: StringReference,
        content: StringReference,
        disposition: LinkNodeDisposition,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (value_block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        let result =
            self.get_or_create_string_reference(content).and_then(|content_block_index| {
                self.heap
                    .container
                    .block_at_unchecked_mut::<StringRef>(content_block_index)
                    .increment_ref_count()?;
                self.heap
                    .container
                    .block_at_unchecked_mut::<Reserved>(value_block_index)
                    .become_link(name_block_index, parent_index, content_block_index, disposition);
                Ok(())
            });
        match result {
            Ok(()) => Ok(value_block_index),
            Err(err) => {
                self.delete_value(value_block_index)?;
                Err(err)
            }
        }
    }

    /// Free a *_VALUE block at the given |index|.
    fn free_value(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.delete_value(index)?;
        Ok(())
    }

    /// Allocate a BUFFER_VALUE block with the given |name|, |value| and |parent_index|.
    fn create_buffer_property(
        &mut self,
        name: StringReference,
        value: &[u8],
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        self.heap.container.block_at_unchecked_mut::<Reserved>(block_index).become_property(
            name_block_index,
            parent_index,
            PropertyFormat::Bytes,
        );
        if let Err(err) = self.inner_set_buffer_property_value(block_index, value) {
            self.heap.free_block(block_index)?;
            self.release_string_reference(name_block_index)?;
            return Err(err);
        }
        Ok(block_index)
    }

    /// Allocate a BUFFER_VALUE block with the given |name|, |value| and |parent_index|, where
    /// |value| is stored as a STRING_REFERENCE.
    fn create_string(
        &mut self,
        name: StringReference,
        value: StringReference,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        self.heap.container.block_at_unchecked_mut::<Reserved>(block_index).become_property(
            name_block_index,
            parent_index,
            PropertyFormat::StringReference,
        );

        let value_block_index = match self.get_or_create_string_reference(value) {
            Ok(b_index) => {
                self.heap
                    .container
                    .block_at_unchecked_mut::<StringRef>(b_index)
                    .increment_ref_count()?;
                b_index
            }
            Err(err) => {
                self.heap.free_block(block_index)?;
                return Err(err);
            }
        };

        let mut block = self.heap.container.block_at_unchecked_mut::<Buffer>(block_index);
        block.set_extent_index(value_block_index);
        block.set_total_length(0);

        Ok(block_index)
    }

    /// Get or allocate a STRING_REFERENCE block with the given |value|.
    /// When a new string reference is created, its reference count is set to zero.
    fn get_or_create_string_reference(
        &mut self,
        value: StringReference,
    ) -> Result<BlockIndex, Error> {
        let string_reference = value;
        match self.string_reference_block_indexes.get(&string_reference) {
            Some(index) => Ok(*index),
            None => {
                let block_index = self.heap.allocate_block(utils::block_size_for_payload(
                    string_reference.len() + constants::STRING_REFERENCE_TOTAL_LENGTH_BYTES,
                ))?;
                self.heap
                    .container
                    .block_at_unchecked_mut::<Reserved>(block_index)
                    .become_string_reference();
                self.write_string_reference_payload(block_index, &string_reference)?;
                self.string_reference_block_indexes.insert(string_reference, block_index);
                Ok(block_index)
            }
        }
    }

    /// Given a string, write the canonical value out, allocating as needed.
    fn write_string_reference_payload(
        &mut self,
        block_index: BlockIndex,
        value: &str,
    ) -> Result<(), Error> {
        let value_bytes = value.as_bytes();
        let (head_extent, bytes_written) = {
            let inlined = self.inline_string_reference(block_index, value.as_bytes());
            if inlined < value.len() {
                let (head, in_extents) = self.write_extents(&value_bytes[inlined..])?;
                (head, inlined + in_extents)
            } else {
                (BlockIndex::EMPTY, inlined)
            }
        };
        let mut block = self.heap.container.block_at_unchecked_mut::<StringRef>(block_index);
        block.set_next_index(head_extent);
        block.set_total_length(bytes_written.try_into().unwrap_or(u32::MAX));
        Ok(())
    }

    /// Given a string, write the portion that can be inlined to the given block.
    /// Return the number of bytes written.
    fn inline_string_reference(&mut self, block_index: BlockIndex, value: &[u8]) -> usize {
        self.heap.container.block_at_unchecked_mut::<StringRef>(block_index).write_inline(value)
    }

    /// Decrement the reference count on the block and free it if the count is 0.
    /// This is the function to call if you want to give up your hold on a StringReference.
    fn release_string_reference(&mut self, block_index: BlockIndex) -> Result<(), Error> {
        self.heap
            .container
            .block_at_unchecked_mut::<StringRef>(block_index)
            .decrement_ref_count()?;
        self.maybe_free_string_reference(block_index)
    }

    /// Free a STRING_REFERENCE if the count is 0. This should not be
    /// directly called outside of tests.
    fn maybe_free_string_reference(&mut self, block_index: BlockIndex) -> Result<(), Error> {
        let block = self.heap.container.block_at_unchecked::<StringRef>(block_index);
        if block.reference_count() != 0 {
            return Ok(());
        }
        let first_extent = block.next_extent();
        self.heap.free_block(block_index)?;
        self.string_reference_block_indexes.retain(|_, vmo_index| *vmo_index != block_index);

        if first_extent == BlockIndex::EMPTY {
            return Ok(());
        }
        self.free_extents(first_extent)
    }

    fn load_key_string(&self, index: BlockIndex) -> Result<String, Error> {
        let block = self.heap.container.block_at(index);
        match block.block_type() {
            Some(BlockType::StringReference) => {
                self.read_string_reference(block.cast::<StringRef>().unwrap())
            }
            Some(BlockType::Name) => block
                .cast::<Name>()
                .unwrap()
                .contents()
                .map(|s| s.to_string())
                .map_err(|_| Error::NameNotUtf8),
            _ => Err(Error::InvalidBlockTypeNumber(index, block.block_type_raw())),
        }
    }

    /// Read a StringReference
    fn read_string_reference(&self, block: Block<&Container, StringRef>) -> Result<String, Error> {
        let mut content = block.inline_data()?.to_vec();
        let mut next = block.next_extent();
        while next != BlockIndex::EMPTY {
            let next_block = self.heap.container.block_at_unchecked::<Extent>(next);
            content.extend_from_slice(next_block.contents()?);
            next = next_block.next_extent();
        }

        content.truncate(block.total_length());
        String::from_utf8(content).ok().ok_or(Error::NameNotUtf8)
    }

    /// Free a BUFFER_VALUE block.
    fn free_string_or_bytes_buffer_property(&mut self, index: BlockIndex) -> Result<(), Error> {
        let (format, data_index) = {
            let block = self.heap.container.block_at_unchecked::<Buffer>(index);
            (block.format(), block.extent_index())
        };
        match format {
            Some(PropertyFormat::String) | Some(PropertyFormat::Bytes) => {
                self.free_extents(data_index)?;
            }
            Some(PropertyFormat::StringReference) => {
                self.release_string_reference(data_index)?;
            }
            _ => {
                return Err(Error::VmoFormat(FormatError::InvalidBufferFormat(
                    self.heap.container.block_at_unchecked(index).format_raw(),
                )));
            }
        }

        self.delete_value(index)?;
        Ok(())
    }

    /// Set the |value| of a String BUFFER_VALUE block.
    fn set_string_property(
        &mut self,
        block_index: BlockIndex,
        value: impl Into<StringReference>,
    ) -> Result<(), Error> {
        self.inner_set_string_property_value(block_index, value)?;
        Ok(())
    }

    /// Set the |value| of a String BUFFER_VALUE block.
    fn set_buffer_property(&mut self, block_index: BlockIndex, value: &[u8]) -> Result<(), Error> {
        self.inner_set_buffer_property_value(block_index, value)?;
        Ok(())
    }

    fn check_lineage(
        &self,
        being_reparented: BlockIndex,
        new_parent: BlockIndex,
    ) -> Result<(), Error> {
        // you cannot adopt the root node
        if being_reparented == BlockIndex::ROOT {
            return Err(Error::AdoptAncestor);
        }

        let mut being_checked = new_parent;
        while being_checked != BlockIndex::ROOT {
            if being_checked == being_reparented {
                return Err(Error::AdoptAncestor);
            }
            // Note: all values share the parent_index in the same position, so we can just assume
            // we have ANY_VALUE here, so just using a Node.
            being_checked =
                self.heap.container.block_at_unchecked::<Node>(being_checked).parent_index();
        }

        Ok(())
    }

    fn reparent(
        &mut self,
        being_reparented: BlockIndex,
        new_parent: BlockIndex,
    ) -> Result<(), Error> {
        self.check_lineage(being_reparented, new_parent)?;
        let original_parent_idx =
            self.heap.container.block_at_unchecked::<Node>(being_reparented).parent_index();
        if original_parent_idx != BlockIndex::ROOT {
            let mut original_parent_block =
                self.heap.container.block_at_unchecked_mut::<Node>(original_parent_idx);
            let child_count = original_parent_block.child_count() - 1;
            original_parent_block.set_child_count(child_count);
        }

        self.heap.container.block_at_unchecked_mut::<Node>(being_reparented).set_parent(new_parent);

        if new_parent != BlockIndex::ROOT {
            let mut new_parent_block =
                self.heap.container.block_at_unchecked_mut::<Node>(new_parent);
            let child_count = new_parent_block.child_count() + 1;
            new_parent_block.set_child_count(child_count);
        }

        Ok(())
    }

    fn create_bool(
        &mut self,
        name: StringReference,
        value: bool,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        self.heap.container.block_at_unchecked_mut::<Reserved>(block_index).become_bool_value(
            value,
            name_block_index,
            parent_index,
        );
        Ok(block_index)
    }

    fn set_bool(&mut self, block_index: BlockIndex, value: bool) {
        let mut block = self.heap.container.block_at_unchecked_mut::<Bool>(block_index);
        block.set(value);
    }

    metric_fns!(int, i64, Int);
    metric_fns!(uint, u64, Uint);
    metric_fns!(double, f64, Double);

    arithmetic_array_fns!(int, i64, IntValue, Int);
    arithmetic_array_fns!(uint, u64, UintValue, Uint);
    arithmetic_array_fns!(double, f64, DoubleValue, Double);

    fn create_string_array(
        &mut self,
        name: StringReference,
        slots: usize,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let block_size = slots * StringRef::array_entry_type_size() + constants::MIN_ORDER_SIZE;
        if block_size > constants::MAX_ORDER_SIZE {
            return Err(Error::BlockSizeTooBig(block_size));
        }
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, block_size)?;
        self.heap
            .container
            .block_at_unchecked_mut::<Reserved>(block_index)
            .become_array_value::<StringRef>(
                slots,
                ArrayFormat::Default,
                name_block_index,
                parent_index,
            )?;
        Ok(block_index)
    }

    fn get_array_size(&self, block_index: BlockIndex) -> usize {
        let block = self.heap.container.block_at_unchecked::<Array<Unknown>>(block_index);
        block.slots()
    }

    fn set_array_string_slot(
        &mut self,
        block_index: BlockIndex,
        slot_index: usize,
        value: StringReference,
    ) -> Result<(), Error> {
        if self.heap.container.block_at_unchecked_mut::<Array<StringRef>>(block_index).slots()
            <= slot_index
        {
            return Err(Error::VmoFormat(FormatError::ArrayIndexOutOfBounds(slot_index)));
        }

        let reference_index = if !value.is_empty() {
            let reference_index = self.get_or_create_string_reference(value)?;
            self.heap
                .container
                .block_at_unchecked_mut::<StringRef>(reference_index)
                .increment_ref_count()?;
            reference_index
        } else {
            BlockIndex::EMPTY
        };

        let existing_index = self
            .heap
            .container
            .block_at_unchecked::<Array<StringRef>>(block_index)
            .get_string_index_at(slot_index)
            .ok_or(Error::InvalidArrayIndex(slot_index))?;
        if existing_index != BlockIndex::EMPTY {
            self.release_string_reference(existing_index)?;
        }

        self.heap
            .container
            .block_at_unchecked_mut::<Array<StringRef>>(block_index)
            .set_string_slot(slot_index, reference_index);
        Ok(())
    }

    /// Sets all slots of the array at the given index to zero.
    /// Does appropriate deallocation on string references in payload.
    fn clear_array(
        &mut self,
        block_index: BlockIndex,
        start_slot_index: usize,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/392965471): this should be cleaner. Technically we can know
        // statically what kind of block we are dealing with.
        let block = self.heap.container.block_at_unchecked_mut::<Array<Unknown>>(block_index);
        match block.entry_type() {
            Some(value) if value.is_numeric_value() => {
                self.heap
                    .container
                    .block_at_unchecked_mut::<Array<Unknown>>(block_index)
                    .clear(start_slot_index);
            }
            Some(BlockType::StringReference) => {
                let array_slots = block.slots();
                for i in start_slot_index..array_slots {
                    let index = {
                        let mut block = self
                            .heap
                            .container
                            .block_at_unchecked_mut::<Array<StringRef>>(block_index);
                        let index =
                            block.get_string_index_at(i).ok_or(Error::InvalidArrayIndex(i))?;
                        if index == BlockIndex::EMPTY {
                            continue;
                        }
                        block.set_string_slot(i, BlockIndex::EMPTY);
                        index
                    };
                    self.release_string_reference(index)?;
                }
            }

            _ => return Err(Error::InvalidArrayType(block_index)),
        }

        Ok(())
    }

    fn allocate_reserved_value(
        &mut self,
        name: StringReference,
        parent_index: BlockIndex,
        block_size: usize,
    ) -> Result<(BlockIndex, BlockIndex), Error> {
        let block_index = self.heap.allocate_block(block_size)?;
        let name_block_index = match self.get_or_create_string_reference(name) {
            Ok(b_index) => {
                self.heap
                    .container
                    .block_at_unchecked_mut::<StringRef>(b_index)
                    .increment_ref_count()?;
                b_index
            }
            Err(err) => {
                self.heap.free_block(block_index)?;
                return Err(err);
            }
        };

        let result = {
            // Safety: NodeValues and Tombstones always have child_count
            let mut parent_block = self.heap.container.block_at_unchecked_mut::<Node>(parent_index);
            let parent_block_type = parent_block.block_type();
            match parent_block_type {
                Some(BlockType::NodeValue) | Some(BlockType::Tombstone) => {
                    parent_block.set_child_count(parent_block.child_count() + 1);
                    Ok(())
                }
                Some(BlockType::Header) => Ok(()),
                _ => Err(Error::InvalidBlockType(parent_index, parent_block.block_type_raw())),
            }
        };
        match result {
            Ok(()) => Ok((block_index, name_block_index)),
            Err(err) => {
                self.release_string_reference(name_block_index)?;
                self.heap.free_block(block_index)?;
                Err(err)
            }
        }
    }

    fn delete_value(&mut self, block_index: BlockIndex) -> Result<(), Error> {
        // For our purposes here, we just need "ANY_VALUE". Using "node".
        let block = self.heap.container.block_at_unchecked::<Node>(block_index);
        let parent_index = block.parent_index();
        let name_index = block.name_index();

        // Decrement parent child count.
        if parent_index != BlockIndex::ROOT {
            let parent = self.heap.container.block_at_mut(parent_index);
            match parent.block_type() {
                Some(BlockType::Tombstone) => {
                    let mut parent = parent.cast::<Tombstone>().unwrap();
                    let child_count = parent.child_count() - 1;
                    if child_count == 0 {
                        self.heap.free_block(parent_index).expect("Failed to free block");
                    } else {
                        parent.set_child_count(child_count);
                    }
                }
                Some(BlockType::NodeValue) => {
                    let mut parent = parent.cast::<Node>().unwrap();
                    let child_count = parent.child_count() - 1;
                    parent.set_child_count(child_count);
                }
                other => {
                    unreachable!(
                        "the parent of any value is either tombstone or node. Saw: {other:?}"
                    );
                }
            }
        }

        // Free the name block.
        match self.heap.container.block_at(name_index).block_type() {
            Some(BlockType::StringReference) => {
                self.release_string_reference(name_index)?;
            }
            _ => self.heap.free_block(name_index).expect("Failed to free block"),
        }

        // If the block is a NODE and has children, make it a TOMBSTONE so that
        // it's freed when the last of its children is freed. Otherwise, free it.
        let block = self.heap.container.block_at_mut(block_index);
        match block.cast::<Node>() {
            Some(block) if block.child_count() != 0 => {
                let _ = block.become_tombstone();
            }
            _ => {
                self.heap.free_block(block_index)?;
            }
        }
        Ok(())
    }

    fn inner_set_string_property_value(
        &mut self,
        block_index: BlockIndex,
        value: impl Into<StringReference>,
    ) -> Result<(), Error> {
        let old_string_ref_idx =
            self.heap.container.block_at_unchecked::<Buffer>(block_index).extent_index();
        let new_string_ref_idx = match self.get_or_create_string_reference(value.into()) {
            Ok(b_index) => {
                self.heap
                    .container
                    .block_at_unchecked_mut::<StringRef>(b_index)
                    .increment_ref_count()?;
                b_index
            }
            Err(err) => {
                self.heap.free_block(block_index)?;
                return Err(err);
            }
        };

        self.heap
            .container
            .block_at_unchecked_mut::<Buffer>(block_index)
            .set_extent_index(new_string_ref_idx);
        self.release_string_reference(old_string_ref_idx)?;
        Ok(())
    }

    fn inner_set_buffer_property_value(
        &mut self,
        block_index: BlockIndex,
        value: &[u8],
    ) -> Result<(), Error> {
        self.free_extents(
            self.heap.container.block_at_unchecked::<Buffer>(block_index).extent_index(),
        )?;
        let (result, (extent_index, written)) = match self.write_extents(value) {
            Ok((e, w)) => (Ok(()), (e, w)),
            Err(err) => (Err(err), (BlockIndex::ROOT, 0)),
        };
        let mut block = self.heap.container.block_at_unchecked_mut::<Buffer>(block_index);
        block.set_total_length(written.try_into().unwrap_or(u32::MAX));
        block.set_extent_index(extent_index);
        result
    }

    fn free_extents(&mut self, head_extent_index: BlockIndex) -> Result<(), Error> {
        let mut index = head_extent_index;
        while index != BlockIndex::ROOT {
            let next_index = self.heap.container.block_at_unchecked::<Extent>(index).next_extent();
            self.heap.free_block(index)?;
            index = next_index;
        }
        Ok(())
    }

    fn write_extents(&mut self, value: &[u8]) -> Result<(BlockIndex, usize), Error> {
        if value.is_empty() {
            // Invalid index
            return Ok((BlockIndex::ROOT, 0));
        }
        let mut offset = 0;
        let total_size = value.len();
        let head_extent_index =
            self.heap.allocate_block(utils::block_size_for_payload(total_size - offset))?;
        let mut extent_block_index = head_extent_index;
        while offset < total_size {
            let bytes_written = {
                let mut extent_block = self
                    .heap
                    .container
                    .block_at_unchecked_mut::<Reserved>(extent_block_index)
                    .become_extent(BlockIndex::EMPTY);
                extent_block.set_contents(&value[offset..])
            };
            offset += bytes_written;
            if offset < total_size {
                let Ok(block_index) =
                    self.heap.allocate_block(utils::block_size_for_payload(total_size - offset))
                else {
                    // If we fail to allocate, just take what was written already and bail.
                    return Ok((head_extent_index, offset));
                };
                self.heap
                    .container
                    .block_at_unchecked_mut::<Extent>(extent_block_index)
                    .set_next_index(block_index);
                extent_block_index = block_index;
            }
        }
        Ok((head_extent_index, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::snapshot::{BackingBuffer, ScannedBlock, Snapshot};
    use crate::reader::PartialNodeHierarchy;
    use crate::writer::testing_utils::get_state;
    use assert_matches::assert_matches;
    use diagnostics_assertions::assert_data_tree;
    use futures::prelude::*;
    use inspect_format::Header;

    #[track_caller]
    fn assert_all_free<'a>(blocks: impl Iterator<Item = Block<&'a BackingBuffer, Unknown>>) {
        let mut errors = vec![];
        for block in blocks {
            if block.block_type() != Some(BlockType::Free) {
                errors.push(format!(
                    "block at {} is {:?}, expected {}",
                    block.index(),
                    block.block_type(),
                    BlockType::Free
                ));
            }
        }

        if !errors.is_empty() {
            panic!("{errors:#?}");
        }
    }

    #[fuchsia::test]
    fn test_create() {
        let state = get_state(4096);
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_load_string() {
        let outer = get_state(4096);
        let mut state = outer.try_lock().expect("lock state");
        let block_index =
            state.inner_lock.get_or_create_string_reference("a value".into()).unwrap();
        assert_eq!(state.load_string(block_index).unwrap(), "a value");
    }

    #[fuchsia::test]
    fn test_check_lineage() {
        let core_state = get_state(4096);
        let mut state = core_state.try_lock().expect("lock state");
        let parent_index = state.create_node("".into(), 0.into()).unwrap();
        let child_index = state.create_node("".into(), parent_index).unwrap();
        let uncle_index = state.create_node("".into(), 0.into()).unwrap();

        state.inner_lock.check_lineage(parent_index, child_index).unwrap_err();
        state.inner_lock.check_lineage(0.into(), child_index).unwrap_err();
        state.inner_lock.check_lineage(child_index, uncle_index).unwrap();
    }

    #[fuchsia::test]
    fn test_reparent() {
        let core_state = get_state(4096);
        let mut state = core_state.try_lock().expect("lock state");

        let a_index = state.create_node("a".into(), 0.into()).unwrap();
        let b_index = state.create_node("b".into(), 0.into()).unwrap();

        let a = state.get_block::<Node>(a_index);
        let b = state.get_block::<Node>(b_index);
        assert_eq!(*a.parent_index(), 0);
        assert_eq!(*b.parent_index(), 0);

        assert_eq!(a.child_count(), 0);
        assert_eq!(b.child_count(), 0);

        state.reparent(b_index, a_index).unwrap();

        let a = state.get_block::<Node>(a_index);
        let b = state.get_block::<Node>(b_index);
        assert_eq!(*a.parent_index(), 0);
        assert_eq!(b.parent_index(), a.index());

        assert_eq!(a.child_count(), 1);
        assert_eq!(b.child_count(), 0);

        let c_index = state.create_node("c".into(), a_index).unwrap();

        let a = state.get_block::<Node>(a_index);
        let b = state.get_block::<Node>(b_index);
        let c = state.get_block::<Node>(c_index);
        assert_eq!(*a.parent_index(), 0);
        assert_eq!(b.parent_index(), a.index());
        assert_eq!(c.parent_index(), a.index());

        assert_eq!(a.child_count(), 2);
        assert_eq!(b.child_count(), 0);
        assert_eq!(c.child_count(), 0);

        state.reparent(c_index, b_index).unwrap();

        let a = state.get_block::<Node>(a_index);
        let b = state.get_block::<Node>(b_index);
        let c = state.get_block::<Node>(c_index);
        assert_eq!(*a.parent_index(), 0);
        assert_eq!(b.parent_index(), a_index);
        assert_eq!(c.parent_index(), b_index);

        assert_eq!(a.child_count(), 1);
        assert_eq!(b.child_count(), 1);
        assert_eq!(c.child_count(), 0);
    }

    #[fuchsia::test]
    fn test_node() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Create a node value and verify its fields
            let block_index = state.create_node("test-node".into(), 0.into()).unwrap();
            let block = state.get_block::<Node>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::NodeValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(block.child_count(), 0);
            assert_eq!(*block.name_index(), 4);
            assert_eq!(*block.parent_index(), 0);

            // Verify name block.
            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 9);
            assert_eq!(name_block.order(), 1);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test-node");
            block_index
        };

        // Verify blocks.
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::NodeValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::Free));
        assert_eq!(blocks[3].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(4));

        {
            let mut state = core_state.try_lock().expect("lock state");
            let child_block_index = state.create_node("child1".into(), block_index).unwrap();
            assert_eq!(state.get_block::<Node>(block_index).child_count(), 1);

            // Create a child of the child and verify child counts.
            let child11_block_index =
                state.create_node("child1-1".into(), child_block_index).unwrap();
            {
                assert_eq!(state.get_block::<Node>(child11_block_index).child_count(), 0);
                assert_eq!(state.get_block::<Node>(child_block_index).child_count(), 1);
                assert_eq!(state.get_block::<Node>(block_index).child_count(), 1);
            }

            assert!(state.free_value(child11_block_index).is_ok());
            {
                let child_block = state.get_block::<Node>(child_block_index);
                assert_eq!(child_block.child_count(), 0);
            }

            // Add a couple more children to the block and verify count.
            let child_block2_index = state.create_node("child2".into(), block_index).unwrap();
            let child_block3_index = state.create_node("child3".into(), block_index).unwrap();
            assert_eq!(state.get_block::<Node>(block_index).child_count(), 3);

            // Free children and verify count.
            assert!(state.free_value(child_block_index).is_ok());
            assert!(state.free_value(child_block2_index).is_ok());
            assert!(state.free_value(child_block3_index).is_ok());
            assert_eq!(state.get_block::<Node>(block_index).child_count(), 0);

            // Free node.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_int_metric() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index = state.create_int_metric("test".into(), 3, 0.into()).unwrap();
            let block = state.get_block::<Int>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::IntValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(block.value(), 3);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(*block.parent_index(), 0);

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::IntValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(3));

        {
            let mut state = core_state.try_lock().expect("lock state");
            assert_eq!(state.add_int_metric(block_index, 10), 13);
            assert_eq!(state.get_block::<Int>(block_index).value(), 13);

            assert_eq!(state.subtract_int_metric(block_index, 5), 8);
            assert_eq!(state.get_block::<Int>(block_index).value(), 8);

            state.set_int_metric(block_index, -6);
            assert_eq!(state.get_block::<Int>(block_index).value(), -6);

            assert_eq!(state.subtract_int_metric(block_index, i64::MAX), i64::MIN);
            assert_eq!(state.get_block::<Int>(block_index).value(), i64::MIN);
            state.set_int_metric(block_index, i64::MAX);

            assert_eq!(state.add_int_metric(block_index, 2), i64::MAX);
            assert_eq!(state.get_block::<Int>(block_index).value(), i64::MAX);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_uint_metric() {
        let core_state = get_state(4096);

        // Creates with value
        let block_index = {
            let mut state = core_state.try_lock().expect("try lock");
            let block_index = state.create_uint_metric("test".into(), 3, 0.into()).unwrap();
            let block = state.get_block::<Uint>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::UintValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(block.value(), 3);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(*block.parent_index(), 0);

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::UintValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(3));

        {
            let mut state = core_state.try_lock().expect("try lock");
            assert_eq!(state.add_uint_metric(block_index, 10), 13);
            assert_eq!(state.get_block::<Uint>(block_index).value(), 13);

            assert_eq!(state.subtract_uint_metric(block_index, 5), 8);
            assert_eq!(state.get_block::<Uint>(block_index).value(), 8);

            state.set_uint_metric(block_index, 0);
            assert_eq!(state.get_block::<Uint>(block_index).value(), 0);

            assert_eq!(state.subtract_uint_metric(block_index, u64::MAX), 0);
            assert_eq!(state.get_block::<Uint>(block_index).value(), 0);

            state.set_uint_metric(block_index, 3);
            assert_eq!(state.add_uint_metric(block_index, u64::MAX), u64::MAX);
            assert_eq!(state.get_block::<Uint>(block_index).value(), u64::MAX);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_double_metric() {
        let core_state = get_state(4096);

        // Creates with value
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index = state.create_double_metric("test".into(), 3.0, 0.into()).unwrap();
            let block = state.get_block::<Double>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::DoubleValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(block.value(), 3.0);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(*block.parent_index(), 0);

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::DoubleValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(3));

        {
            let mut state = core_state.try_lock().expect("lock state");
            assert_eq!(state.add_double_metric(block_index, 10.5), 13.5);
            assert_eq!(state.get_block::<Double>(block_index).value(), 13.5);

            assert_eq!(state.subtract_double_metric(block_index, 5.1), 8.4);
            assert_eq!(state.get_block::<Double>(block_index).value(), 8.4);

            state.set_double_metric(block_index, -6.0);
            assert_eq!(state.get_block::<Double>(block_index).value(), -6.0);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_create_buffer_property_cleanup_on_failure() {
        // this implementation detail is important for the test below to be valid
        assert_eq!(constants::MAX_ORDER_SIZE, 2048);

        let core_state = get_state(5121); // large enough to fit to max size blocks plus 1024
        let mut state = core_state.try_lock().expect("lock state");
        // allocate a max size block and one extent
        let name: StringReference = (0..3000).map(|_| " ").collect::<String>().into();
        // allocate a max size property + at least one extent
        // the extent won't fit into the VMO, causing allocation failure when the property
        // is set
        let payload = [0u8; 4096]; // won't fit into vmo

        // fails because the property is too big, but, allocates the name and should clean it up
        assert!(state.create_buffer_property(name, &payload, 0.into()).is_err());

        drop(state);

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();

        // if cleanup happened correctly, the name + extent and property + extent have been freed
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_string_reference_allocations() {
        let core_state = get_state(4096); // allocates HEADER
        {
            let mut state = core_state.try_lock().expect("lock state");
            let sf = "a reference-counted canonical name";
            assert_eq!(state.stats().allocated_blocks, 1);

            let mut collected = vec![];
            for _ in 0..100 {
                collected.push(state.create_node(sf.into(), 0.into()).unwrap());
            }

            assert!(state.inner_lock.string_reference_block_indexes.contains_key(sf));

            assert_eq!(state.stats().allocated_blocks, 102);
            let block = state.get_block::<Node>(collected[0]);
            let sf_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(sf_block.reference_count(), 100);

            collected.into_iter().for_each(|b| {
                assert!(state.inner_lock.string_reference_block_indexes.contains_key(sf));
                assert!(state.free_value(b).is_ok())
            });

            assert!(!state.inner_lock.string_reference_block_indexes.contains_key(sf));

            let node_index = state.create_node(sf.into(), 0.into()).unwrap();
            assert!(state.inner_lock.string_reference_block_indexes.contains_key(sf));
            assert!(state.free_value(node_index).is_ok());
            assert!(!state.inner_lock.string_reference_block_indexes.contains_key(sf));
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_string_reference_data() {
        let core_state = get_state(4096); // allocates HEADER
        let mut state = core_state.try_lock().expect("lock state");

        // 4 bytes (4 ASCII characters in UTF-8) will fit inlined with a minimum block size
        let block_index = state.inner_lock.get_or_create_string_reference("abcd".into()).unwrap();
        let block = state.get_block::<StringRef>(block_index);
        assert_eq!(block.block_type(), Some(BlockType::StringReference));
        assert_eq!(block.order(), 0);
        assert_eq!(state.stats().allocated_blocks, 2);
        assert_eq!(state.stats().deallocated_blocks, 0);
        assert_eq!(block.reference_count(), 0);
        assert_eq!(block.total_length(), 4);
        assert_eq!(*block.next_extent(), 0);
        assert_eq!(block.order(), 0);
        assert_eq!(state.load_string(block.index()).unwrap(), "abcd");

        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
        assert_eq!(state.stats().deallocated_blocks, 1);

        let block_index = state.inner_lock.get_or_create_string_reference("longer".into()).unwrap();
        let block = state.get_block::<StringRef>(block_index);
        assert_eq!(block.block_type(), Some(BlockType::StringReference));
        assert_eq!(block.order(), 1);
        assert_eq!(block.reference_count(), 0);
        assert_eq!(block.total_length(), 6);
        assert_eq!(state.stats().allocated_blocks, 3);
        assert_eq!(state.stats().deallocated_blocks, 1);
        assert_eq!(state.load_string(block.index()).unwrap(), "longer");

        let idx = block.next_extent();
        assert_eq!(*idx, 0);

        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
        assert_eq!(state.stats().deallocated_blocks, 2);

        let block_index = state.inner_lock.get_or_create_string_reference("longer".into()).unwrap();
        let mut block = state.get_block_mut::<StringRef>(block_index);
        assert_eq!(block.order(), 1);
        block.increment_ref_count().unwrap();
        // not an error to try and free
        assert!(state.inner_lock.maybe_free_string_reference(block_index).is_ok());

        let mut block = state.get_block_mut(block_index);
        block.decrement_ref_count().unwrap();
        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
    }

    #[fuchsia::test]
    fn test_string_reference_format_property() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Creates with value
            let block_index = state
                .create_string("test".into(), "test-property".into(), BlockIndex::from(0))
                .unwrap();
            let block = state.get_block::<Buffer>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::BufferValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(block.total_length(), 0);
            assert_eq!(block.format(), Some(PropertyFormat::StringReference));

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let data_block = state.get_block::<StringRef>(block.extent_index());
            assert_eq!(data_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(state.load_string(data_block.index()).unwrap(), "test-property");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::BufferValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks[3].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(4));

        {
            let mut state = core_state.try_lock().expect("lock state");
            // Free property.
            assert!(state.free_string_or_bytes_buffer_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_string_arrays() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 4, 0.into()).unwrap();
            assert_eq!(state.set_array_string_slot(array_index, 0, "0".into()), Ok(()));
            assert_eq!(state.set_array_string_slot(array_index, 1, "1".into()), Ok(()));
            assert_eq!(state.set_array_string_slot(array_index, 2, "2".into()), Ok(()));
            assert_eq!(state.set_array_string_slot(array_index, 3, "3".into()), Ok(()));

            // size is 4
            assert_matches!(
                state.set_array_string_slot(array_index, 4, "".into()),
                Err(Error::VmoFormat(FormatError::ArrayIndexOutOfBounds(4)))
            );
            assert_matches!(
                state.set_array_string_slot(array_index, 5, "".into()),
                Err(Error::VmoFormat(FormatError::ArrayIndexOutOfBounds(5)))
            );

            for i in 0..4 {
                let idx = state
                    .get_block::<Array<StringRef>>(array_index)
                    .get_string_index_at(i)
                    .unwrap();
                assert_eq!(i.to_string(), state.load_string(idx).unwrap());
            }

            assert_eq!(
                state.get_block::<Array<StringRef>>(array_index).get_string_index_at(4),
                None
            );
            assert_eq!(
                state.get_block::<Array<StringRef>>(array_index).get_string_index_at(5),
                None
            );

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn update_string_array_value() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 2, 0.into()).unwrap();

            assert_eq!(state.set_array_string_slot(array_index, 0, "abc".into()), Ok(()));
            assert_eq!(state.set_array_string_slot(array_index, 1, "def".into()), Ok(()));

            assert_eq!(state.set_array_string_slot(array_index, 0, "cba".into()), Ok(()));
            assert_eq!(state.set_array_string_slot(array_index, 1, "fed".into()), Ok(()));

            let cba_index_slot =
                state.get_block::<Array<StringRef>>(array_index).get_string_index_at(0).unwrap();
            let fed_index_slot =
                state.get_block::<Array<StringRef>>(array_index).get_string_index_at(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap());
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        blocks[1..].iter().enumerate().for_each(|(i, b)| {
            assert!(b.block_type() == Some(BlockType::Free), "index is {}", i + 1);
        });
    }

    #[fuchsia::test]
    fn set_string_reference_instances_multiple_times_in_array() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 2, 0.into()).unwrap();

            let abc = StringReference::from("abc");
            let def = StringReference::from("def");
            let cba = StringReference::from("cba");
            let fed = StringReference::from("fed");

            state.set_array_string_slot(array_index, 0, abc.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, def.clone()).unwrap();
            state.set_array_string_slot(array_index, 0, abc.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, def.clone()).unwrap();

            let abc_index_slot = state.get_block(array_index).get_string_index_at(0).unwrap();
            let def_index_slot = state.get_block(array_index).get_string_index_at(1).unwrap();
            assert_eq!("abc".to_string(), state.load_string(abc_index_slot).unwrap(),);
            assert_eq!("def".to_string(), state.load_string(def_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, cba.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, fed.clone()).unwrap();

            let cba_index_slot = state.get_block(array_index).get_string_index_at(0).unwrap();
            let fed_index_slot = state.get_block(array_index).get_string_index_at(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap(),);
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, abc.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, def.clone()).unwrap();

            let abc_index_slot = state.get_block(array_index).get_string_index_at(0).unwrap();
            let def_index_slot = state.get_block(array_index).get_string_index_at(1).unwrap();
            assert_eq!("abc".to_string(), state.load_string(abc_index_slot).unwrap(),);
            assert_eq!("def".to_string(), state.load_string(def_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, cba.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, fed.clone()).unwrap();

            let cba_index_slot = state.get_block(array_index).get_string_index_at(0).unwrap();
            let fed_index_slot = state.get_block(array_index).get_string_index_at(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap(),);
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        blocks[1..].iter().enumerate().for_each(|(i, b)| {
            assert!(b.block_type() == Some(BlockType::Free), "index is {}", i + 1);
        });
    }

    #[fuchsia::test]
    fn test_empty_value_string_arrays() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 4, 0.into()).unwrap();

            state.set_array_string_slot(array_index, 0, "".into()).unwrap();
            state.set_array_string_slot(array_index, 1, "".into()).unwrap();
            state.set_array_string_slot(array_index, 2, "".into()).unwrap();
            state.set_array_string_slot(array_index, 3, "".into()).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let state = core_state.try_lock().expect("lock state");

        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        for b in blocks {
            if b.block_type() == Some(BlockType::StringReference)
                && state.load_string(b.index()).unwrap() == "array"
            {
                continue;
            }

            assert_ne!(
                b.block_type(),
                Some(BlockType::StringReference),
                "Got unexpected StringReference, index: {}, value (wrapped in single quotes): '{:?}'",
                b.index(),
                b.block_type()
            );
        }
    }

    #[fuchsia::test]
    fn test_bytevector_property() {
        let core_state = get_state(4096);

        // Creates with value
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index =
                state.create_buffer_property("test".into(), b"test-property", 0.into()).unwrap();
            let block = state.get_block::<Buffer>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::BufferValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(block.total_length(), 13);
            assert_eq!(*block.extent_index(), 4);
            assert_eq!(block.format(), Some(PropertyFormat::Bytes));

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let extent_block = state.get_block::<Extent>(4.into());
            assert_eq!(extent_block.block_type(), Some(BlockType::Extent));
            assert_eq!(*extent_block.next_extent(), 0);
            assert_eq!(
                std::str::from_utf8(extent_block.contents().unwrap()).unwrap(),
                "test-property\0\0\0\0\0\0\0\0\0\0\0"
            );
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::BufferValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks[3].block_type(), Some(BlockType::Extent));
        assert_all_free(blocks.into_iter().skip(4));

        // Free property.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_string_or_bytes_buffer_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_bool() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Creates with value
            let block_index = state.create_bool("test".into(), true, 0.into()).unwrap();
            let block = state.get_block::<Bool>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::BoolValue));
            assert_eq!(*block.index(), 2);
            assert!(block.value());
            assert_eq!(*block.name_index(), 3);
            assert_eq!(*block.parent_index(), 0);

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::BoolValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(3));

        // Free metric.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_int_array() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index =
                state.create_int_array("test".into(), 5, ArrayFormat::Default, 0.into()).unwrap();
            let block = state.get_block::<Array<Int>>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::ArrayValue));
            assert_eq!(block.order(), 2);
            assert_eq!(*block.index(), 4);
            assert_eq!(*block.name_index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(block.slots(), 5);
            assert_eq!(block.format(), Some(ArrayFormat::Default));
            assert_eq!(block.entry_type(), Some(BlockType::IntValue));

            let name_block = state.get_block::<StringRef>(BlockIndex::from(2));
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            for i in 0..5 {
                state.set_array_int_slot(block_index, i, 3 * i as i64);
            }
            for i in 0..5 {
                assert_eq!(state.get_block::<Array<Int>>(block_index).get(i), Some(3 * i as i64));
            }
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks[2].block_type(), Some(BlockType::Free));
        assert_eq!(blocks[3].block_type(), Some(BlockType::ArrayValue));
        assert_all_free(blocks.into_iter().skip(4));

        // Free the array.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_write_extent_overflow() {
        const SIZE: usize = constants::MAX_ORDER_SIZE * 2;
        const EXPECTED_WRITTEN: usize = constants::MAX_ORDER_SIZE - constants::HEADER_SIZE_BYTES;
        const TRIED_TO_WRITE: usize = SIZE + 1;
        let core_state = get_state(SIZE);
        let (_, written) = core_state
            .try_lock()
            .unwrap()
            .inner_lock
            .write_extents(&[4u8; TRIED_TO_WRITE])
            .unwrap();
        assert_eq!(written, EXPECTED_WRITTEN);
    }

    #[fuchsia::test]
    fn overflow_property() {
        const SIZE: usize = constants::MAX_ORDER_SIZE * 2;
        const EXPECTED_WRITTEN: usize = constants::MAX_ORDER_SIZE - constants::HEADER_SIZE_BYTES;

        let core_state = get_state(SIZE);
        let mut state = core_state.try_lock().expect("lock state");

        let data = "X".repeat(SIZE * 2);
        let block_index =
            state.create_buffer_property("test".into(), data.as_bytes(), 0.into()).unwrap();
        let block = state.get_block::<Buffer>(block_index);
        assert_eq!(block.block_type(), Some(BlockType::BufferValue));
        assert_eq!(*block.index(), 2);
        assert_eq!(*block.parent_index(), 0);
        assert_eq!(*block.name_index(), 3);
        assert_eq!(block.total_length(), EXPECTED_WRITTEN);
        assert_eq!(*block.extent_index(), 128);
        assert_eq!(block.format(), Some(PropertyFormat::Bytes));

        let name_block = state.get_block::<StringRef>(block.name_index());
        assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
        assert_eq!(name_block.total_length(), 4);
        assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

        let extent_block = state.get_block::<Extent>(128.into());
        assert_eq!(extent_block.block_type(), Some(BlockType::Extent));
        assert_eq!(extent_block.order(), 7);
        assert_eq!(*extent_block.next_extent(), *BlockIndex::EMPTY);
        assert_eq!(
            extent_block.contents().unwrap(),
            data.chars().take(EXPECTED_WRITTEN).map(|c| c as u8).collect::<Vec<u8>>()
        );
    }

    #[fuchsia::test]
    fn test_multi_extent_property() {
        let core_state = get_state(10000);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            let chars = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];
            let data = chars.iter().cycle().take(6000).collect::<String>();
            let block_index =
                state.create_buffer_property("test".into(), data.as_bytes(), 0.into()).unwrap();
            let block = state.get_block::<Buffer>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::BufferValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(block.total_length(), 6000);
            assert_eq!(*block.extent_index(), 128);
            assert_eq!(block.format(), Some(PropertyFormat::Bytes));

            let name_block = state.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let extent_block = state.get_block::<Extent>(128.into());
            assert_eq!(extent_block.block_type(), Some(BlockType::Extent));
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent(), 256);
            assert_eq!(
                extent_block.contents().unwrap(),
                chars.iter().cycle().take(2040).map(|&c| c as u8).collect::<Vec<u8>>()
            );

            let extent_block = state.get_block::<Extent>(256.into());
            assert_eq!(extent_block.block_type(), Some(BlockType::Extent));
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent(), 384);
            assert_eq!(
                extent_block.contents().unwrap(),
                chars.iter().cycle().skip(2040).take(2040).map(|&c| c as u8).collect::<Vec<u8>>()
            );

            let extent_block = state.get_block::<Extent>(384.into());
            assert_eq!(extent_block.block_type(), Some(BlockType::Extent));
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent(), 0);
            assert_eq!(
                extent_block.contents().unwrap()[..1920],
                chars.iter().cycle().skip(4080).take(1920).map(|&c| c as u8).collect::<Vec<u8>>()[..]
            );
            assert_eq!(extent_block.contents().unwrap()[1920..], [0u8; 120][..]);
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 11);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::BufferValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks[8].block_type(), Some(BlockType::Extent));
        assert_eq!(blocks[9].block_type(), Some(BlockType::Extent));
        assert_eq!(blocks[10].block_type(), Some(BlockType::Extent));
        assert_all_free(blocks.into_iter().skip(3).take(5));
        // Free property.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_string_or_bytes_buffer_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_freeing_string_references() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert_eq!(state.stats().allocated_blocks, 1);

            let block0_index = state.create_node("abcd123456789".into(), 0.into()).unwrap();
            let block0_name_index = {
                let block0_name_index = state.get_block::<Node>(block0_index).name_index();
                let block0_name = state.get_block::<StringRef>(block0_name_index);
                assert_eq!(block0_name.order(), 1);
                block0_name_index
            };
            assert_eq!(state.stats().allocated_blocks, 3);

            let block1_index =
                state.inner_lock.get_or_create_string_reference("abcd".into()).unwrap();
            assert_eq!(state.stats().allocated_blocks, 4);
            assert_eq!(state.get_block::<StringRef>(block1_index).order(), 0);

            // no allocation!
            let block2_index =
                state.inner_lock.get_or_create_string_reference("abcd123456789".into()).unwrap();
            assert_eq!(state.get_block::<StringRef>(block2_index).order(), 1);
            assert_eq!(block0_name_index, block2_index);
            assert_eq!(state.stats().allocated_blocks, 4);

            let block3_index = state.create_node("abcd12345678".into(), 0.into()).unwrap();
            let block3 = state.get_block::<Node>(block3_index);
            let block3_name = state.get_block::<StringRef>(block3.name_index());
            assert_eq!(block3_name.order(), 1);
            assert_eq!(block3.order(), 0);
            assert_eq!(state.stats().allocated_blocks, 6);

            let mut long_name = "".to_string();
            for _ in 0..3000 {
                long_name += " ";
            }

            let block4_index = state.create_node(long_name.into(), 0.into()).unwrap();
            let block4 = state.get_block::<Node>(block4_index);
            let block4_name = state.get_block::<StringRef>(block4.name_index());
            assert_eq!(block4_name.order(), 7);
            assert!(*block4_name.next_extent() != 0);
            assert_eq!(state.stats().allocated_blocks, 9);

            assert!(state.inner_lock.maybe_free_string_reference(block1_index).is_ok());
            assert_eq!(state.stats().deallocated_blocks, 1);
            assert!(state.inner_lock.maybe_free_string_reference(block2_index).is_ok());
            // no deallocation because same ref as block2 is held in block0_name
            assert_eq!(state.stats().deallocated_blocks, 1);
            assert!(state.free_value(block3_index).is_ok());
            assert_eq!(state.stats().deallocated_blocks, 3);
            assert!(state.free_value(block4_index).is_ok());
            assert_eq!(state.stats().deallocated_blocks, 6);
        }

        // Current expected layout of VMO:
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();

        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::NodeValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::Free));
        assert_eq!(blocks[3].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(4));
    }

    #[fuchsia::test]
    fn test_tombstone() {
        let core_state = get_state(4096);
        let child_block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Create a node value and verify its fields
            let block_index = state.create_node("root-node".into(), 0.into()).unwrap();
            let block_name_as_string_ref =
                state.get_block::<StringRef>(state.get_block::<Node>(block_index).name_index());
            assert_eq!(block_name_as_string_ref.order(), 1);
            assert_eq!(state.stats().allocated_blocks, 3);
            assert_eq!(state.stats().deallocated_blocks, 0);

            let child_block_index = state.create_node("child-node".into(), block_index).unwrap();
            assert_eq!(state.stats().allocated_blocks, 5);
            assert_eq!(state.stats().deallocated_blocks, 0);

            // Node still has children, so will become a tombstone.
            assert!(state.free_value(block_index).is_ok());
            assert_eq!(state.stats().allocated_blocks, 5);
            assert_eq!(state.stats().deallocated_blocks, 1);
            child_block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();

        // Note that the way Extents get allocated means that they aren't necessarily
        // put in the buffer where it would seem they should based on the literal order of allocation.
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::Tombstone));
        assert_eq!(blocks[2].block_type(), Some(BlockType::NodeValue));
        assert_eq!(blocks[3].block_type(), Some(BlockType::Free));
        assert_eq!(blocks[4].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(5));

        // Freeing the child, causes all blocks to be freed.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(child_block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_with_header_lock() {
        let state = get_state(4096);
        // Initial generation count is 0
        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 0);
        });

        // Lock the state
        let mut lock_guard = state.try_lock().expect("lock state");
        assert!(lock_guard.header().is_locked());
        assert_eq!(lock_guard.header().generation_count(), 1);
        // Operations on the lock guard do not change the generation counter.
        let _ = lock_guard.create_node("test".into(), 0.into()).unwrap();
        let _ = lock_guard.create_node("test2".into(), 2.into()).unwrap();
        assert_eq!(lock_guard.header().generation_count(), 1);

        // Dropping the guard releases the lock.
        drop(lock_guard);
        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 2);
            assert!(!header.is_locked());
        });
    }

    #[fuchsia::test]
    async fn test_link() {
        // Initialize state and create a link block.
        let state = get_state(4096);
        let block_index = {
            let mut state_guard = state.try_lock().expect("lock state");
            let block_index = state_guard
                .create_lazy_node("link-name".into(), 0.into(), LinkNodeDisposition::Inline, || {
                    async move {
                        let inspector = Inspector::default();
                        inspector.root().record_uint("a", 1);
                        Ok(inspector)
                    }
                    .boxed()
                })
                .unwrap();

            // Verify the callback was properly saved.
            assert!(state_guard.callbacks().get("link-name-0").is_some());
            let callback = state_guard.callbacks().get("link-name-0").unwrap();
            match callback().await {
                Ok(inspector) => {
                    let hierarchy =
                        PartialNodeHierarchy::try_from(Snapshot::try_from(&inspector).unwrap())
                            .unwrap();
                    assert_data_tree!(hierarchy, root: {
                        a: 1u64,
                    });
                }
                Err(_) => unreachable!("we never return errors in the callback"),
            }

            // Verify link block.
            let block = state_guard.get_block::<Link>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::LinkValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(*block.name_index(), 4);
            assert_eq!(*block.content_index(), 6);
            assert_eq!(block.link_node_disposition(), Some(LinkNodeDisposition::Inline));

            // Verify link's name block.
            let name_block = state_guard.get_block::<StringRef>(block.name_index());
            assert_eq!(name_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(name_block.total_length(), 9);
            assert_eq!(state_guard.load_string(name_block.index()).unwrap(), "link-name");

            // Verify link's content block.
            let content_block = state_guard.get_block::<StringRef>(block.content_index());
            assert_eq!(content_block.block_type(), Some(BlockType::StringReference));
            assert_eq!(content_block.total_length(), 11);
            assert_eq!(state_guard.load_string(content_block.index()).unwrap(), "link-name-0");
            block_index
        };

        // Verify all the VMO blocks.
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::LinkValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::Free));
        assert_eq!(blocks[3].block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks[4].block_type(), Some(BlockType::StringReference));
        assert_all_free(blocks.into_iter().skip(5));

        // Free link
        {
            let mut state_guard = state.try_lock().expect("lock state");
            assert!(state_guard.free_lazy_node(block_index).is_ok());

            // Verify the callback was cleared on free link.
            assert!(state_guard.callbacks().get("link-name-0").is_none());
        }
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));

        // Verify adding another link generates a different ID regardless of the params.
        let mut state_guard = state.try_lock().expect("lock state");
        state_guard
            .create_lazy_node("link-name".into(), 0.into(), LinkNodeDisposition::Inline, || {
                async move { Ok(Inspector::default()) }.boxed()
            })
            .unwrap();
        let content_block = state_guard.get_block::<StringRef>(6.into());
        assert_eq!(state_guard.load_string(content_block.index()).unwrap(), "link-name-1");
    }

    #[fuchsia::test]
    fn free_lazy_node_test() {
        let state = get_state(4096);
        let (lazy_index, _int_with_magic_name_index) = {
            let mut state_guard = state.try_lock().expect("lock state");
            let lazy_index = state_guard
                .create_lazy_node("lk".into(), 0.into(), LinkNodeDisposition::Inline, || {
                    async move { Ok(Inspector::default()) }.boxed()
                })
                .unwrap();

            let magic_link_name = StringReference::from("lk-0");
            let int_with_magic_name_index =
                state_guard.create_int_metric(magic_link_name, 0, BlockIndex::from(0)).unwrap();

            (lazy_index, int_with_magic_name_index)
        };

        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let mut blocks = snapshot.scan();
        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::Header));

        let block = blocks.next().and_then(|b| b.cast::<Link>()).unwrap();
        assert_eq!(block.block_type(), Some(BlockType::LinkValue));
        assert_eq!(state.try_lock().unwrap().load_string(block.name_index()).unwrap(), "lk");
        assert_eq!(state.try_lock().unwrap().load_string(block.content_index()).unwrap(), "lk-0");
        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::StringReference));
        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::StringReference));
        let block = blocks.next().and_then(|b| b.cast::<Int>()).unwrap();
        assert_eq!(block.block_type(), Some(BlockType::IntValue));
        assert_eq!(state.try_lock().unwrap().load_string(block.name_index()).unwrap(), "lk-0");
        assert_all_free(blocks);

        state.try_lock().unwrap().free_lazy_node(lazy_index).unwrap();

        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let mut blocks = snapshot.scan();

        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::Header));
        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::Free));
        assert_eq!(blocks.next().unwrap().block_type(), Some(BlockType::StringReference));
        let block = blocks.next().and_then(|b| b.cast::<Int>()).unwrap();
        assert_eq!(block.block_type(), Some(BlockType::IntValue));
        assert_eq!(state.try_lock().unwrap().load_string(block.name_index()).unwrap(), "lk-0");
        assert_all_free(blocks);
    }

    #[fuchsia::test]
    async fn stats() {
        // Initialize state and create a link block.
        let state = get_state(3 * 4096);
        let mut state_guard = state.try_lock().expect("lock state");
        let _block1 = state_guard
            .create_lazy_node("link-name".into(), 0.into(), LinkNodeDisposition::Inline, || {
                async move {
                    let inspector = Inspector::default();
                    inspector.root().record_uint("a", 1);
                    Ok(inspector)
                }
                .boxed()
            })
            .unwrap();
        let _block2 = state_guard.create_uint_metric("test".into(), 3, 0.into()).unwrap();
        assert_eq!(
            state_guard.stats(),
            Stats {
                total_dynamic_children: 1,
                maximum_size: 3 * 4096,
                current_size: 4096,
                allocated_blocks: 6, /* HEADER, state_guard, _block1 (and content),
                                     // "link-name", _block2, "test" */
                deallocated_blocks: 0,
                failed_allocations: 0,
            }
        )
    }

    #[fuchsia::test]
    fn transaction_locking() {
        let state = get_state(4096);
        // Initial generation count is 0
        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 0);
        });

        // Begin a transaction
        state.begin_transaction();
        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 1);
            assert!(header.is_locked());
        });

        // Operations on the lock  guard do not change the generation counter.
        let mut lock_guard1 = state.try_lock().expect("lock state");
        assert_eq!(lock_guard1.inner_lock.transaction_count, 1);
        assert_eq!(lock_guard1.header().generation_count(), 1);
        assert!(lock_guard1.header().is_locked());
        let _ = lock_guard1.create_node("test".into(), 0.into());
        assert_eq!(lock_guard1.inner_lock.transaction_count, 1);
        assert_eq!(lock_guard1.header().generation_count(), 1);

        // Dropping the guard releases the mutex lock but the header remains locked.
        drop(lock_guard1);
        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 1);
            assert!(header.is_locked());
        });

        // When the transaction finishes, the header is unlocked.
        state.end_transaction();

        state.with_current_header(|header| {
            assert_eq!(header.generation_count(), 2);
            assert!(!header.is_locked());
        });

        // Operations under no transaction work as usual.
        let lock_guard2 = state.try_lock().expect("lock state");
        assert!(lock_guard2.header().is_locked());
        assert_eq!(lock_guard2.header().generation_count(), 3);
        assert_eq!(lock_guard2.inner_lock.transaction_count, 0);
    }

    #[fuchsia::test]
    async fn update_header_vmo_size() {
        let core_state = get_state(3 * 4096);
        core_state.get_block(BlockIndex::HEADER, |header: &Block<_, Header>| {
            assert_eq!(header.vmo_size(), Ok(Some(4096)));
        });
        let block1_index = {
            let mut state = core_state.try_lock().expect("lock state");

            let chars = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];
            let data = chars.iter().cycle().take(6000).collect::<String>();
            let block_index =
                state.create_buffer_property("test".into(), data.as_bytes(), 0.into()).unwrap();
            assert_eq!(state.header().vmo_size(), Ok(Some(2 * 4096)));

            block_index
        };

        let block2_index = {
            let mut state = core_state.try_lock().expect("lock state");

            let chars = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];
            let data = chars.iter().cycle().take(3000).collect::<String>();
            let block_index =
                state.create_buffer_property("test".into(), data.as_bytes(), 0.into()).unwrap();
            assert_eq!(state.header().vmo_size(), Ok(Some(3 * 4096)));

            block_index
        };
        // Free properties.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_string_or_bytes_buffer_property(block1_index).is_ok());
            assert!(state.free_string_or_bytes_buffer_property(block2_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_all_free(blocks.into_iter().skip(1));
    }

    #[fuchsia::test]
    fn test_buffer_property_on_overflow_set() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Create string property with value.
            let block_index =
                state.create_buffer_property("test".into(), b"test-property", 0.into()).unwrap();

            // Fill the vmo.
            for _ in 10..(4096 / constants::MIN_ORDER_SIZE).try_into().unwrap() {
                state.inner_lock.heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
            }

            // Set the value of the string to something very large that causes an overflow.
            let values = [b'a'; 8096];
            assert!(state.set_buffer_property(block_index, &values).is_err());

            // We now expect the length of the payload, as well as the property extent index to be
            // reset.
            let block = state.get_block::<Buffer>(block_index);
            assert_eq!(block.block_type(), Some(BlockType::BufferValue));
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index(), 0);
            assert_eq!(*block.name_index(), 3);
            assert_eq!(block.total_length(), 0);
            assert_eq!(*block.extent_index(), 0);
            assert_eq!(block.format(), Some(PropertyFormat::Bytes));

            block_index
        };

        // We also expect no extents to be present.
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 251);
        assert_eq!(blocks[0].block_type(), Some(BlockType::Header));
        assert_eq!(blocks[1].block_type(), Some(BlockType::BufferValue));
        assert_eq!(blocks[2].block_type(), Some(BlockType::StringReference));
        assert!(blocks[3..].iter().all(|b| b.block_type() == Some(BlockType::Reserved)
            || b.block_type() == Some(BlockType::Free)));

        {
            let mut state = core_state.try_lock().expect("lock state");
            // Free property.
            assert_matches!(state.free_string_or_bytes_buffer_property(block_index), Ok(()));
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_, Unknown>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == Some(BlockType::Reserved)
            || b.block_type() == Some(BlockType::Free)));
    }
}
