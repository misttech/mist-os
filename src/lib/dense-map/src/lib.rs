// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A densely packed map.
//!
//! Defines the [`DenseMap`] data structure: A generic mapped container keyed by
//! an internally managed pool of identifiers kept densely packed.

#![no_std]
#![deny(missing_docs, unreachable_patterns)]

extern crate fakealloc as alloc;

pub mod collection;
#[cfg(test)]
mod testutil;

use alloc::vec::Vec;
use core::fmt::Debug;

/// [`DenseMap`]s use `usize`s for keys.
pub type Key = usize;

/// DenseMapEntry where all free blocks are linked together.
#[derive(PartialEq, Eq, Debug)]
#[cfg_attr(test, derive(Clone))]
enum DenseMapEntry<T> {
    /// The Entry should either be allocated and contains a value...
    Allocated(AllocatedEntry<T>),
    /// Or it is not currently used and should be part of a freelist.
    Free(ListLink),
}

#[derive(PartialEq, Eq, Debug)]
#[cfg_attr(test, derive(Clone))]
struct AllocatedEntry<T> {
    link: ListLink,
    item: T,
}

/// The link of a doubly-linked list.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct ListLink {
    /// The index of the previous free block in the list.
    prev: Option<usize>,
    /// The index of the next free block in the list.
    next: Option<usize>,
}

impl Default for ListLink {
    /// By default, an entry is not linked into the list.
    fn default() -> Self {
        Self { prev: None, next: None }
    }
}

/// Stores positions of the head and tail of a doubly-linked list by
/// `ListLink`.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct List {
    /// The index of the first free block.
    head: usize,
    /// The index of the last free block.
    tail: usize,
}

impl List {
    /// Construct a doubly-linked list with only one element.
    fn singleton(elem: usize) -> List {
        List { head: elem, tail: elem }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DenseMapEntryKind {
    Allocated,
    Free,
}

impl<T> DenseMapEntry<T> {
    fn link(&self) -> (&ListLink, DenseMapEntryKind) {
        match self {
            DenseMapEntry::Allocated(entry) => (&entry.link, DenseMapEntryKind::Allocated),
            DenseMapEntry::Free(link) => (&link, DenseMapEntryKind::Free),
        }
    }

    fn link_mut_and_check(&mut self, expected_kind: DenseMapEntryKind) -> &mut ListLink {
        let (link, got_kind) = match self {
            DenseMapEntry::Allocated(entry) => (&mut entry.link, DenseMapEntryKind::Allocated),
            DenseMapEntry::Free(link) => (link, DenseMapEntryKind::Free),
        };
        assert_eq!(expected_kind, got_kind);
        link
    }

    /// Returns a reference to the freelist link if the entry is free, otherwise None.
    fn as_free_or_none(&self) -> Option<&ListLink> {
        match self {
            DenseMapEntry::Free(link) => Some(link),
            DenseMapEntry::Allocated(_) => None,
        }
    }

    /// Returns a mutable reference to the freelist link if the entry is free, otherwise None.
    fn as_free_or_none_mut(&mut self) -> Option<&mut ListLink> {
        match self {
            DenseMapEntry::Free(link) => Some(link),
            DenseMapEntry::Allocated(_) => None,
        }
    }
}

/// A generic container for `T` keyed by densely packed integers.
///
/// `DenseMap` is a generic container keyed by `usize` that manages its own key
/// pool. `DenseMap` reuses keys that are free to keep its key pool as dense as
/// possible.
///
/// The main guarantee provided by `DenseMap` is that all `get` operations are
/// provided in O(1) without the need to hash the keys.
///
/// All operations that mutate the `DenseMap` may be O(n) during resizing or
/// compression but averages at O(1).
///
/// `push` will grab the lowest free `id` and assign it to the given value,
/// returning the assigned `id`. `insert` can be used for assigning a specific
/// `id` to an object, and returns the previous object at that `id` if any.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub struct DenseMap<T> {
    freelist: Option<List>,
    allocatedlist: Option<List>,
    data: Vec<DenseMapEntry<T>>,
}

impl<T> DenseMap<T> {
    /// Creates a new empty [`DenseMap`].
    pub fn new() -> Self {
        Self { freelist: None, allocatedlist: None, data: Vec::new() }
    }

    /// Returns `true` if there are no items in [`DenseMap`].
    pub fn is_empty(&self) -> bool {
        // Because of `compress`, our map is empty if and only if the underlying
        // vector is empty. If the underlying vector is not empty but our map is
        // empty, it must be the case where the underlying vector contains
        // nothing but free entries, and all these entries should be reclaimed
        // when the last allocated entry is removed.
        self.data.is_empty()
    }

    /// Returns a reference to the item indexed by `key`, or `None` if the `key`
    /// doesn't exist.
    pub fn get(&self, key: Key) -> Option<&T> {
        self.data.get(key).and_then(|v| match v {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => Some(item),
            DenseMapEntry::Free(_) => None,
        })
    }

    /// Returns a mutable reference to the item indexed by `key`, or `None` if
    /// the `key` doesn't exist.
    pub fn get_mut(&mut self, key: Key) -> Option<&mut T> {
        self.data.get_mut(key).and_then(|v| match v {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => Some(item),
            DenseMapEntry::Free(_) => None,
        })
    }

    /// Removes item indexed by `key` from the container.
    ///
    /// Returns the removed item if it exists, or `None` otherwise.
    ///
    /// Note: the worst case complexity of `remove` is O(key) if the backing
    /// data structure of the [`DenseMap`] is too sparse.
    pub fn remove(&mut self, key: Key) -> Option<T> {
        let r = self.remove_inner(key);
        if r.is_some() {
            self.compress();
        }
        r
    }

    fn remove_inner(&mut self, key: Key) -> Option<T> {
        let r = self.data.get_mut(key).and_then(|v| {
            match v {
                DenseMapEntry::Allocated(_) => {
                    let old_head = self.freelist.map(|l| l.head);
                    let new_link = DenseMapEntry::Free(ListLink { prev: None, next: old_head });
                    match core::mem::replace(v, new_link) {
                        DenseMapEntry::Allocated(entry) => Some(entry),
                        DenseMapEntry::Free(_) => unreachable!("already matched"),
                    }
                }
                // If it is currently free, we don't want to unlink the entry and
                // link it back at the head again.
                DenseMapEntry::Free(_) => None,
            }
        });
        r.map(|AllocatedEntry { link, item }| {
            self.unlink_entry_inner(DenseMapEntryKind::Allocated, link);
            // If it was allocated, we add the removed entry to the head of the
            // free-list.
            match self.freelist.as_mut() {
                Some(List { head, .. }) => {
                    self.data[*head]
                        .as_free_or_none_mut()
                        .unwrap_or_else(|| panic!("freelist head node {} is not free", head))
                        .prev = Some(key);
                    *head = key;
                }
                None => self.freelist = Some(List::singleton(key)),
            }

            item
        })
    }

    fn allocated_link(
        allocatedlist: &mut Option<List>,
        data: &mut [DenseMapEntry<T>],
        key: Key,
    ) -> ListLink {
        // Add the key to the tail of the allocated list. There are four cases
        // to consider. In all cases however, we update the list's tail to point
        // to `key` and create a `ListLink` with a previous pointer that points
        // to the current allocate list's tail and no next pointer.
        //
        //  1) The allocated list is empty.
        //  2) `key` points to a free entry
        //  3) `key` is the (non-empty) allocated list's tail.
        //  4) `key` is in the middle of the (non-empty) allocated list.
        if let Some(List { head: _, tail }) = allocatedlist {
            match data[*tail] {
                DenseMapEntry::Allocated(ref mut entry) => {
                    assert_eq!(None, core::mem::replace(&mut entry.link.next, Some(key)));
                    ListLink { next: None, prev: Some(core::mem::replace(tail, key)) }
                }
                DenseMapEntry::Free(_) => unreachable!(
                    "allocated list tail entry is free; key = {}; tail = {}",
                    key, tail,
                ),
            }
        } else {
            *allocatedlist = Some(List { head: key, tail: key });

            ListLink { next: None, prev: None }
        }
    }

    /// Inserts `item` at `key`.
    ///
    /// If the [`DenseMap`] already contained an item indexed by `key`, `insert`
    /// returns it, or `None` otherwise.
    ///
    /// Note: The worst case complexity of `insert` is O(key) if `key` is larger
    /// than the number of items currently held by the [`DenseMap`].
    pub fn insert(&mut self, key: Key, item: T) -> Option<T> {
        if key < self.data.len() {
            // Remove the entry from whatever list it may be in
            self.unlink_entry(key);
            // Add the key to the allocated list.
            let link = Self::allocated_link(&mut self.allocatedlist, &mut self.data, key);

            let prev = core::mem::replace(
                &mut self.data[key],
                DenseMapEntry::Allocated(AllocatedEntry { link, item }),
            );
            // We don't need to do anything with the `ListLink` since we removed
            // the entry from the list.
            match prev {
                DenseMapEntry::Free(ListLink { .. }) => None,
                DenseMapEntry::Allocated(AllocatedEntry { link: ListLink { .. }, item }) => {
                    Some(item)
                }
            }
        } else {
            let start_len = self.data.len();
            // Fill the gap `start_len .. key` with free entries. Currently, the
            // free entries introduced by `insert` is linked at the end of the
            // free list so that hopefully these free entries near the end will
            // get less likely to be allocated than those near the beginning,
            // this may help reduce the memory footprint because we have
            // increased the chance for the underlying vector to be compressed.
            // TODO: explore whether we can reorder the list on the fly to
            // further increase the chance for compressing.
            for idx in start_len..key {
                // These new free entries will be linked to each other, except:
                // - the first entry's prev should point to the old tail.
                // - the last entry's next should be None.
                self.data.push(DenseMapEntry::Free(ListLink {
                    prev: if idx == start_len {
                        self.freelist.map(|l| l.tail)
                    } else {
                        Some(idx - 1)
                    },
                    next: if idx == key - 1 { None } else { Some(idx + 1) },
                }));
            }
            // If `key > start_len`, we have inserted at least one free entry,
            // so we have to update our freelist.
            if key > start_len {
                let new_tail = key - 1;
                match self.freelist.as_mut() {
                    Some(List { tail, .. }) => {
                        self.data[*tail]
                            .as_free_or_none_mut()
                            .unwrap_or_else(|| panic!("freelist tail node {} is not free", tail))
                            .next = Some(start_len);
                        *tail = new_tail;
                    }
                    None => {
                        self.freelist = Some(List { head: start_len, tail: new_tail });
                    }
                }
            }
            // And finally we insert our item into the map.
            let link = Self::allocated_link(&mut self.allocatedlist, &mut self.data, key);
            self.data.push(DenseMapEntry::Allocated(AllocatedEntry { link, item }));
            None
        }
    }

    /// Inserts `item` into the [`DenseMap`].
    ///
    /// `push` inserts a new `item` into the [`DenseMap`] and returns the key value
    /// allocated for `item`. `push` will allocate *any* key that is currently
    /// free in the internal structure, so it may return a key that was used
    /// previously but has since been removed.
    ///
    /// Note: The worst case complexity of `push` is O(n) where n is the number
    /// of items held by the [`DenseMap`]. This can happen if the internal
    /// structure gets fragmented.
    pub fn push(&mut self, item: T) -> Key {
        *self.push_entry(item).key()
    }

    /// Inserts `item` into the [`DenseMap`] and returns an [`OccupiedEntry`] for
    /// it.
    ///
    /// Like [`DenseMap::push`] except that it returns an entry instead of an
    /// index.
    pub fn push_entry(&mut self, item: T) -> OccupiedEntry<'_, usize, T> {
        self.push_with(|_: usize| item)
    }

    /// Creates an `item` in the `DenseMap` via functor.
    ///
    /// Like [`DenseMap::push`] except that the item is constructed by the provided
    /// function, which is passed its index.
    pub fn push_with(&mut self, make_item: impl FnOnce(Key) -> T) -> OccupiedEntry<'_, usize, T> {
        let Self { freelist, allocatedlist, data } = self;
        if let Some(List { head, .. }) = freelist.as_mut() {
            let ret = *head;
            let link = Self::allocated_link(allocatedlist, data, ret);
            let old = core::mem::replace(
                data.get_mut(ret).unwrap(),
                DenseMapEntry::Allocated(AllocatedEntry { link, item: make_item(ret) }),
            );
            let old_link = old
                .as_free_or_none()
                .unwrap_or_else(|| panic!("freelist head node {} is not free", head));
            // Update the head of the freelist.
            match old_link.next {
                Some(new_head) => {
                    *head = new_head;
                    data[new_head]
                        .as_free_or_none_mut()
                        .unwrap_or_else(|| panic!("new free list head {} is not free", new_head))
                        .prev = None;
                }
                None => *freelist = None,
            }
            OccupiedEntry { key: ret, id_map: self }
        } else {
            // If we run out of freelist, we simply push a new entry into the
            // underlying vector.
            let key = data.len();
            let link = Self::allocated_link(allocatedlist, data, key);
            data.push(DenseMapEntry::Allocated(AllocatedEntry { link, item: make_item(key) }));
            OccupiedEntry { key, id_map: self }
        }
    }

    /// Compresses the tail of the internal `Vec`.
    ///
    /// `compress` removes all trailing elements in `data` that are `None`,
    /// shrinking the internal `Vec`.
    fn compress(&mut self) {
        // First, find the last non-free entry.
        if let Some(idx) = self.data.iter().enumerate().rev().find_map(|(k, v)| match v {
            DenseMapEntry::Allocated(_) => Some(k),
            DenseMapEntry::Free(_) => None,
        }) {
            // Remove all the trailing free entries.
            for i in idx + 1..self.data.len() {
                let link = *self.data[i].as_free_or_none().expect("already confirmed as free");
                self.unlink_entry_inner(DenseMapEntryKind::Free, link);
            }
            self.data.truncate(idx + 1);
        } else {
            // There is nothing left in the vector.
            self.data.clear();
            self.freelist = None;
        }
    }

    /// Creates an iterator over the containing items and their associated keys.
    ///
    /// The iterator will return items in insertion order.
    pub fn insertion_ordered_iter(&self) -> InsertionOrderedIter<'_, T> {
        let Self { data, freelist: _, allocatedlist } = self;
        let next_idx = allocatedlist.map(|l| l.head);
        InsertionOrderedIter { next_idx, map: data.as_ref() }
    }

    /// Creates an iterator over the containing items and their associated keys.
    ///
    /// The iterator will return items in key order.
    pub fn key_ordered_iter(&self) -> KeyOrderedIter<'_, T> {
        IntoIterator::into_iter(self)
    }

    /// Creates a mutable iterator over the containing items and their
    /// associated keys.
    ///
    /// The iterator will return items in key order.
    pub fn key_ordered_iter_mut(&mut self) -> KeyOrderedIterMut<'_, T> {
        IntoIterator::into_iter(self)
    }

    /// Consumes the `DenseMap` and returns an iterator over the contained items
    /// and their associated keys.
    ///
    /// The iterator will return items in key order.
    pub fn key_ordered_into_iter(self) -> IntoKeyOrderedIter<T> {
        IntoIterator::into_iter(self)
    }

    /// Gets the given key's corresponding entry in the map for in-place
    /// manipulation.
    pub fn entry(&mut self, key: usize) -> Entry<'_, usize, T> {
        if let Some(DenseMapEntry::Allocated(_)) = self.data.get(key) {
            Entry::Occupied(OccupiedEntry { key, id_map: self })
        } else {
            Entry::Vacant(VacantEntry { key, id_map: self })
        }
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all elements e for which f(&e) returns false. The
    /// elements are visited in ascending key order.
    pub fn key_ordered_retain<F: FnMut(&T) -> bool>(&mut self, mut should_retain: F) {
        (0..self.data.len()).for_each(|k| {
            if let DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) =
                self.data.get_mut(k).unwrap()
            {
                if !should_retain(item) {
                    // Note the use of `remove_inner` rather than `remove`
                    // here. `remove` calls `self.compress()`, which is an
                    // O(n) operation. Instead, we postpone that operation
                    // and perform it once during the last iteration so that
                    // the overall complexity is O(n) rather than O(n^2).
                    //
                    // TODO(joshlf): Could we improve the performance here
                    // by doing something smarter than just calling
                    // `remove_inner`? E.g., perhaps we could build up a
                    // separate linked list that we only insert into the
                    // existing free list once at the end? That there is a
                    // performance issue here at all is pure speculation,
                    // and will need to be measured to determine whether
                    // such an optimization is worth it.
                    let _: T = self.remove_inner(k).unwrap();
                }
            }
        });

        self.compress();
    }

    fn unlink_entry_inner(&mut self, kind: DenseMapEntryKind, link: ListLink) {
        let ListLink { next, prev } = link;

        let list = match kind {
            DenseMapEntryKind::Allocated => &mut self.allocatedlist,
            DenseMapEntryKind::Free => &mut self.freelist,
        };

        match (prev, next) {
            (Some(prev), Some(next)) => {
                // A normal node in the middle of a list.
                self.data[prev].link_mut_and_check(kind).next = Some(next);
                self.data[next].link_mut_and_check(kind).prev = Some(prev);
            }
            (Some(prev), None) => {
                // The node at the tail.
                self.data[prev].link_mut_and_check(kind).next = next;
                list.as_mut().unwrap().tail = prev;
            }
            (None, Some(next)) => {
                // The node at the head.
                self.data[next].link_mut_and_check(kind).prev = prev;
                list.as_mut().unwrap().head = next;
            }
            (None, None) => {
                // We are the last node.
                *list = None;
            }
        }
    }

    fn unlink_entry(&mut self, i: Key) {
        let (link, kind) = self.data[i].link();
        self.unlink_entry_inner(kind, *link);
    }
}

impl<T> Default for DenseMap<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// An iterator over the keys and values stored in an [`DenseMap`].
///
/// This iterator returns items in insertion order.
pub struct InsertionOrderedIter<'s, T> {
    next_idx: Option<usize>,
    map: &'s [DenseMapEntry<T>],
}

impl<'a, T> Iterator for InsertionOrderedIter<'a, T> {
    type Item = (Key, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        let Self { next_idx, map } = self;
        let k = (*next_idx)?;
        match &map[k] {
            DenseMapEntry::Allocated(AllocatedEntry { link: ListLink { next, prev: _ }, item }) => {
                *next_idx = *next;
                Some((k, item))
            }
            DenseMapEntry::Free(_) => {
                unreachable!("free entry found in allocated list @ idx={}", k)
            }
        }
    }
}

/// An iterator over the keys and values stored in an [`DenseMap`].
///
/// This iterator returns items in key order.
pub struct IntoKeyOrderedIter<T>(core::iter::Enumerate<alloc::vec::IntoIter<DenseMapEntry<T>>>);

impl<T> Iterator for IntoKeyOrderedIter<T> {
    type Item = (Key, T);
    fn next(&mut self) -> Option<Self::Item> {
        let Self(it) = self;
        it.filter_map(|(k, v)| match v {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => Some((k, item)),
            DenseMapEntry::Free(_) => None,
        })
        .next()
    }
}

impl<T> IntoIterator for DenseMap<T> {
    type Item = (Key, T);
    type IntoIter = IntoKeyOrderedIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoKeyOrderedIter(self.data.into_iter().enumerate())
    }
}

/// An iterator over the keys and values stored in an [`DenseMap`].
///
/// This iterator returns items in key order.
pub struct KeyOrderedIter<'s, T>(core::iter::Enumerate<core::slice::Iter<'s, DenseMapEntry<T>>>);

impl<'a, T> Iterator for KeyOrderedIter<'a, T> {
    type Item = (Key, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        let Self(it) = self;
        it.filter_map(|(k, v)| match v {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => Some((k, item)),
            DenseMapEntry::Free(_) => None,
        })
        .next()
    }
}

impl<'a, T> IntoIterator for &'a DenseMap<T> {
    type Item = (Key, &'a T);
    type IntoIter = KeyOrderedIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        KeyOrderedIter(self.data.iter().enumerate())
    }
}

/// An iterator over the keys and mutable values stored in an [`DenseMap`].
///
/// This iterator returns items in key order.
pub struct KeyOrderedIterMut<'s, T>(
    core::iter::Enumerate<core::slice::IterMut<'s, DenseMapEntry<T>>>,
);

impl<'a, T> Iterator for KeyOrderedIterMut<'a, T> {
    type Item = (Key, &'a mut T);

    fn next(&mut self) -> Option<Self::Item> {
        let Self(it) = self;
        it.filter_map(|(k, v)| match v {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => Some((k, item)),
            DenseMapEntry::Free(_) => None,
        })
        .next()
    }
}

impl<'a, T> IntoIterator for &'a mut DenseMap<T> {
    type Item = (Key, &'a mut T);
    type IntoIter = KeyOrderedIterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        KeyOrderedIterMut(self.data.iter_mut().enumerate())
    }
}

/// A key providing an index into an [`DenseMap`].
pub trait EntryKey {
    /// Returns the index for this key.
    fn get_key_index(&self) -> usize;
}

impl EntryKey for usize {
    fn get_key_index(&self) -> usize {
        *self
    }
}

/// A view into a vacant entry in a map. It is part of the [`Entry`] enum.
pub struct VacantEntry<'a, K, T> {
    key: K,
    id_map: &'a mut DenseMap<T>,
}

impl<'a, K, T> VacantEntry<'a, K, T> {
    /// Sets the value of the entry with the VacantEntry's key, and returns a
    /// mutable reference to it.
    pub fn insert(self, value: T) -> &'a mut T
    where
        K: EntryKey,
    {
        assert!(self.id_map.insert(self.key.get_key_index(), value).is_none());
        match &mut self.id_map.data[self.key.get_key_index()] {
            DenseMapEntry::Allocated(AllocatedEntry { link: _, item }) => item,
            DenseMapEntry::Free(_) => unreachable!("entry is known to be vacant"),
        }
    }

    /// Gets a reference to the key that would be used when inserting a value
    /// through the `VacantEntry`.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Take ownership of the key.
    pub fn into_key(self) -> K {
        self.key
    }

    /// Changes the key type of this `VacantEntry` to another key `X` that still
    /// maps to the same index in an `DenseMap`.
    ///
    /// # Panics
    ///
    /// Panics if the resulting mapped key from `f` does not return the same
    /// value for [`EntryKey::get_key_index`] as the old key did.
    pub(crate) fn map_key<X, F>(self, f: F) -> VacantEntry<'a, X, T>
    where
        K: EntryKey,
        X: EntryKey,
        F: FnOnce(K) -> X,
    {
        let idx = self.key.get_key_index();
        let key = f(self.key);
        assert_eq!(idx, key.get_key_index());
        VacantEntry { key, id_map: self.id_map }
    }
}

/// A view into an occupied entry in a map. It is part of the [`Entry`] enum.
pub struct OccupiedEntry<'a, K, T> {
    key: K,
    id_map: &'a mut DenseMap<T>,
}

impl<'a, K: EntryKey, T> OccupiedEntry<'a, K, T> {
    /// Gets a reference to the key in the entry.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Leaves the value in the map and returns the key.
    pub fn into_key(self) -> K {
        self.key
    }

    /// Gets a reference to the value in the entry.
    pub fn get(&self) -> &T {
        // We can unwrap because value is always Some for OccupiedEntry.
        self.id_map.get(self.key.get_key_index()).unwrap()
    }

    /// Gets a mutable reference to the value in the entry.
    ///
    /// If you need a reference to the `OccupiedEntry` which may outlive the
    /// destruction of the entry value, see [`OccupiedEntry::into_mut`].
    pub fn get_mut(&mut self) -> &mut T {
        // We can unwrap because value is always Some for OccupiedEntry.
        self.id_map.get_mut(self.key.get_key_index()).unwrap()
    }

    /// Converts the `OccupiedEntry` into a mutable reference to the value in
    /// the entry with a lifetime bound to the map itself.
    ///
    /// If you need multiple references to the `OccupiedEntry`, see
    /// [`OccupiedEntry::get_mut`].
    pub fn into_mut(self) -> &'a mut T {
        // We can unwrap because value is always Some for OccupiedEntry.
        self.id_map.get_mut(self.key.get_key_index()).unwrap()
    }

    /// Sets the value of the entry, and returns the entry's old value.
    pub fn insert(&mut self, value: T) -> T {
        // We can unwrap because value is always Some for OccupiedEntry.
        self.id_map.insert(self.key.get_key_index(), value).unwrap()
    }

    /// Takes the value out of the entry, and returns it.
    pub fn remove(self) -> T {
        // We can unwrap because value is always Some for OccupiedEntry.
        self.id_map.remove(self.key.get_key_index()).unwrap()
    }

    /// Changes the key type of this `OccupiedEntry` to another key `X` that
    /// still maps to the same index in an `DenseMap`.
    ///
    /// # Panics
    ///
    /// Panics if the resulting mapped key from `f` does not return the same
    /// value for [`EntryKey::get_key_index`] as the old key did.
    pub(crate) fn map_key<X, F>(self, f: F) -> OccupiedEntry<'a, X, T>
    where
        K: EntryKey,
        X: EntryKey,
        F: FnOnce(K) -> X,
    {
        let idx = self.key.get_key_index();
        let key = f(self.key);
        assert_eq!(idx, key.get_key_index());
        OccupiedEntry { key, id_map: self.id_map }
    }
}

impl<'a, K: Debug, T> Debug for OccupiedEntry<'a, K, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self { key, id_map: _ } = self;
        f.debug_struct("OccupiedEntry").field("key", key).field("id_map", &"_").finish()
    }
}

/// A view into an in-place entry in a map that can be vacant or occupied.
pub enum Entry<'a, K, T> {
    /// A vacant entry.
    Vacant(VacantEntry<'a, K, T>),
    /// An occupied entry.
    Occupied(OccupiedEntry<'a, K, T>),
}

impl<'a, K: EntryKey, T> Entry<'a, K, T> {
    /// Returns a reference to this entry's key.
    pub fn key(&self) -> &K {
        match self {
            Entry::Vacant(e) => e.key(),
            Entry::Occupied(e) => e.key(),
        }
    }

    /// Ensures a value is in the entry by inserting `default` if empty, and
    /// returns a mutable reference to the value in the entry.
    pub fn or_insert(self, default: T) -> &'a mut T
    where
        K: EntryKey,
    {
        match self {
            Entry::Vacant(e) => e.insert(default),
            Entry::Occupied(e) => e.into_mut(),
        }
    }

    /// Ensures a value is in the entry by inserting the result of the function
    /// `f` if empty, and returns a mutable reference to the value in the entry.
    pub fn or_insert_with<F: FnOnce() -> T>(self, f: F) -> &'a mut T
    where
        K: EntryKey,
    {
        match self {
            Entry::Vacant(e) => e.insert(f()),
            Entry::Occupied(e) => e.into_mut(),
        }
    }

    /// Ensures a value is in the entry by inserting the default value if empty,
    /// and returns a mutable reference to the value in the entry.
    pub fn or_default(self) -> &'a mut T
    where
        T: Default,
        K: EntryKey,
    {
        self.or_insert_with(<T as Default>::default)
    }

    /// Provides in-place mutable access to an occupied entry before any
    /// potential inserts into the map.
    pub fn and_modify<F: FnOnce(&mut T)>(self, f: F) -> Self {
        match self {
            Entry::Vacant(e) => Entry::Vacant(e),
            Entry::Occupied(mut e) => {
                f(e.get_mut());
                Entry::Occupied(e)
            }
        }
    }

    /// Remove the entry from [`DenseMap`].
    pub fn remove(self) -> Option<T> {
        match self {
            Entry::Vacant(_) => None,
            Entry::Occupied(e) => Some(e.remove()),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashMap, vec, vec::Vec};

    use rand::seq::SliceRandom as _;

    use crate::{
        testutil::assert_empty,
        DenseMapEntry::{Allocated, Free},
        *,
    };

    // Smart constructors
    fn free<T>(prev: usize, next: usize) -> DenseMapEntry<T> {
        Free(ListLink { prev: Some(prev), next: Some(next) })
    }

    fn free_head<T>(next: usize) -> DenseMapEntry<T> {
        Free(ListLink { prev: None, next: Some(next) })
    }

    fn free_tail<T>(prev: usize) -> DenseMapEntry<T> {
        Free(ListLink { prev: Some(prev), next: None })
    }

    fn free_none<T>() -> DenseMapEntry<T> {
        Free(ListLink::default())
    }

    #[test]
    fn test_push() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(1, 2), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 2 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 1 }));
        assert_eq!(map.push(1), 0);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: Some(1), next: None }, item: 1 }),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(0) }, item: 2 })
            ]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 0 }));
        assert_eq!(map.push(3), 2);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(2) },
                    item: 1
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(0) }, item: 2 }),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 2 }));
    }

    #[test]
    fn test_get() {
        let mut map = DenseMap::new();
        assert_eq!(map.push(1), 0);
        assert_eq!(map.insert(2, 3), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(2) }, item: 1 }),
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(1)));
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 2 }));
        assert_eq!(*map.get(0).unwrap(), 1);
        assert_eq!(map.get(1), None);
        assert_eq!(*map.get(2).unwrap(), 3);
        assert_eq!(map.get(3), None);
    }

    #[test]
    fn test_get_mut() {
        let mut map = DenseMap::new();
        assert_eq!(map.push(1), 0);
        assert_eq!(map.insert(2, 3), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(2) }, item: 1 }),
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(1)));
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 2 }));
        *map.get_mut(2).unwrap() = 10;
        assert_eq!(*map.get(0).unwrap(), 1);
        assert_eq!(*map.get(2).unwrap(), 10);

        assert_eq!(map.get_mut(1), None);
        assert_eq!(map.get_mut(3), None);
    }

    #[test]
    fn test_is_empty() {
        let mut map = DenseMap::<i32>::new();
        assert!(map.is_empty());
        assert_eq!(map.push(1), 0);
        assert!(!map.is_empty());
    }

    #[test]
    fn test_remove() {
        let mut map = DenseMap::new();
        assert_eq!(map.push(1), 0);
        assert_eq!(map.push(2), 1);
        assert_eq!(map.push(3), 2);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(1) }, item: 1 }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(0), next: Some(2) },
                    item: 2
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(1), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 2 }));
        assert_eq!(map.remove(1).unwrap(), 2);

        assert_eq!(map.remove(1), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(2) }, item: 1 }),
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(1)));
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 2 }));
    }

    #[test]
    fn test_remove_compress() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(0, 1), None);
        assert_eq!(map.insert(2, 3), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(2) }, item: 1 }),
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 3 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(1)));
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 2 }));
        assert_eq!(map.remove(2).unwrap(), 3);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 1 })]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 0 }));
        assert_eq!(map.remove(0).unwrap(), 1);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_empty(data);
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &None);
    }

    #[test]
    fn test_insert() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(1, 2), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 2 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 1 }));
        assert_eq!(map.insert(3, 4), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_head(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 2 }),
                free_tail(0),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(1), next: None }, item: 4 })
            ]
        );
        assert_eq!(freelist, &Some(List { head: 0, tail: 2 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 3 }));
        assert_eq!(map.insert(0, 1), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 1 }),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 2 }),
                free_none(),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(0) },
                    item: 4
                })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(2)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 0 }));
        assert_eq!(map.insert(3, 5).unwrap(), 4);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(3) },
                    item: 1
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(0) }, item: 2 }),
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 5 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(2)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 3 }));
    }

    #[test]
    fn test_insert_gap() {
        // Regression test for https://fxbug.dev/42171085: a sequence of inserts that creates a run of
        // free elements with size > 1 followed by removes can result in `freelist` = None even
        // though `data` contains ListLink entries.
        let mut map = DenseMap::new();
        assert_eq!(map.insert(0, 0), None);
        assert_eq!(map.insert(3, 5), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 0 }),
                free_head(2),
                free_tail(1),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(0), next: None }, item: 5 })
            ]
        );
        assert_eq!(freelist, &Some(List { head: 1, tail: 2 }));
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 3 }));

        assert_eq!(map.push(6), 1);
        assert_eq!(map.remove(1), Some(6));
        assert_eq!(map.remove(3), Some(5));

        // The remove() call compresses the list, which leaves just the 0 element.
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 0 })]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 0, tail: 0 }));
    }

    #[test]
    fn test_key_ordered_iter() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(1, 0), None);
        assert_eq!(map.insert(3, 1), None);
        assert_eq!(map.insert(6, 2), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_head(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 0 }),
                free(0, 4),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(6) },
                    item: 1
                }),
                free(2, 5),
                free_tail(4),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 2 }),
            ]
        );
        assert_eq!(freelist, &Some(List { head: 0, tail: 5 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 6 }));
        let mut c = 0;
        let mut last_k = None;
        for (i, (k, v)) in map.key_ordered_iter().enumerate() {
            assert_eq!(i, *v as usize);
            assert_eq!(map.get(k).unwrap(), v);
            assert!(last_k < Some(k));
            last_k = Some(k);
            c += 1;
        }
        assert_eq!(c, 3);
    }

    #[test]
    fn test_insertion_ordered_iter() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(6, 0), None);
        assert_eq!(map.insert(3, 2), None);
        assert_eq!(map.push(1), 0);
        assert_eq!(map.insertion_ordered_iter().collect::<Vec<_>>(), [(6, &0), (3, &2), (0, &1)]);

        assert_eq!(map.insert(3, 0), Some(2));
        assert_eq!(map.insertion_ordered_iter().collect::<Vec<_>>(), [(6, &0), (0, &1), (3, &0)]);
    }

    #[test]
    fn test_iter_mut() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(1, 0), None);
        assert_eq!(map.insert(3, 1), None);
        assert_eq!(map.insert(6, 2), None);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_head(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 0 }),
                free(0, 4),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(6) },
                    item: 1
                }),
                free(2, 5),
                free_tail(4),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 2 }),
            ]
        );
        assert_eq!(freelist, &Some(List { head: 0, tail: 5 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 6 }));
        let mut last_k = None;
        for (k, v) in map.key_ordered_iter_mut() {
            *v += k as u32;
            assert!(last_k < Some(k));
            last_k = Some(k);
        }
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_head(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 1 }),
                free(0, 4),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(6) },
                    item: 4
                }),
                free(2, 5),
                free_tail(4),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 8 }),
            ]
        );
        assert_eq!(freelist, &Some(List { head: 0, tail: 5 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 6 }));
    }

    #[test]
    fn test_into_iter() {
        let mut map = DenseMap::new();
        assert_eq!(map.insert(1, 0), None);
        assert_eq!(map.insert(3, 1), None);
        assert_eq!(map.insert(6, 2), None);
        assert_eq!(map.into_iter().collect::<Vec<_>>(), &[(1, 0), (3, 1), (6, 2)]);
    }

    #[test]
    fn test_key_ordered_retain() {
        // First, test that removed entries are actually removed, and that the
        // remaining entries are actually left there.

        let mut map = DenseMap::new();
        for i in 0..8 {
            assert_eq!(map.push(i), i);
        }

        // Keep only the odd entries.
        map.key_ordered_retain(|x: &usize| *x % 2 != 0);
        let remaining: Vec<_> =
            map.key_ordered_iter().map(|(key, entry)| (key, entry.clone())).collect();
        assert_eq!(remaining.as_slice(), [(1, 1), (3, 3), (5, 5), (7, 7)]);

        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &[
                free_tail(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 1 }),
                free(4, 0),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(5) },
                    item: 3
                }),
                free(6, 2),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(3), next: Some(7) },
                    item: 5
                }),
                free_head(4),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(5), next: None }, item: 7 }),
            ]
        );
        assert_eq!(freelist, &Some(List { head: 6, tail: 0 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 7 }));

        // Make sure that the underlying vector gets compressed.
        map.key_ordered_retain(|x| *x < 5);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &[
                free_tail(2),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: Some(3) }, item: 1 }),
                free_head(0),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(1), next: None }, item: 3 }),
            ]
        );
        assert_eq!(freelist, &Some(List { head: 2, tail: 0 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 3 }));
    }

    #[test]
    fn test_entry() {
        let mut map = DenseMap::new();
        assert_eq!(*map.entry(1).or_insert(2), 2);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 2 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List::singleton(1)));
        assert_eq!(
            *map.entry(1)
                .and_modify(|v| {
                    *v = 10;
                })
                .or_insert(5),
            10
        );
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 10 })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List::singleton(1)));
        assert_eq!(
            *map.entry(2)
                .and_modify(|v| {
                    *v = 10;
                })
                .or_insert(5),
            5
        );
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(1), next: None }, item: 5 }),
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 2 }));
        assert_eq!(*map.entry(4).or_default(), 0);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_head(3),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(4) },
                    item: 5
                }),
                free_tail(0),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(2), next: None }, item: 0 })
            ]
        );
        assert_eq!(freelist, &Some(List { head: 0, tail: 3 }));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 4 }));
        assert_eq!(*map.entry(3).or_insert_with(|| 7), 7);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(4) },
                    item: 5
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(4), next: None }, item: 7 }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(2), next: Some(3) },
                    item: 0
                })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 3 }));
        assert_eq!(*map.entry(0).or_insert(1), 1);
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 1 }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(4) },
                    item: 5
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(4), next: Some(0) },
                    item: 7
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(2), next: Some(3) },
                    item: 0
                })
            ]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 0 }));
        match map.entry(0) {
            Entry::Occupied(mut e) => {
                assert_eq!(*e.key(), 0);
                assert_eq!(*e.get(), 1);
                *e.get_mut() = 2;
                assert_eq!(*e.get(), 2);
                assert_eq!(e.remove(), 2);
            }
            _ => panic!("Wrong entry type, should be occupied"),
        }
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                free_none(),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(4) },
                    item: 5
                }),
                Allocated(AllocatedEntry { link: ListLink { prev: Some(4), next: None }, item: 7 }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(2), next: Some(3) },
                    item: 0
                })
            ]
        );
        assert_eq!(freelist, &Some(List::singleton(0)));
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 3 }));

        match map.entry(0) {
            Entry::Vacant(e) => {
                assert_eq!(*e.key(), 0);
                assert_eq!(*e.insert(4), 4);
            }
            _ => panic!("Wrong entry type, should be vacant"),
        }
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_eq!(
            data,
            &vec![
                Allocated(AllocatedEntry { link: ListLink { prev: Some(3), next: None }, item: 4 }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: None, next: Some(2) },
                    item: 10
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(1), next: Some(4) },
                    item: 5
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(4), next: Some(0) },
                    item: 7
                }),
                Allocated(AllocatedEntry {
                    link: ListLink { prev: Some(2), next: Some(3) },
                    item: 0
                })
            ]
        );
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &Some(List { head: 1, tail: 0 }));
    }

    #[test]
    fn test_freelist_order() {
        let mut rng = crate::testutil::new_rng(1234981);
        const NELEMS: usize = 1_000;
        for _ in 0..1_000 {
            let mut map = DenseMap::new();
            for i in 0..NELEMS {
                assert_eq!(map.push(i), i);
            }
            // Don't remove the last one to prevent compressing.
            let mut remove_seq: Vec<usize> = (0..NELEMS - 1).collect();
            remove_seq.shuffle(&mut rng);
            for i in &remove_seq {
                assert_ne!(map.remove(*i), None);
            }
            for i in remove_seq.iter().rev() {
                // We should be able to push into the array in the same order.
                assert_eq!(map.push(*i), *i);
            }
            assert_ne!(map.remove(NELEMS - 1), None);
            for i in &remove_seq {
                assert_ne!(map.remove(*i), None);
            }
            assert_empty(map.key_ordered_iter());
        }
    }

    #[test]
    fn test_compress_freelist() {
        let mut map = DenseMap::new();
        for i in 0..100 {
            assert_eq!(map.push(0), i);
        }
        for i in 0..100 {
            assert_eq!(map.remove(i), Some(0));
        }
        let DenseMap { data, freelist, allocatedlist } = &map;
        assert_empty(data.iter());
        assert_eq!(freelist, &None);
        assert_eq!(allocatedlist, &None);
    }

    #[test]
    fn test_insert_beyond_end_freelist() {
        let mut map = DenseMap::new();
        for i in 0..10 {
            assert_eq!(map.insert(2 * i + 1, 0), None);
        }
        for i in 0..10 {
            assert_eq!(map.push(1), 2 * i);
        }
    }

    #[test]
    fn test_double_free() {
        const MAX_KEY: usize = 100;
        let mut map1 = DenseMap::new();
        assert_eq!(map1.insert(MAX_KEY, 2), None);
        let mut map2 = DenseMap::new();
        assert_eq!(map2.insert(MAX_KEY, 2), None);
        for i in 0..MAX_KEY {
            assert_eq!(map1.remove(i), None);
            // Removing an already free entry should be a no-op.
            assert_eq!(map1.data, map2.data);
            assert_eq!(map1.freelist, map2.freelist);
        }
    }

    #[derive(Debug)]
    enum Operation<K, V> {
        Get { key: K },
        Insert { key: K, value: V },
        Remove { key: K },
        Push { value: V },
    }

    impl<V> Operation<usize, V>
    where
        V: Copy + core::cmp::PartialEq + core::fmt::Debug,
    {
        fn apply(self, map: &mut DenseMap<V>, source_of_truth: &mut HashMap<usize, V>) {
            match self {
                Self::Get { key } => {
                    assert_eq!(
                        map.get(key),
                        source_of_truth.get(&key),
                        "key={} map.get == truth.get",
                        key
                    );
                }
                Self::Insert { key, value } => {
                    assert_eq!(
                        map.insert(key, value),
                        source_of_truth.insert(key, value),
                        "key={}, map.insert == truth.insert",
                        key
                    );
                }
                Self::Remove { key } => {
                    assert_eq!(
                        map.remove(key),
                        source_of_truth.remove(&key),
                        "key={} map.remove == truth.remove",
                        key,
                    );
                }
                Self::Push { value } => {
                    let key = map.push(value);
                    assert_eq!(
                        source_of_truth.insert(key, value),
                        None,
                        "pushed key={}, value={:?}",
                        key,
                        value
                    );
                }
            }
        }
    }

    use proptest::strategy::Strategy;

    fn operation_strategy() -> impl Strategy<Value = Operation<usize, i32>> {
        let key_strategy = || 0..20usize;
        // Use a small range for values since we don't do anything fancy with them
        // so a larger range probably won't expose additional issues.
        let value_strategy = || 0..10i32;

        proptest::prop_oneof![
            key_strategy().prop_map(|key| Operation::Get { key }),
            (key_strategy(), value_strategy())
                .prop_map(|(key, value)| Operation::Insert { key, value }),
            key_strategy().prop_map(|key| Operation::Remove { key }),
            value_strategy().prop_map(|value| Operation::Push { value }),
        ]
    }

    fn find_elements<T>(
        data: &[DenseMapEntry<T>],
        get_link: impl Fn(&DenseMapEntry<T>) -> Option<&ListLink>,
    ) -> Vec<usize> {
        let head = data.iter().enumerate().find_map(|(i, e)| {
            let link = get_link(e)?;
            let ListLink { prev, next: _ } = link;
            if prev == &None {
                Some((i, *link))
            } else {
                None
            }
        });
        let mut found = Vec::new();
        let mut next = head;

        // Traverse the free list, collecting all indices into `found`.
        while let Some((index, link)) = next {
            found.push(index);
            next = link.next.map(|next_i| {
                let next_entry =
                    *get_link(&data[next_i]).expect("entry should match target link type");
                assert_eq!(Some(index), next_entry.prev, "data[{}] and data[{}]", index, next_i);
                (next_i, next_entry)
            })
        }

        // The freelist should contain all of the free data elements.
        data.iter().enumerate().for_each(|(i, e)| {
            assert_eq!(
                found.contains(&i),
                get_link(e).is_some(),
                "data[{}] should be in found list if link type matches",
                i,
            )
        });
        found
    }

    /// Searches through the given data entries to identify the free list. Returns the indices of
    /// elements in the free list in order, panicking if there is any inconsistency in the list.
    fn find_free_elements<T>(data: &[DenseMapEntry<T>]) -> Vec<usize> {
        find_elements(data, |entry| match entry {
            DenseMapEntry::Free(link) => Some(link),
            DenseMapEntry::Allocated(_) => None,
        })
    }

    /// Searches through the given data entries to identify the allocated list. Returns the indices of
    /// elements in the allocated list in order, panicking if there is any inconsistency in the list.
    fn find_allocated_elements<T>(data: &[DenseMapEntry<T>]) -> Vec<usize> {
        find_elements(data, |entry| match entry {
            DenseMapEntry::Allocated(AllocatedEntry { item: _, link }) => Some(link),
            DenseMapEntry::Free(_) => None,
        })
    }

    #[test]
    fn test_find_free_elements() {
        let data = vec![
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 1 }),
            free_tail(2),
            free(3, 1),
            free_head(2),
        ];
        assert_eq!(find_free_elements(&data), vec![3, 2, 1]);
    }

    #[test]
    fn test_find_free_elements_none_free() {
        let data = vec![
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 1 }),
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 2 }),
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 3 }),
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 2 }),
        ];
        assert_eq!(find_free_elements(&data), vec![]);
    }

    #[test]
    #[should_panic(expected = "entry should match target link type")]
    fn test_find_free_elements_includes_allocated() {
        let data = vec![
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 1 }),
            free_head(0),
            free_tail(0),
        ];
        let _ = find_free_elements(&data);
    }

    #[test]
    #[should_panic(expected = "should be in found list if link type matches")]
    fn test_find_free_elements_in_cycle() {
        let data = vec![
            free(2, 1),
            free(0, 2),
            free(1, 0),
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 5 }),
        ];
        let _ = find_free_elements(&data);
    }

    #[test]
    #[should_panic(expected = "should be in found list if link type matches")]
    fn test_find_free_elements_multiple_lists() {
        let data = vec![
            free_head(1),
            free_tail(0),
            Allocated(AllocatedEntry { link: ListLink { prev: None, next: None }, item: 13 }),
            free_head(4),
            free_tail(3),
        ];
        let _ = find_free_elements(&data);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            // Add all failed seeds here.
            failure_persistence: proptest_support::failed_seeds!(),
            ..proptest::test_runner::Config::default()
        })]

        #[test]
        fn test_arbitrary_operations(operations in proptest::collection::vec(operation_strategy(), 10)) {
            let mut map = DenseMap::new();
            let mut reference = HashMap::new();
            for op in operations {
                op.apply(&mut map, &mut reference);

                // Now check the invariants that the map should be guaranteeing.
                let DenseMap { data, freelist, allocatedlist } = &map;

                match freelist {
                    None => {
                        // No freelist means all nodes are allocated.
                        data.iter().enumerate().for_each(|(i, d)| match d {
                            DenseMapEntry::Free(_) => panic!("no freelist but data[{}] is free", i),
                            DenseMapEntry::Allocated(_) => (),
                        })
                    },
                    Some(List {head, tail}) => {
                        let free = find_free_elements(data);
                        assert_eq!(free.first(), Some(head));
                        assert_eq!(free.last(), Some(tail));
                    }
                }

                match allocatedlist {
                    None => {
                        // No allocatedlist means all nodes are free.
                        data.iter().enumerate().for_each(|(i, d)| match d {
                            DenseMapEntry::Allocated(_) => panic!("no allocatedlist but data[{}] is allocated", i),
                            DenseMapEntry::Free(_) => (),
                        })
                    },
                    Some(List {head, tail}) => {
                        let allocated = find_allocated_elements(data);
                        assert_eq!(allocated.first(), Some(head));
                        assert_eq!(allocated.last(), Some(tail));
                    }
                }
            }

            // After all operations have completed, the contents of the map should match the source of truth.
            let elements : HashMap<_, i32> = map.key_ordered_iter().map(|(a, b)| (a, *b)).collect();
            assert_eq!(elements, reference);
        }
    }
}
