// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::writer::{error::Error, heap::Heap, Inspector, StringReference},
    anyhow,
    derivative::Derivative,
    futures::future::BoxFuture,
    inspect_format::{
        constants, utils, BlockAccessorExt, BlockAccessorMutExt, BlockContainer, BlockIndex,
        BlockType, Container, Error as FormatError,
        {ArrayFormat, Block, LinkNodeDisposition, PropertyFormat},
    },
    parking_lot::{Mutex, MutexGuard},
    std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
    },
    tracing::error,
};

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
        self.checked_add(other).unwrap_or(std::u64::MAX)
    }
}

impl SafeOp for i64 {
    fn safe_sub(&self, other: i64) -> i64 {
        self.checked_sub(other).unwrap_or(std::i64::MIN)
    }
    fn safe_add(&self, other: i64) -> i64 {
        self.checked_add(other).unwrap_or(std::i64::MAX)
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

            pub fn [<set_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                self.inner_lock.[<set_ $name _metric>](block_index, value)
            }

            pub fn [<add_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                self.inner_lock.[<add_ $name _metric>](block_index, value)
            }

            pub fn [<subtract_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                self.inner_lock.[<subtract_ $name _metric>](block_index, value)
            }

            pub fn [<get_ $name _metric>](&self, block_index: BlockIndex) -> Result<$type, Error> {
                self.inner_lock.[<get_ $name _metric>](block_index)
            }
        }
    };
}

/// Generate create, set, add and subtract methods for a metric.
macro_rules! metric_fns {
    ($name:ident, $type:ident) => {
        paste::paste! {
            fn [<create_ $name _metric>](
                &mut self,
                name: StringReference,
                value: $type,
                parent_index: BlockIndex,
            ) -> Result<BlockIndex, Error> {
                let (block_index, name_block_index) = self.allocate_reserved_value(
                    name, parent_index, constants::MIN_ORDER_SIZE)?;
                self.heap.container.block_at_mut(block_index)
                    .[<become_ $name _value>](value, name_block_index, parent_index)?;
                Ok(block_index)
            }

            fn [<set_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                block.[<set_ $name _value>](value)?;
                Ok(())
            }

            fn [<add_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                let current_value = block.[<$name _value>]()?;
                block.[<set_ $name _value>](current_value.safe_add(value))?;
                Ok(())
            }

            fn [<subtract_ $name _metric>](&mut self, block_index: BlockIndex, value: $type)
                -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                let current_value = block.[<$name _value>]()?;
                let new_value = current_value.safe_sub(value);
                block.[<set_ $name _value>](new_value)?;
                Ok(())
            }

            fn [<get_ $name _metric>](&self, block_index: BlockIndex) -> Result<$type, Error> {
                let block = self.heap.container.block_at(block_index);
                let current_value = block.[<$name _value>]()?;
                Ok(current_value)
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
            ) -> Result<(), Error> {
                self.inner_lock.[<set_array_ $name _slot>](block_index, slot_index, value)
            }

            pub fn [<add_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Result<(), Error> {
                self.inner_lock.[<add_array_ $name _slot>](block_index, slot_index, value)
            }

            pub fn [<subtract_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Result<(), Error> {
                self.inner_lock.[<subtract_array_ $name _slot>](block_index, slot_index, value)
            }
        }
    };
}

macro_rules! arithmetic_array_fns {
    ($name:ident, $type:ident, $value:ident) => {
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
                self.heap.container.block_at_mut(block_index).become_array_value(
                    slots, array_format, BlockType::$value, name_block_index, parent_index)?;
                Ok(block_index)
            }

            pub fn [<set_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                block.[<array_set_ $name _slot>](slot_index, value)?;
                Ok(())
            }

            pub fn [<add_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                let previous_value = block.[<array_get_ $name _slot>](slot_index)?;
                let new_value = previous_value.safe_add(value);
                block.[<array_set_ $name _slot>](slot_index, new_value)?;
                Ok(())
            }

            pub fn [<subtract_array_ $name _slot>](
                &mut self, block_index: BlockIndex, slot_index: usize, value: $type
            ) -> Result<(), Error> {
                let mut block = self.heap.container.block_at_mut(block_index);
                let previous_value = block.[<array_get_ $name _slot>](slot_index)?;
                let new_value = previous_value.safe_sub(value);
                block.[<array_set_ $name _slot>](slot_index, new_value)?;
                Ok(())
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
    // TODO(fxbug.dev/51298): have a single locking mechanism implemented on top of the vmo header.
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
        F: FnOnce(&Block<&Container>) -> R,
    {
        // A lock guard for the test, which doesn't execute its drop impl as well as that would
        // cause changes in the VMO generation count.
        let lock_guard = LockedStateGuard::without_gen_count_changes(self.inner.lock());
        let block = lock_guard.get_block(BlockIndex::HEADER);
        callback(&block)
    }

    pub(crate) fn get_block<F>(&self, index: BlockIndex, callback: F)
    where
        F: FnOnce(&Block<&Container>) -> (),
    {
        let state_lock = self.try_lock().unwrap();
        callback(&state_lock.get_block(index))
    }

    pub(crate) fn get_block_mut<F>(&self, index: BlockIndex, callback: F)
    where
        F: FnOnce(&mut Block<&mut Container>) -> (),
    {
        let mut state_lock = self.try_lock().unwrap();
        callback(&mut state_lock.get_block_mut(index))
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
impl<'a> LockedStateGuard<'a> {
    /// Freezes the VMO, does a CoW duplication, thaws the parent, and returns the child.
    pub fn frozen_vmo_copy(&mut self) -> Result<Option<fuchsia_zircon::Vmo>, Error> {
        self.inner_lock.frozen_vmo_copy()
    }
}

impl<'a> LockedStateGuard<'a> {
    fn new(mut inner_lock: MutexGuard<'a, InnerState>) -> Result<Self, Error> {
        if inner_lock.transaction_count == 0 {
            let mut header = inner_lock.heap.container.block_at_mut(BlockIndex::HEADER);
            header.lock_header()?;
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

    /// Allocate a PROPERTY block with the given |name|, |value| and |parent_index|.
    pub fn create_property(
        &mut self,
        name: StringReference,
        value: &[u8],
        format: PropertyFormat,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_property(name, value, format, parent_index)
    }

    pub fn reparent(
        &mut self,
        being_reparented: BlockIndex,
        new_parent: BlockIndex,
    ) -> Result<(), Error> {
        self.inner_lock.reparent(being_reparented, new_parent)
    }

    /// Free a PROPERTY block.
    pub fn free_property(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.inner_lock.free_property(index)
    }

    /// Set the |value| of a String PROPERTY block.
    pub fn set_property(&mut self, block_index: BlockIndex, value: &[u8]) -> Result<(), Error> {
        self.inner_lock.set_property(block_index, value)
    }

    pub fn create_bool(
        &mut self,
        name: StringReference,
        value: bool,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        self.inner_lock.create_bool(name, value, parent_index)
    }

    pub fn set_bool(&mut self, block_index: BlockIndex, value: bool) -> Result<(), Error> {
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

    pub fn get_array_size(&self, block_index: BlockIndex) -> Result<usize, Error> {
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
            self.inner_lock
                .heap
                .container
                .block_at_mut(BlockIndex::HEADER)
                .unlock_header()
                .unwrap_or_else(|e| {
                    error!(?e, "Failed to unlock header");
                });
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
        self.inner_lock.allocate_link(name, content.into(), disposition, parent_index)
    }

    pub(crate) fn get_block(&self, index: BlockIndex) -> Block<&Container> {
        self.inner_lock.heap.container.block_at(index)
    }

    fn header(&self) -> Block<&Container> {
        self.get_block(BlockIndex::HEADER)
    }

    fn get_block_mut(&mut self, index: BlockIndex) -> Block<&mut Container> {
        self.inner_lock.heap.container.block_at_mut(index)
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
    fn frozen_vmo_copy(&mut self) -> Result<Option<fuchsia_zircon::Vmo>, Error> {
        if self.transaction_count > 0 {
            return Ok(None);
        }

        let mut header = self.heap.container.block_at_mut(BlockIndex::HEADER);
        let old = header.freeze_header()?;
        let ret = self
            .storage
            .create_child(
                fuchsia_zircon::VmoChildOptions::SNAPSHOT
                    | fuchsia_zircon::VmoChildOptions::NO_WRITE,
                0,
                self.storage.get_size().map_err(|status| Error::VmoSize(status))?,
            )
            .ok();
        header.thaw_header(old)?;
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

    fn lock_header(&mut self) {
        if self.transaction_count == 0 {
            let res = self.heap.container.block_at_mut(BlockIndex::HEADER).lock_header();
            debug_assert!(res.is_ok());
        }
        self.transaction_count += 1;
    }

    fn unlock_header(&mut self) {
        self.transaction_count -= 1;
        if self.transaction_count == 0 {
            let res = self.heap.container.block_at_mut(BlockIndex::HEADER).unlock_header();
            debug_assert!(res.is_ok());
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
            .block_at_mut(block_index)
            .become_node(name_block_index, parent_index)?;
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
        let (content_block_index, content_block_type) = {
            let block = self.heap.container.block_at(index);
            (block.link_content_index()?, block.block_type())
        };
        let content = self.load_key_string(content_block_index)?;
        self.delete_value(index)?;
        // Free the name or string reference block used for content.
        match content_block_type {
            BlockType::StringReference => {
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
        let result = self.get_or_create_string_reference(content).and_then(|content_block_index| {
            self.heap
                .container
                .block_at_mut(content_block_index)
                .increment_string_reference_count()?;
            self.heap.container.block_at_mut(value_block_index).become_link(
                name_block_index,
                parent_index,
                content_block_index,
                disposition,
            )?;
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

    /// Allocate a PROPERTY block with the given |name|, |value| and |parent_index|.
    fn create_property(
        &mut self,
        name: StringReference,
        value: &[u8],
        format: PropertyFormat,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, constants::MIN_ORDER_SIZE)?;
        self.heap.container.block_at_mut(block_index).become_property(
            name_block_index,
            parent_index,
            format,
        )?;
        if let Err(err) = self.inner_set_property_value(block_index, &value) {
            self.heap.free_block(block_index)?;
            self.release_string_reference(name_block_index)?;
            return Err(err);
        }
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
                self.heap.container.block_at_mut(block_index).become_string_reference()?;
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
        let (head_extent, bytes_written) = match self
            .inline_string_reference(block_index, value.as_bytes())
        {
            Ok(inlined) if (inlined as usize) < value.len() => {
                let (head, in_extents) = self.write_extents(&value_bytes[inlined as usize..])?;
                (head, inlined + in_extents)
            }
            Ok(inlined) => (BlockIndex::EMPTY, inlined),
            Err(e) => return Err(e),
        };
        let mut block = self.heap.container.block_at_mut(block_index);
        block.set_extent_next_index(head_extent)?;
        block.set_total_length(bytes_written)?;
        Ok(())
    }

    /// Given a string, write the portion that can be inlined to the given block.
    /// Return the number of bytes written.
    fn inline_string_reference(
        &mut self,
        block_index: BlockIndex,
        value: &[u8],
    ) -> Result<u32, Error> {
        // only returns an error if you call with wrong block type
        // Safety: we convert from usize to u32 here, because we know that there are fewer
        // than u32::MAX bytes written inline in even the largest string reference block.
        // This allows safe promotion to usize anywhere necessary later.
        Ok(self.heap.container.block_at_mut(block_index).write_string_reference_inline(value)?
            as u32)
    }

    /// Decrement the reference count on the block and free it if the count is 0.
    /// This is the function to call if you want to give up your hold on a StringReference.
    fn release_string_reference(&mut self, block_index: BlockIndex) -> Result<(), Error> {
        self.heap.container.block_at_mut(block_index).decrement_string_reference_count()?;
        self.maybe_free_string_reference(block_index)
    }

    /// Free a STRING_REFERENCE if the count is 0. This should not be
    /// directly called outside of tests.
    fn maybe_free_string_reference(&mut self, block_index: BlockIndex) -> Result<(), Error> {
        let block = self.heap.container.block_at(block_index);
        if block.string_reference_count()? != 0 {
            return Ok(());
        }
        let first_extent = block.next_extent()?;
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
            BlockType::StringReference => self.read_string_reference(block),
            BlockType::Name => {
                block.name_contents().map(|s| s.to_string()).map_err(|_| Error::NameNotUtf8)
            }
            wrong_type => Err(Error::InvalidBlockType(index, wrong_type)),
        }
    }

    /// Read a StringReference
    fn read_string_reference(&self, block: Block<&Container>) -> Result<String, Error> {
        let mut content = block.inline_string_reference()?.to_vec();
        let mut next = block.next_extent()?;
        while next != BlockIndex::EMPTY {
            let next_block = self.heap.container.block_at(next);
            content.extend_from_slice(&next_block.extent_contents()?);
            next = next_block.next_extent()?;
        }

        content.truncate(block.total_length()?);
        Ok(String::from_utf8(content).ok().ok_or(Error::NameNotUtf8)?)
    }

    /// Free a PROPERTY block.
    fn free_property(&mut self, index: BlockIndex) -> Result<(), Error> {
        let property_extent_index = self.heap.container.block_at(index).property_extent_index()?;
        self.free_extents(property_extent_index)?;
        self.delete_value(index)?;
        Ok(())
    }

    /// Set the |value| of a String PROPERTY block.
    fn set_property(&mut self, block_index: BlockIndex, value: &[u8]) -> Result<(), Error> {
        self.inner_set_property_value(block_index, value)?;
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

            being_checked = self.heap.container.block_at(being_checked).parent_index()?;
        }

        Ok(())
    }

    fn reparent(
        &mut self,
        being_reparented: BlockIndex,
        new_parent: BlockIndex,
    ) -> Result<(), Error> {
        self.check_lineage(being_reparented, new_parent)?;
        let original_parent_idx = self.heap.container.block_at(being_reparented).parent_index()?;
        if original_parent_idx != BlockIndex::ROOT {
            let mut original_parent_block = self.heap.container.block_at_mut(original_parent_idx);
            let child_count = original_parent_block.child_count()? - 1;
            original_parent_block.set_child_count(child_count)?;
        }

        self.heap.container.block_at_mut(being_reparented).set_parent(new_parent)?;

        if new_parent != BlockIndex::ROOT {
            let mut new_parent_block = self.heap.container.block_at_mut(new_parent);
            let child_count = new_parent_block.child_count()? + 1;
            new_parent_block.set_child_count(child_count)?;
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
        self.heap.container.block_at_mut(block_index).become_bool_value(
            value,
            name_block_index,
            parent_index,
        )?;
        Ok(block_index)
    }

    fn set_bool(&mut self, block_index: BlockIndex, value: bool) -> Result<(), Error> {
        let mut block = self.heap.container.block_at_mut(block_index);
        block.set_bool_value(value)?;
        Ok(())
    }

    metric_fns!(int, i64);
    metric_fns!(uint, u64);
    metric_fns!(double, f64);

    arithmetic_array_fns!(int, i64, IntValue);
    arithmetic_array_fns!(uint, u64, UintValue);
    arithmetic_array_fns!(double, f64, DoubleValue);

    fn create_string_array(
        &mut self,
        name: StringReference,
        slots: usize,
        parent_index: BlockIndex,
    ) -> Result<BlockIndex, Error> {
        // Safety: array_element_size will never fail for BlockType::StringReference.
        let block_size = slots as usize * BlockType::StringReference.array_element_size().unwrap()
            + constants::MIN_ORDER_SIZE;
        if block_size > constants::MAX_ORDER_SIZE {
            return Err(Error::BlockSizeTooBig(block_size));
        }
        let (block_index, name_block_index) =
            self.allocate_reserved_value(name, parent_index, block_size)?;
        self.heap.container.block_at_mut(block_index).become_array_value(
            slots,
            ArrayFormat::Default,
            BlockType::StringReference,
            name_block_index,
            parent_index,
        )?;
        Ok(block_index)
    }

    fn get_array_size(&self, block_index: BlockIndex) -> Result<usize, Error> {
        let block = self.heap.container.block_at(block_index);
        block.array_slots().map_err(|e| Error::VmoFormat(e))
    }

    fn set_array_string_slot(
        &mut self,
        block_index: BlockIndex,
        slot_index: usize,
        value: StringReference,
    ) -> Result<(), Error> {
        if self.heap.container.block_at(block_index).array_slots()? <= slot_index {
            return Err(Error::VmoFormat(FormatError::ArrayIndexOutOfBounds(slot_index)));
        }

        let reference_index = if !value.is_empty() {
            let reference_index = self.get_or_create_string_reference(value.into())?;
            self.heap.container.block_at_mut(reference_index).increment_string_reference_count()?;
            reference_index
        } else {
            BlockIndex::EMPTY
        };

        let existing_index =
            self.heap.container.block_at(block_index).array_get_string_index_slot(slot_index)?;
        if existing_index != BlockIndex::EMPTY {
            self.release_string_reference(existing_index)?;
        }

        self.heap
            .container
            .block_at_mut(block_index)
            .array_set_string_slot(slot_index, reference_index)?;
        Ok(())
    }

    /// Sets all slots of the array at the given index to zero.
    /// Does appropriate deallocation on string references in payload.
    fn clear_array(
        &mut self,
        block_index: BlockIndex,
        start_slot_index: usize,
    ) -> Result<(), Error> {
        match self.heap.container.block_at(block_index).array_entry_type()? {
            value if value.is_numeric_value() => {
                self.heap.container.block_at_mut(block_index).array_clear(start_slot_index)?
            }
            BlockType::StringReference => {
                let array_slots = self.heap.container.block_at(block_index).array_slots()?;
                for i in start_slot_index..array_slots {
                    let index = {
                        let mut block = self.heap.container.block_at_mut(block_index);
                        let index = block.array_get_string_index_slot(i)?;
                        if index == BlockIndex::EMPTY {
                            continue;
                        }
                        block.array_set_string_slot(i, BlockIndex::EMPTY)?;
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
                self.heap.container.block_at_mut(b_index).increment_string_reference_count()?;
                b_index
            }
            Err(err) => {
                self.heap.free_block(block_index)?;
                return Err(err);
            }
        };

        let result = {
            let mut parent_block = self.heap.container.block_at_mut(parent_index);
            let parent_block_type = parent_block.block_type();
            match parent_block_type {
                BlockType::NodeValue | BlockType::Tombstone => {
                    // Safety: NodeValues and Tombstones always have child_count
                    parent_block.set_child_count(parent_block.child_count().unwrap() + 1)?;
                    Ok(())
                }
                BlockType::Header => Ok(()),
                _ => Err(Error::InvalidBlockType(parent_index, parent_block_type)),
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
        let block = self.heap.container.block_at(block_index);
        let parent_index = block.parent_index()?;
        let name_index = block.name_index()?;

        // Decrement parent child count.
        if parent_index != BlockIndex::ROOT {
            let mut parent = self.heap.container.block_at_mut(parent_index);
            let child_count = parent.child_count()? - 1;
            if parent.block_type() == BlockType::Tombstone && child_count == 0 {
                drop(parent); // drop exclusive reference.
                self.heap.free_block(parent_index).expect("Failed to free block");
            } else {
                parent.set_child_count(child_count)?;
            }
        }

        // Free the name block.
        match self.heap.container.block_at(name_index).block_type() {
            BlockType::StringReference => {
                self.release_string_reference(name_index)?;
            }
            _ => self.heap.free_block(name_index).expect("Failed to free block"),
        }

        // If the block is a NODE and has children, make it a TOMBSTONE so that
        // it's freed when the last of its children is freed. Otherwise, free it.
        let mut block = self.heap.container.block_at_mut(block_index);
        if block.block_type() == BlockType::NodeValue && block.child_count()? != 0 {
            block.become_tombstone()?;
        } else {
            drop(block); // drop exclusive &mut self reference only.
            self.heap.free_block(block_index)?;
        }
        Ok(())
    }

    fn inner_set_property_value(
        &mut self,
        block_index: BlockIndex,
        value: &[u8],
    ) -> Result<(), Error> {
        self.free_extents(self.heap.container.block_at(block_index).property_extent_index()?)?;
        let (extent_index, written) = self.write_extents(value)?;
        let mut block = self.heap.container.block_at_mut(block_index);
        block.set_total_length(written)?;
        block.set_property_extent_index(extent_index)?;
        Ok(())
    }

    fn free_extents(&mut self, head_extent_index: BlockIndex) -> Result<(), Error> {
        let mut index = head_extent_index;
        while index != BlockIndex::ROOT {
            let next_index = self.heap.container.block_at(index).next_extent()?;
            self.heap.free_block(index)?;
            index = next_index;
        }
        Ok(())
    }

    fn write_extents(&mut self, value: &[u8]) -> Result<(BlockIndex, u32), Error> {
        if value.len() == 0 {
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
                let mut extent_block = self.heap.container.block_at_mut(extent_block_index);
                extent_block.become_extent(BlockIndex::EMPTY)?;
                extent_block.extent_set_contents(&value[offset..])?
            };
            offset += bytes_written;
            if offset < total_size {
                let Ok(block_index) =
                    self.heap.allocate_block(utils::block_size_for_payload(total_size - offset))
                else {
                    // If we fail to allocate, just take what was written already and bail.
                    // Safety: See the below safety comment. In general, if the VMO could be
                    // larger than u32::MAX bytes, but the max size block could not be,
                    // this would still safely truncate the data and be an accurate count of
                    // how many bytes were actually written, because the heap has not
                    // allocated and we have written exactly `offset` bytes.
                    return Ok((head_extent_index, offset as u32));
                };
                self.heap
                    .container
                    .block_at_mut(extent_block_index)
                    .set_extent_next_index(block_index)?;
                extent_block_index = block_index;
            }
        }

        // TODO(fxbug.dev/124958): we can remove cast
        // Safety: each max sized block has fewer than u32::MAX bytes, so an individual
        // block cannot have more than u32::MAX bytes written. The max VMO is also smaller
        // than u32::MAX bytes, so the sum of the bytes of all the blocks in the VMO
        // is smaller than u32::MAX. That means that the total length written to extents
        // is smaller than u32::MAX, and demotion from usize is always safe.
        Ok((head_extent_index, offset as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assert_data_tree,
        reader::{
            snapshot::{ScannedBlock, Snapshot},
            PartialNodeHierarchy,
        },
        writer::testing_utils::get_state,
        Inspector,
    };
    use futures::prelude::*;

    #[fuchsia::test]
    fn test_create() {
        let state = get_state(4096);
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
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

        let a = state.get_block(a_index);
        let b = state.get_block(b_index);
        assert_eq!(*a.parent_index().unwrap(), 0);
        assert_eq!(*b.parent_index().unwrap(), 0);

        assert_eq!(a.child_count().unwrap(), 0);
        assert_eq!(b.child_count().unwrap(), 0);

        state.reparent(b_index, a_index).unwrap();

        let a = state.get_block(a_index);
        let b = state.get_block(b_index);
        assert_eq!(*a.parent_index().unwrap(), 0);
        assert_eq!(b.parent_index().unwrap(), a.index());

        assert_eq!(a.child_count().unwrap(), 1);
        assert_eq!(b.child_count().unwrap(), 0);

        let c_index = state.create_node("c".into(), a_index).unwrap();

        let a = state.get_block(a_index);
        let b = state.get_block(b_index);
        let c = state.get_block(c_index);
        assert_eq!(*a.parent_index().unwrap(), 0);
        assert_eq!(b.parent_index().unwrap(), a.index());
        assert_eq!(c.parent_index().unwrap(), a.index());

        assert_eq!(a.child_count().unwrap(), 2);
        assert_eq!(b.child_count().unwrap(), 0);
        assert_eq!(c.child_count().unwrap(), 0);

        state.reparent(c_index, b_index).unwrap();

        let a = state.get_block(a_index);
        let b = state.get_block(b_index);
        let c = state.get_block(c_index);
        assert_eq!(*a.parent_index().unwrap(), 0);
        assert_eq!(b.parent_index().unwrap(), a_index);
        assert_eq!(c.parent_index().unwrap(), b_index);

        assert_eq!(a.child_count().unwrap(), 1);
        assert_eq!(b.child_count().unwrap(), 1);
        assert_eq!(c.child_count().unwrap(), 0);
    }

    #[fuchsia::test]
    fn test_node() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Create a node value and verify its fields
            let block_index = state.create_node("test-node".into(), 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::NodeValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(block.child_count().unwrap(), 0);
            assert_eq!(*block.name_index().unwrap(), 4);
            assert_eq!(*block.parent_index().unwrap(), 0);

            // Verify name block.
            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 9);
            assert_eq!(name_block.order(), 1);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test-node");
            block_index
        };

        // Verify blocks.
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::NodeValue);
        assert_eq!(blocks[2].block_type(), BlockType::Free);
        assert_eq!(blocks[3].block_type(), BlockType::StringReference);
        assert!(blocks[4..].iter().all(|b| b.block_type() == BlockType::Free));

        {
            let mut state = core_state.try_lock().expect("lock state");
            let child_block_index = state.create_node("child1".into(), block_index).unwrap();
            assert_eq!(state.get_block(block_index).child_count().unwrap(), 1);

            // Create a child of the child and verify child counts.
            let child11_block_index =
                state.create_node("child1-1".into(), child_block_index).unwrap();
            {
                assert_eq!(state.get_block(child11_block_index).child_count().unwrap(), 0);
                assert_eq!(state.get_block(child_block_index).child_count().unwrap(), 1);
                assert_eq!(state.get_block(block_index).child_count().unwrap(), 1);
            }

            assert!(state.free_value(child11_block_index).is_ok());
            {
                let child_block = state.get_block(child_block_index);
                assert_eq!(child_block.child_count().unwrap(), 0);
            }

            // Add a couple more children to the block and verify count.
            let child_block2_index = state.create_node("child2".into(), block_index).unwrap();
            let child_block3_index = state.create_node("child3".into(), block_index).unwrap();
            assert_eq!(state.get_block(block_index).child_count().unwrap(), 3);

            // Free children and verify count.
            assert!(state.free_value(child_block_index).is_ok());
            assert!(state.free_value(child_block2_index).is_ok());
            assert!(state.free_value(child_block3_index).is_ok());
            assert_eq!(state.get_block(block_index).child_count().unwrap(), 0);

            // Free node.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_int_metric() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index = state.create_int_metric("test".into(), 3, 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::IntValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(block.int_value().unwrap(), 3);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(*block.parent_index().unwrap(), 0);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::IntValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert!(blocks[3..].iter().all(|b| b.block_type() == BlockType::Free));

        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.add_int_metric(block_index, 10).is_ok());
            assert_eq!(state.get_block(block_index).int_value().unwrap(), 13);

            assert!(state.subtract_int_metric(block_index, 5).is_ok());
            assert_eq!(state.get_block(block_index).int_value().unwrap(), 8);

            assert!(state.set_int_metric(block_index, -6).is_ok());
            assert_eq!(state.get_block(block_index).int_value().unwrap(), -6);
            assert_eq!(state.get_int_metric(block_index).unwrap(), -6);

            assert!(state.subtract_int_metric(block_index, std::i64::MAX).is_ok());
            assert_eq!(state.get_block(block_index).int_value().unwrap(), std::i64::MIN);
            assert!(state.set_int_metric(block_index, std::i64::MAX).is_ok());

            assert!(state.add_int_metric(block_index, 2).is_ok());
            assert_eq!(state.get_block(block_index).int_value().unwrap(), std::i64::MAX);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_uint_metric() {
        let core_state = get_state(4096);

        // Creates with value
        let block_index = {
            let mut state = core_state.try_lock().expect("try lock");
            let block_index = state.create_uint_metric("test".into(), 3, 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::UintValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(block.uint_value().unwrap(), 3);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(*block.parent_index().unwrap(), 0);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::UintValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert!(blocks[3..].iter().all(|b| b.block_type() == BlockType::Free));

        {
            let mut state = core_state.try_lock().expect("try lock");
            assert!(state.add_uint_metric(block_index, 10).is_ok());
            assert_eq!(state.get_block(block_index).uint_value().unwrap(), 13);

            assert!(state.subtract_uint_metric(block_index, 5).is_ok());
            assert_eq!(state.get_block(block_index).uint_value().unwrap(), 8);

            assert!(state.set_uint_metric(block_index, 0).is_ok());
            assert_eq!(state.get_block(block_index).uint_value().unwrap(), 0);
            assert_eq!(state.get_uint_metric(block_index).unwrap(), 0);

            assert!(state.subtract_uint_metric(block_index, std::u64::MAX).is_ok());
            assert_eq!(state.get_block(block_index).uint_value().unwrap(), 0);

            assert!(state.set_uint_metric(block_index, 3).is_ok());
            assert!(state.add_uint_metric(block_index, std::u64::MAX).is_ok());
            assert_eq!(state.get_block(block_index).uint_value().unwrap(), std::u64::MAX);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_double_metric() {
        let core_state = get_state(4096);

        // Creates with value
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index = state.create_double_metric("test".into(), 3.0, 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::DoubleValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(block.double_value().unwrap(), 3.0);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(*block.parent_index().unwrap(), 0);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::DoubleValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert!(blocks[3..].iter().all(|b| b.block_type() == BlockType::Free));

        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.add_double_metric(block_index, 10.5).is_ok());
            assert_eq!(state.get_block(block_index).double_value().unwrap(), 13.5);

            assert!(state.subtract_double_metric(block_index, 5.1).is_ok());
            assert_eq!(state.get_block(block_index).double_value().unwrap(), 8.4);

            assert!(state.set_double_metric(block_index, -6.0).is_ok());
            assert_eq!(state.get_block(block_index).double_value().unwrap(), -6.0);
            assert_eq!(state.get_double_metric(block_index).unwrap(), -6.0);

            // Free metric.
            assert!(state.free_value(block_index).is_ok());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_create_property_cleanup_on_failure() {
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
        assert!(state.create_property(name, &payload, PropertyFormat::Bytes, 0.into()).is_err());

        drop(state);

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();

        // if cleanup happened correctly, the name + extent and property + extent have been freed
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
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

            assert!(state.inner_lock.string_reference_block_indexes.get(sf).is_some());

            assert_eq!(state.stats().allocated_blocks, 102);
            let block = state.get_block(collected[0]);
            let sf_block = state.get_block(block.name_index().unwrap());
            assert_eq!(sf_block.string_reference_count().unwrap(), 100);

            collected.into_iter().for_each(|b| {
                assert!(state.inner_lock.string_reference_block_indexes.get(sf).is_some());
                assert!(state.free_value(b).is_ok())
            });

            assert!(state.inner_lock.string_reference_block_indexes.get(sf).is_none());

            let node_index = state.create_node(sf.into(), 0.into()).unwrap();
            assert!(state.inner_lock.string_reference_block_indexes.get(sf).is_some());
            assert!(state.free_value(node_index).is_ok());
            assert!(state.inner_lock.string_reference_block_indexes.get(sf).is_none());
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_string_reference_data() {
        let core_state = get_state(4096); // allocates HEADER
        let mut state = core_state.try_lock().expect("lock state");

        // 4 bytes (4 ASCII characters in UTF-8) will fit inlined with a minimum block size
        let block_index = state.inner_lock.get_or_create_string_reference("abcd".into()).unwrap();
        let block = state.get_block(block_index);
        assert_eq!(block.block_type(), BlockType::StringReference);
        assert_eq!(block.order(), 0);
        assert_eq!(state.stats().allocated_blocks, 2);
        assert_eq!(state.stats().deallocated_blocks, 0);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 4);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.order(), 0);
        assert_eq!(state.load_string(block.index()).unwrap(), "abcd");

        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
        assert_eq!(state.stats().deallocated_blocks, 1);

        let block_index = state.inner_lock.get_or_create_string_reference("longer".into()).unwrap();
        let block = state.get_block(block_index);
        assert_eq!(block.block_type(), BlockType::StringReference);
        assert_eq!(block.order(), 1);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 6);
        assert_eq!(state.stats().allocated_blocks, 3);
        assert_eq!(state.stats().deallocated_blocks, 1);
        assert_eq!(state.load_string(block.index()).unwrap(), "longer");

        let idx = block.next_extent().unwrap();
        assert_eq!(*idx, 0);

        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
        assert_eq!(state.stats().deallocated_blocks, 2);

        let block_index = state.inner_lock.get_or_create_string_reference("longer".into()).unwrap();
        let mut block = state.get_block_mut(block_index);
        assert_eq!(block.order(), 1);
        block.increment_string_reference_count().unwrap();
        // not an error to try and free
        assert!(state.inner_lock.maybe_free_string_reference(block_index).is_ok());

        let mut block = state.get_block_mut(block_index);
        block.decrement_string_reference_count().unwrap();
        state.inner_lock.maybe_free_string_reference(block_index).unwrap();
    }

    #[fuchsia::test]
    fn test_string_property() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Creates with value
            let block_index = state
                .create_property("test".into(), b"test-property", PropertyFormat::String, 0.into())
                .unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::BufferValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index().unwrap(), 0);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(block.total_length().unwrap(), 13);
            assert_eq!(block.property_format().unwrap(), PropertyFormat::String);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let extent_block = state.get_block(4.into());
            assert_eq!(extent_block.block_type(), BlockType::Extent);
            assert_eq!(*extent_block.next_extent().unwrap(), 0);
            assert_eq!(
                std::str::from_utf8(extent_block.extent_contents().unwrap()).unwrap(),
                "test-property\0\0\0\0\0\0\0\0\0\0\0"
            );
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::BufferValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert_eq!(blocks[3].block_type(), BlockType::Extent);
        assert!(blocks[4..].iter().all(|b| b.block_type() == BlockType::Free));

        {
            let mut state = core_state.try_lock().expect("lock state");
            // Free property.
            assert!(state.free_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_string_arrays() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 4, 0.into()).unwrap();
            state.set_array_string_slot(array_index, 0, "0".into()).unwrap();
            state.set_array_string_slot(array_index, 1, "1".into()).unwrap();
            state.set_array_string_slot(array_index, 2, "2".into()).unwrap();
            state.set_array_string_slot(array_index, 3, "3".into()).unwrap();

            // size is 4
            assert!(state.set_array_string_slot(array_index, 4, "".into()).is_err());
            assert!(state.set_array_string_slot(array_index, 5, "".into()).is_err());

            for i in 0..4 {
                let idx = state.get_block(array_index).array_get_string_index_slot(i).unwrap();
                assert_eq!(i.to_string(), state.load_string(idx).unwrap());
            }

            assert!(state.get_block(array_index).array_get_string_index_slot(4).is_err());
            assert!(state.get_block(array_index).array_get_string_index_slot(5).is_err());

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn update_string_array_value() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            let array_index = state.create_string_array("array".into(), 2, 0.into()).unwrap();

            state.set_array_string_slot(array_index, 0, "abc".into()).unwrap();
            state.set_array_string_slot(array_index, 1, "def".into()).unwrap();

            state.set_array_string_slot(array_index, 0, "cba".into()).unwrap();
            state.set_array_string_slot(array_index, 1, "fed".into()).unwrap();

            let cba_index_slot =
                state.get_block(array_index).array_get_string_index_slot(0).unwrap();
            let fed_index_slot =
                state.get_block(array_index).array_get_string_index_slot(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap());
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        blocks[1..].iter().enumerate().for_each(|(i, b)| {
            assert!(b.block_type() == BlockType::Free, "index is {}", i + 1);
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

            let abc_index_slot =
                state.get_block(array_index).array_get_string_index_slot(0).unwrap();
            let def_index_slot =
                state.get_block(array_index).array_get_string_index_slot(1).unwrap();
            assert_eq!("abc".to_string(), state.load_string(abc_index_slot).unwrap(),);
            assert_eq!("def".to_string(), state.load_string(def_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, cba.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, fed.clone()).unwrap();

            let cba_index_slot =
                state.get_block(array_index).array_get_string_index_slot(0).unwrap();
            let fed_index_slot =
                state.get_block(array_index).array_get_string_index_slot(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap(),);
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, abc.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, def.clone()).unwrap();

            let abc_index_slot =
                state.get_block(array_index).array_get_string_index_slot(0).unwrap();
            let def_index_slot =
                state.get_block(array_index).array_get_string_index_slot(1).unwrap();
            assert_eq!("abc".to_string(), state.load_string(abc_index_slot).unwrap(),);
            assert_eq!("def".to_string(), state.load_string(def_index_slot).unwrap(),);

            state.set_array_string_slot(array_index, 0, cba.clone()).unwrap();
            state.set_array_string_slot(array_index, 1, fed.clone()).unwrap();

            let cba_index_slot =
                state.get_block(array_index).array_get_string_index_slot(0).unwrap();
            let fed_index_slot =
                state.get_block(array_index).array_get_string_index_slot(1).unwrap();
            assert_eq!("cba".to_string(), state.load_string(cba_index_slot).unwrap(),);
            assert_eq!("fed".to_string(), state.load_string(fed_index_slot).unwrap(),);

            state.clear_array(array_index, 0).unwrap();
            state.free_value(array_index).unwrap();
        }

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        blocks[1..].iter().enumerate().for_each(|(i, b)| {
            assert!(b.block_type() == BlockType::Free, "index is {}", i + 1);
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

        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        for b in blocks {
            if b.block_type() == BlockType::StringReference
                && state.load_string(b.index()).unwrap() == "array"
            {
                continue;
            }

            assert!(
                b.block_type() != BlockType::StringReference,
                "Got unexpected StringReference, index: {}, value (wrapped in single quotes): '{}'",
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
            let block_index = state
                .create_property("test".into(), b"test-property", PropertyFormat::Bytes, 0.into())
                .unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::BufferValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index().unwrap(), 0);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(block.total_length().unwrap(), 13);
            assert_eq!(*block.property_extent_index().unwrap(), 4);
            assert_eq!(block.property_format().unwrap(), PropertyFormat::Bytes);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let extent_block = state.get_block(4.into());
            assert_eq!(extent_block.block_type(), BlockType::Extent);
            assert_eq!(*extent_block.next_extent().unwrap(), 0);
            assert_eq!(
                std::str::from_utf8(extent_block.extent_contents().unwrap()).unwrap(),
                "test-property\0\0\0\0\0\0\0\0\0\0\0"
            );
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::BufferValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert_eq!(blocks[3].block_type(), BlockType::Extent);
        assert!(blocks[4..].iter().all(|b| b.block_type() == BlockType::Free));

        // Free property.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_bool() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Creates with value
            let block_index = state.create_bool("test".into(), true, 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::BoolValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(block.bool_value().unwrap(), true);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(*block.parent_index().unwrap(), 0);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 9);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::BoolValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert!(blocks[3..].iter().all(|b| b.block_type() == BlockType::Free));

        // Free metric.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_int_array() {
        let core_state = get_state(4096);
        let block_index = {
            let mut state = core_state.try_lock().expect("lock state");
            let block_index =
                state.create_int_array("test".into(), 5, ArrayFormat::Default, 0.into()).unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::ArrayValue);
            assert_eq!(block.order(), 2);
            assert_eq!(*block.index(), 4);
            assert_eq!(*block.name_index().unwrap(), 2);
            assert_eq!(*block.parent_index().unwrap(), 0);
            assert_eq!(block.array_slots().unwrap(), 5);
            assert_eq!(block.array_format().unwrap(), ArrayFormat::Default);
            assert_eq!(block.array_entry_type().unwrap(), BlockType::IntValue);

            let name_block = state.get_block(BlockIndex::from(2));
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");
            for i in 0..5 {
                state.set_array_int_slot(block_index, i, 3 * i as i64).unwrap();
            }
            for i in 0..5 {
                assert_eq!(
                    state.get_block(block_index).array_get_int_slot(i).unwrap(),
                    3 * i as i64
                );
            }
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::StringReference);
        assert_eq!(blocks[2].block_type(), BlockType::Free);
        assert_eq!(blocks[3].block_type(), BlockType::ArrayValue);
        assert!(blocks[4..].iter().all(|b| b.block_type() == BlockType::Free));

        // Free the array.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
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
        assert_eq!(written as usize, EXPECTED_WRITTEN);
        assert!(EXPECTED_WRITTEN < TRIED_TO_WRITE);
    }

    #[fuchsia::test]
    fn overflow_property() {
        const SIZE: usize = constants::MAX_ORDER_SIZE * 2;
        const EXPECTED_WRITTEN: usize = constants::MAX_ORDER_SIZE - constants::HEADER_SIZE_BYTES;

        let core_state = get_state(SIZE);
        let mut state = core_state.try_lock().expect("lock state");

        let data = "X".repeat(SIZE * 2);
        let block_index = state
            .create_property("test".into(), data.as_bytes(), PropertyFormat::String, 0.into())
            .unwrap();
        let block = state.get_block(block_index);
        assert_eq!(block.block_type(), BlockType::BufferValue);
        assert_eq!(*block.index(), 2);
        assert_eq!(*block.parent_index().unwrap(), 0);
        assert_eq!(*block.name_index().unwrap(), 3);
        assert_eq!(block.total_length().unwrap(), EXPECTED_WRITTEN);
        assert_eq!(*block.property_extent_index().unwrap(), 128);
        assert_eq!(block.property_format().unwrap(), PropertyFormat::String);

        let name_block = state.get_block(block.name_index().unwrap());
        assert_eq!(name_block.block_type(), BlockType::StringReference);
        assert_eq!(name_block.total_length().unwrap(), 4);
        assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

        let extent_block = state.get_block(128.into());
        assert_eq!(extent_block.block_type(), BlockType::Extent);
        assert_eq!(extent_block.order(), 7);
        assert_eq!(*extent_block.next_extent().unwrap(), *BlockIndex::EMPTY);
        assert_eq!(
            extent_block.extent_contents().unwrap(),
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
            let block_index = state
                .create_property("test".into(), data.as_bytes(), PropertyFormat::String, 0.into())
                .unwrap();
            let block = state.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::BufferValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index().unwrap(), 0);
            assert_eq!(*block.name_index().unwrap(), 3);
            assert_eq!(block.total_length().unwrap(), 6000);
            assert_eq!(*block.property_extent_index().unwrap(), 128);
            assert_eq!(block.property_format().unwrap(), PropertyFormat::String);

            let name_block = state.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 4);
            assert_eq!(state.load_string(name_block.index()).unwrap(), "test");

            let extent_block = state.get_block(128.into());
            assert_eq!(extent_block.block_type(), BlockType::Extent);
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent().unwrap(), 256);
            assert_eq!(
                extent_block.extent_contents().unwrap(),
                chars.iter().cycle().take(2040).map(|&c| c as u8).collect::<Vec<u8>>()
            );

            let extent_block = state.get_block(256.into());
            assert_eq!(extent_block.block_type(), BlockType::Extent);
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent().unwrap(), 384);
            assert_eq!(
                extent_block.extent_contents().unwrap(),
                chars.iter().cycle().skip(2040).take(2040).map(|&c| c as u8).collect::<Vec<u8>>()
            );

            let extent_block = state.get_block(384.into());
            assert_eq!(extent_block.block_type(), BlockType::Extent);
            assert_eq!(extent_block.order(), 7);
            assert_eq!(*extent_block.next_extent().unwrap(), 0);
            assert_eq!(
                extent_block.extent_contents().unwrap()[..1920],
                chars.iter().cycle().skip(4080).take(1920).map(|&c| c as u8).collect::<Vec<u8>>()[..]
            );
            assert_eq!(extent_block.extent_contents().unwrap()[1920..], [0u8; 120][..]);
            block_index
        };

        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 11);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::BufferValue);
        assert_eq!(blocks[2].block_type(), BlockType::StringReference);
        assert!(blocks[3..8].iter().all(|b| b.block_type() == BlockType::Free));
        assert_eq!(blocks[8].block_type(), BlockType::Extent);
        assert_eq!(blocks[9].block_type(), BlockType::Extent);
        assert_eq!(blocks[10].block_type(), BlockType::Extent);
        // Free property.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_property(block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_freeing_string_references() {
        let core_state = get_state(4096);
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert_eq!(state.stats().allocated_blocks, 1);

            let block0_index = state.create_node("abcd123456789".into(), 0.into()).unwrap();
            let block0_name_index = {
                let block0_name_index = state.get_block(block0_index).name_index().unwrap();
                let block0_name = state.get_block(block0_name_index);
                assert_eq!(block0_name.order(), 1);
                block0_name_index
            };
            assert_eq!(state.stats().allocated_blocks, 3);

            let block1_index =
                state.inner_lock.get_or_create_string_reference("abcd".into()).unwrap();
            assert_eq!(state.stats().allocated_blocks, 4);
            assert_eq!(state.get_block(block1_index).order(), 0);

            // no allocation!
            let block2_index =
                state.inner_lock.get_or_create_string_reference("abcd123456789".into()).unwrap();
            assert_eq!(state.get_block(block2_index).order(), 1);
            assert_eq!(block0_name_index, block2_index);
            assert_eq!(state.stats().allocated_blocks, 4);

            let block3_index = state.create_node("abcd12345678".into(), 0.into()).unwrap();
            let block3 = state.get_block(block3_index);
            let block3_name = state.get_block(block3.name_index().unwrap());
            assert_eq!(block3_name.order(), 1);
            assert_eq!(block3.order(), 0);
            assert_eq!(state.stats().allocated_blocks, 6);

            let mut long_name = "".to_string();
            for _ in 0..3000 {
                long_name += " ";
            }

            let block4_index = state.create_node(long_name.into(), 0.into()).unwrap();
            let block4 = state.get_block(block4_index);
            let block4_name = state.get_block(block4.name_index().unwrap());
            assert_eq!(block4_name.order(), 7);
            assert!(*block4_name.next_extent().unwrap() != 0);
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
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();

        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::NodeValue);
        assert_eq!(blocks[2].block_type(), BlockType::Free);
        assert_eq!(blocks[3].block_type(), BlockType::StringReference);
        assert!(blocks[4..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_tombstone() {
        let core_state = get_state(4096);
        let child_block_index = {
            let mut state = core_state.try_lock().expect("lock state");

            // Create a node value and verify its fields
            let block_index = state.create_node("root-node".into(), 0.into()).unwrap();
            let block_name_as_string_ref =
                state.get_block(state.get_block(block_index).name_index().unwrap());
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
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();

        // Note that the way Extents get allocated means that they aren't necessarily
        // put in the buffer where it would seem they should based on the literal order of allocation.
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::Tombstone);
        assert_eq!(blocks[2].block_type(), BlockType::NodeValue);
        assert_eq!(blocks[3].block_type(), BlockType::Free);
        assert_eq!(blocks[4].block_type(), BlockType::StringReference);
        assert!(blocks[5..].iter().all(|b| b.block_type() == BlockType::Free));

        // Freeing the child, causes all blocks to be freed.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_value(child_block_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }

    #[fuchsia::test]
    fn test_with_header_lock() {
        let state = get_state(4096);
        // Initial generation count is 0
        state.with_current_header(|header| {
            assert_eq!(header.header_generation_count().unwrap(), 0);
        });

        // Lock the state
        let mut lock_guard = state.try_lock().expect("lock state");
        assert!(lock_guard.header().check_locked(true).is_ok());
        assert_eq!(lock_guard.header().header_generation_count().unwrap(), 1);
        // Operations on the lock guard do not change the generation counter.
        let _ = lock_guard.create_node("test".into(), 0.into()).unwrap();
        let _ = lock_guard.create_node("test2".into(), 2.into()).unwrap();
        assert_eq!(lock_guard.header().header_generation_count().unwrap(), 1);

        // Dropping the guard releases the lock.
        drop(lock_guard);
        state.with_current_header(|header| {
            assert_eq!(header.header_generation_count().unwrap(), 2);
            assert!(header.check_locked(false).is_ok());
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
                Err(_) => assert!(false),
            }

            // Verify link block.
            let block = state_guard.get_block(block_index);
            assert_eq!(block.block_type(), BlockType::LinkValue);
            assert_eq!(*block.index(), 2);
            assert_eq!(*block.parent_index().unwrap(), 0);
            assert_eq!(*block.name_index().unwrap(), 4);
            assert_eq!(*block.link_content_index().unwrap(), 6);
            assert_eq!(block.link_node_disposition().unwrap(), LinkNodeDisposition::Inline);

            // Verify link's name block.
            let name_block = state_guard.get_block(block.name_index().unwrap());
            assert_eq!(name_block.block_type(), BlockType::StringReference);
            assert_eq!(name_block.total_length().unwrap(), 9);
            assert_eq!(state_guard.load_string(name_block.index()).unwrap(), "link-name");

            // Verify link's content block.
            let content_block = state_guard.get_block(block.link_content_index().unwrap());
            assert_eq!(content_block.block_type(), BlockType::StringReference);
            assert_eq!(content_block.total_length().unwrap(), 11);
            assert_eq!(state_guard.load_string(content_block.index()).unwrap(), "link-name-0");
            block_index
        };

        // Verify all the VMO blocks.
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert_eq!(blocks.len(), 10);
        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(blocks[1].block_type(), BlockType::LinkValue);
        assert_eq!(blocks[2].block_type(), BlockType::Free);
        assert_eq!(blocks[3].block_type(), BlockType::StringReference);
        assert_eq!(blocks[4].block_type(), BlockType::StringReference);
        assert!(blocks[5..].iter().all(|b| b.block_type() == BlockType::Free));

        // Free link
        {
            let mut state_guard = state.try_lock().expect("lock state");
            assert!(state_guard.free_lazy_node(block_index).is_ok());

            // Verify the callback was cleared on free link.
            assert!(state_guard.callbacks().get("link-name-0").is_none());
        }
        let snapshot = Snapshot::try_from(state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));

        // Verify adding another link generates a different ID regardless of the params.
        let mut state_guard = state.try_lock().expect("lock state");
        state_guard
            .create_lazy_node("link-name".into(), 0.into(), LinkNodeDisposition::Inline, || {
                async move { Ok(Inspector::default()) }.boxed()
            })
            .unwrap();
        let content_block = state_guard.get_block(6.into());
        assert_eq!(state_guard.load_string(content_block.index()).unwrap(), "link-name-1");
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
            assert_eq!(header.header_generation_count().unwrap(), 0);
        });

        // Begin a transaction
        state.begin_transaction();
        state.with_current_header(|header| {
            assert_eq!(header.header_generation_count().unwrap(), 1);
            assert!(header.check_locked(true).is_ok());
        });

        // Operations on the lock  guard do not change the generation counter.
        let mut lock_guard1 = state.try_lock().expect("lock state");
        assert_eq!(lock_guard1.inner_lock.transaction_count, 1);
        assert_eq!(lock_guard1.header().header_generation_count().unwrap(), 1);
        assert!(lock_guard1.header().check_locked(true).is_ok());
        let _ = lock_guard1.create_node("test".into(), 0.into()).unwrap();
        assert_eq!(lock_guard1.inner_lock.transaction_count, 1);
        assert_eq!(lock_guard1.header().header_generation_count().unwrap(), 1);

        // Dropping the guard releases the mutex lock but the header remains locked.
        drop(lock_guard1);
        state.with_current_header(|header| {
            assert_eq!(header.header_generation_count().unwrap(), 1);
            assert!(header.check_locked(true).is_ok());
        });

        // When the transaction finishes, the header is unlocked.
        state.end_transaction();

        state.with_current_header(|header| {
            assert_eq!(header.header_generation_count().unwrap(), 2);
            assert!(header.check_locked(false).is_ok());
        });

        // Operations under no transaction work as usual.
        let lock_guard2 = state.try_lock().expect("lock state");
        assert!(lock_guard2.header().check_locked(true).is_ok());
        assert_eq!(lock_guard2.header().header_generation_count().unwrap(), 3);
        assert_eq!(lock_guard2.inner_lock.transaction_count, 0);
    }

    #[fuchsia::test]
    async fn update_header_vmo_size() {
        let core_state = get_state(3 * 4096);
        core_state.get_block(BlockIndex::HEADER, |header| {
            assert_eq!(header.header_vmo_size().unwrap().unwrap(), 4096);
        });
        let block1_index = {
            let mut state = core_state.try_lock().expect("lock state");

            let chars = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];
            let data = chars.iter().cycle().take(6000).collect::<String>();
            let block_index = state
                .create_property("test".into(), data.as_bytes(), PropertyFormat::String, 0.into())
                .unwrap();
            assert_eq!(state.header().header_vmo_size().unwrap().unwrap(), 2 * 4096);

            block_index
        };

        let block2_index = {
            let mut state = core_state.try_lock().expect("lock state");

            let chars = ['a', 'b', 'c', 'd', 'e', 'f', 'g'];
            let data = chars.iter().cycle().take(3000).collect::<String>();
            let block_index = state
                .create_property("test".into(), data.as_bytes(), PropertyFormat::String, 0.into())
                .unwrap();
            assert_eq!(state.header().header_vmo_size().unwrap().unwrap(), 3 * 4096);

            block_index
        };
        // Free properties.
        {
            let mut state = core_state.try_lock().expect("lock state");
            assert!(state.free_property(block1_index).is_ok());
            assert!(state.free_property(block2_index).is_ok());
        }
        let snapshot = Snapshot::try_from(core_state.copy_vmo_bytes().unwrap()).unwrap();
        let blocks: Vec<ScannedBlock<'_>> = snapshot.scan().collect();
        assert!(blocks[1..].iter().all(|b| b.block_type() == BlockType::Free));
    }
}
