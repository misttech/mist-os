// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use arrayvec::ArrayVec;
use std::borrow::Borrow;
use std::cmp::{Eq, PartialEq};
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::ops::{Bound, Range, RangeBounds};
use std::sync::Arc;

/// The `B` constant for the btree.
///
/// Controls the size of the nodes inside the tree.
const B: usize = 6;

/// The capacity of nodes in the btree.
const NODE_CAPACITY: usize = 2 * B;

/// A location inside the btree.
#[derive(Debug, Default, Clone, Copy)]
struct Cursor {
    /// The number of valid indices in the `indices` array.
    depth: u8,

    /// The indices of the entry, ordered from leaf to root.
    indices: [u8; 7],
}

impl Cursor {
    /// Create a cursor with a single index.
    fn with_index(index: usize) -> Self {
        let mut cursor = Self::default();
        cursor.push(index);
        cursor
    }

    /// Whether the cursor is empty.
    ///
    /// A cursor is empty if it contains no more indices. This happens when a traversal has reached
    /// a leaf node.
    fn is_empty(&self) -> bool {
        self.depth == 0
    }

    /// Push an index onto the front of the cursor.
    ///
    /// The front of the cursor is towards the root of the tree.
    fn push(&mut self, index: usize) {
        self.indices[self.depth as usize] = index as u8;
        self.depth += 1;
    }

    /// Push an index onto the back of the cursor.
    ///
    /// The back of the cursor is towards the leaves of the tree.
    fn push_back(&mut self, index: usize) {
        self.indices.rotate_right(1);
        self.indices[0] = index as u8;
        self.depth += 1;
    }

    /// Pop an index off the front of the cursor.
    ///
    /// The front of the cursor is towards the root of the tree.
    fn pop(&mut self) -> Option<usize> {
        if self.depth == 0 {
            None
        } else {
            self.depth -= 1;
            Some(self.indices[self.depth as usize] as usize)
        }
    }

    /// Pop an index off the back of the cursor.
    ///
    /// The back of the cursor is towards the leaves of the tree.
    fn pop_back(&mut self) -> Option<usize> {
        if self.depth == 0 {
            None
        } else {
            self.depth -= 1;
            let index = self.indices[0] as usize;
            self.indices.rotate_left(1);
            Some(index)
        }
    }

    /// The backmost index in the cursor.
    ///
    /// The back of the cursor is towards the leaves of the tree.
    ///
    /// Assumes the cursor is non-empty.
    fn back(&self) -> usize {
        self.indices[0] as usize
    }

    /// Increment the backmost index in the cursor.
    ///
    /// The back of the cursor is towards the leaves of the tree.
    ///
    /// Assumes the cursor is non-empty.
    fn increment_back(&mut self) {
        self.indices[0] += 1;
    }

    /// Decrement the backmost index in the cursor.
    ///
    /// The back of the cursor is towards the leaves of the tree.
    ///
    /// Assumes the cursor is non-empty.
    fn decrement_back(&mut self) {
        self.indices[0] -= 1;
    }
}

impl PartialEq for Cursor {
    fn eq(&self, other: &Self) -> bool {
        if self.depth != other.depth {
            return false;
        }
        for i in 0..self.depth {
            if self.indices[i as usize] != other.indices[i as usize] {
                return false;
            }
        }
        true
    }
}

impl Eq for Cursor {}

/// Where to place the cursor relative to the given key.
enum CursorPosition {
    /// The given key represents a left edge of a range.
    ///
    /// Place the cursor to the left of a range containing the cursor.
    Left,

    /// The given key represents a right edge of a range.
    ///
    /// Place the cursor to the right of a range containing the cursor.
    Right,
}

/// Search of the given key in the given array of ranges.
///
/// If the array contains a range that contains the key, returns the index of that range.
/// Otherwise, returns the index at which the given key could be inserted into the array to
/// maintain the ordering.
fn binary_search<K: Ord>(key: &K, keys: &ArrayVec<Range<K>, NODE_CAPACITY>) -> usize {
    let mut left = 0usize;
    let mut right = keys.len();
    while left < right {
        let mid = left + (right - left) / 2;
        // TODO: Consider `get_unchecked`.
        let range = &keys[mid];
        if key < &range.start {
            // This range is too large.
            right = mid;
        } else if key < &range.end {
            // We found the range that contains this key.
            return mid;
        } else {
            // The key might be found in the next range.
            left = mid + 1;
        }
    }
    // The key falls between two ranges. Return the index at which this key could be inserted to
    // maintain the ordering.
    left
}

/// A leaf node in the btree.
///
/// Stores a flat map of keys to values, with the `i`th entry in the keys array corresponding to
/// the `i`th entry in the values array. The balancing rules of the btree ensure that every
/// non-root leaf has between N and N/2 entries populated.
#[derive(Clone)]
struct NodeLeaf<K: Ord + Copy, V: Clone> {
    /// The keys stored in this leaf node.
    ///
    /// We store the key in a dense array to improve cache performance during lookups. We often
    /// need to binary-search the keys in a given leaf node, which means having those keys close
    /// together improves cache performance.
    keys: ArrayVec<Range<K>, NODE_CAPACITY>,

    /// The value stored in this leaf node.
    values: ArrayVec<V, NODE_CAPACITY>,
}

/// Shows the map structure of the leaf node.
impl<K, V> Debug for NodeLeaf<K, V>
where
    K: Debug + Ord + Copy,
    V: Debug + Clone,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.keys.iter().zip(self.values.iter())).finish()
    }
}

/// The result of performing an insertion into a btree node.
enum InsertResult<K: Ord + Copy, V: Clone> {
    /// The value was successfully inserted into an empty slot.
    Inserted,

    /// The value was inserted into an empty slot in a leaf node but that insertion caused the
    /// leaf node to exceed its capacity and split into two leaf nodes. The existing leaf node
    /// now holds the entries to the left of the split and the entries to the right of the split
    /// are returned. The split occurred at the returned key.
    SplitLeaf(K, Arc<NodeLeaf<K, V>>),

    /// The value was inserted into an empty slot in a subtree but that insertion caused the
    /// internal node to exceed its capacity and split into two internal nodes. The internal node
    /// now holds the entries to the left of the split and the entries to the right of the split
    /// are returned. The split occurred at the returned key.
    SplitInternal(K, Arc<NodeInternal<K, V>>),
}

/// The intermediate result of a remove operation.
///
/// The root of the CowTree converts this result value into `Option<T>`, per the usual map
/// interface.
enum RemoveResult<V: Clone> {
    /// The key the client asked to remove was not found in the map.
    NotFound,

    /// The key was successfully removed from the map.
    ///
    /// Returns the value previously stored at this key. No further processing is required.
    Removed(V),

    /// The key was removed from the map but the node that previously contained that node no longer
    /// has sufficient children.
    ///
    /// Returns the value previousoly stored at this key.
    ///
    /// The caller is responsible for rebalancing its children to ensure that each node has at
    /// least this minimum number of children. If the balance invariant can be resolved locally,
    /// the caller should return `Removed` to its caller. If rebalancing the local children
    /// causes this node to have fewer than the minimum number of children, the caller should
    /// return `Underflow` to its caller.
    Underflow(V),
}

impl<K, V> NodeLeaf<K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// Create an empty leaf node.
    ///
    /// Empty leaf nodes are used only at the root of the tree.
    fn empty() -> Self {
        Self { keys: ArrayVec::new(), values: ArrayVec::new() }
    }

    /// Gets the index in this leaf that corresponds to the given cursor.
    ///
    /// Assumes the cursor contains exactly one index.
    ///
    /// Returns `None` if the cursor points beyond the end if this node.
    fn get_index(&self, mut cursor: Cursor) -> Option<usize> {
        let index = cursor.pop().expect("Cursor has sufficient depth");
        assert!(cursor.is_empty(), "Cursor has excess depth");
        if index >= self.keys.len() {
            return None;
        }
        Some(index)
    }

    /// Search this leaf for the given key and return both the key and the value found.
    fn get_key_value(&self, cursor: Cursor) -> Option<(&Range<K>, &V)> {
        if let Some(index) = self.get_index(cursor) {
            let key = &self.keys[index];
            let value = &self.values[index];
            Some((key, value))
        } else {
            None
        }
    }

    /// The last key/value pair stored in this leaf.
    fn last_key_value(&self) -> Option<(&Range<K>, &V)> {
        let key = self.keys.last()?;
        let value = self.values.last()?;
        Some((key, value))
    }

    /// Find the given key in this node.
    ///
    /// Updates `cursor` to point to the position indicated by `position`.
    fn find(&self, key: &K, position: CursorPosition, cursor: &mut Cursor) {
        let index = binary_search(key, &self.keys);
        match position {
            CursorPosition::Left => {
                cursor.push(index);
            }
            CursorPosition::Right => {
                if let Some(range) = self.keys.get(index) {
                    if *key > range.start {
                        cursor.push(index + 1);
                        return;
                    }
                }
                cursor.push(index);
            }
        }
    }

    /// Insert the given entry at the location indicated by `cursor`.
    ///
    /// Inserting a value into a leaf node might cause this node to split into two leaf nodes.
    fn insert(&mut self, mut cursor: Cursor, range: Range<K>, value: V) -> InsertResult<K, V> {
        let index = cursor.pop().expect("valid cursor");
        if self.keys.len() == NODE_CAPACITY {
            if index == NODE_CAPACITY {
                let mut keys = ArrayVec::new();
                let mut values = ArrayVec::new();
                let key = range.start;
                keys.push(range);
                values.push(value);
                return InsertResult::SplitLeaf(key, Arc::new(Self { keys, values }));
            }
            let middle = NODE_CAPACITY / 2;
            assert!(middle > 0);
            let mut right = Self {
                keys: self.keys.drain(middle..).collect(),
                values: self.values.drain(middle..).collect(),
            };
            if index <= middle {
                self.keys.insert(index, range);
                self.values.insert(index, value);
            } else {
                right.keys.insert(index - middle, range);
                right.values.insert(index - middle, value);
            }
            InsertResult::SplitLeaf(right.keys[0].start, Arc::new(right))
        } else {
            self.keys.insert(index, range);
            self.values.insert(index, value);
            InsertResult::Inserted
        }
    }

    /// Remove the entry indicated by `cursor`.
    fn remove(&mut self, cursor: Cursor) -> RemoveResult<V> {
        if let Some(index) = self.get_index(cursor) {
            self.keys.remove(index);
            let value = self.values.remove(index);
            if self.keys.len() < NODE_CAPACITY / 2 {
                RemoveResult::Underflow(value)
            } else {
                RemoveResult::Removed(value)
            }
        } else {
            RemoveResult::NotFound
        }
    }
}

/// The children of an internal node in the btree.
#[derive(Clone, Debug)]
enum ChildList<K: Ord + Copy, V: Clone> {
    /// Used when an internal node has leaf nodes as children.
    Leaf(ArrayVec<Arc<NodeLeaf<K, V>>, NODE_CAPACITY>),

    /// Used when an internal node has other internal nodes as children.
    Internal(ArrayVec<Arc<NodeInternal<K, V>>, NODE_CAPACITY>),
}

impl<K, V> ChildList<K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// Create a child list that has no children.
    fn new_empty(&self) -> Self {
        match self {
            ChildList::Leaf(_) => ChildList::Leaf(ArrayVec::new()),
            ChildList::Internal(_) => ChildList::Internal(ArrayVec::new()),
        }
    }

    /// The number of children for this node.
    fn len(&self) -> usize {
        match self {
            ChildList::Leaf(children) => children.len(),
            ChildList::Internal(children) => children.len(),
        }
    }

    /// The number of gradchildren located at the child with the given index.
    fn size_at(&self, i: usize) -> usize {
        match self {
            ChildList::Leaf(children) => children[i].keys.len(),
            ChildList::Internal(children) => children[i].children.len(),
        }
    }

    /// Obtain the child located at the given index.
    fn get(&self, i: usize) -> Node<K, V> {
        match self {
            ChildList::Leaf(children) => Node::Leaf(children[i].clone()),
            ChildList::Internal(children) => Node::Internal(children[i].clone()),
        }
    }

    /// Get a reference to the child located at the given index.
    fn get_ref(&self, i: usize) -> NodeRef<'_, K, V> {
        match self {
            ChildList::Leaf(children) => NodeRef::Leaf(&children[i]),
            ChildList::Internal(children) => NodeRef::Internal(&children[i]),
        }
    }

    /// Removes all the entries starting at the given index from the child list.
    ///
    /// The removed entries are returned in a new child list.
    fn split_off(&mut self, index: usize) -> Self {
        match self {
            ChildList::Leaf(children) => ChildList::Leaf(children.drain(index..).collect()),
            ChildList::Internal(children) => ChildList::Internal(children.drain(index..).collect()),
        }
    }

    /// Removes all the entries prior to the given index from the child list.
    ///
    /// The removed entries are returned in a new child list.
    fn split_off_front(&mut self, index: usize) -> Self {
        match self {
            ChildList::Leaf(children) => ChildList::Leaf(children.drain(..index).collect()),
            ChildList::Internal(children) => ChildList::Internal(children.drain(..index).collect()),
        }
    }

    /// Insert a child into the child list.
    ///
    /// The type of child node must match the type of the child list.
    fn insert(&mut self, index: usize, child: Node<K, V>) {
        match self {
            ChildList::Leaf(children) => {
                let Node::Leaf(leaf) = child else {
                    unreachable!("Inserting a non-leaf into an internal node for leaf nodes.");
                };
                children.insert(index, leaf);
            }
            ChildList::Internal(children) => {
                let Node::Internal(internal) = child else {
                    unreachable!(
                        "Inserting a non-internal into an internal node for internal nodes."
                    );
                };
                children.insert(index, internal);
            }
        }
    }

    /// Remove the child at the given index from the child list.
    fn remove(&mut self, index: usize) {
        match self {
            ChildList::Leaf(children) => {
                children.remove(index);
            }
            ChildList::Internal(children) => {
                children.remove(index);
            }
        }
    }

    /// Add the children from the given `ChildList` to this child list.
    fn extend(&mut self, other: &Self) {
        match (self, other) {
            (ChildList::Leaf(self_children), ChildList::Leaf(other_children)) => {
                self_children.extend(other_children.iter().cloned());
            }
            (ChildList::Internal(self_children), ChildList::Internal(other_children)) => {
                self_children.extend(other_children.iter().cloned());
            }
            _ => unreachable!("Type mismatch while extending a child list."),
        }
    }
}

/// An internal node in the btree.
#[derive(Clone, Debug)]
struct NodeInternal<K: Ord + Copy, V: Clone> {
    /// A cache of the keys that partition the keys in the children.
    /// The key at index `i` is the smallest key stored in the subtree
    /// of the `i`+1 child.
    ///
    /// We only ever store CAPACITY - 1 keys in this array.
    keys: ArrayVec<K, NODE_CAPACITY>,

    /// The children of this node.
    children: ChildList<K, V>,
}

/// Get mutable references to two entries in a slice.
///
/// When rebalancing nodes, we need to mutate two nodes at the same time. Normally, if you take a
/// mutable reference to one element of an array, the borrow checker prevents you from taking a
/// mutable reference to a second element of the same array.
///
/// The nightly version of `std::primitive::slice` has `get_many_mut` to let you get mutable
/// references to multiple elements. However, without that interface, the recommended approach for
/// avoiding `unsafe` is to use `split_at_mut`.
fn get_two_mut<T>(slice: &mut [T], i: usize, j: usize) -> (&mut T, &mut T) {
    if i < j {
        let (a, b) = slice.split_at_mut(j);
        (&mut a[i], &mut b[0])
    } else {
        let (a, b) = slice.split_at_mut(i);
        (&mut b[0], &mut a[j])
    }
}

impl<K, V> NodeInternal<K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// The index of the child that might contain the given key.
    ///
    /// Searches the cached keys at this node to determine which child node might contain the given
    /// key.
    fn get_child_index(&self, key: &K) -> usize {
        let p = self.keys.partition_point(|k| k < key);
        if self.keys.get(p) == Some(key) {
            // If the query key exactly matches the split key, then we need to look for this key to
            // the right of the split.
            p + 1
        } else {
            // Otherwise, we look to the left of the split.
            p
        }
    }

    /// Search this subtree for the given key and return both the key and the value found.
    fn get_key_value(&self, mut cursor: Cursor) -> Option<(&Range<K>, &V)> {
        let index = cursor.pop().expect("valid cursor");
        match &self.children {
            ChildList::Leaf(children) => children[index].get_key_value(cursor),
            ChildList::Internal(children) => children[index].get_key_value(cursor),
        }
    }

    /// Returns a reference to the node that contains the entry indicated by the cursor.
    ///
    /// Assumes the cursor points a descendant of this node.
    fn get_containing_node(&self, mut cursor: Cursor) -> NodeRef<'_, K, V> {
        debug_assert!(cursor.depth >= 2);
        let index = cursor.pop().expect("valid cursor");
        if cursor.depth == 1 {
            return self.children.get_ref(index);
        }
        match &self.children {
            ChildList::Leaf(_) => unreachable!("leaf nodes do not have children"),
            ChildList::Internal(children) => children[index].get_containing_node(cursor),
        }
    }

    /// The last key/value pair stored in this subtree.
    fn last_key_value(&self) -> Option<(&Range<K>, &V)> {
        match &self.children {
            ChildList::Leaf(children) => {
                children.last().expect("child lists are always non-empty").last_key_value()
            }
            ChildList::Internal(children) => {
                children.last().expect("child lists are always non-empty").last_key_value()
            }
        }
    }

    /// Find the given key in this node.
    ///
    /// Updates `cursor` to point to the position indicated by `position`.
    fn find(&self, key: &K, position: CursorPosition, cursor: &mut Cursor) {
        let index = self.get_child_index(&key);
        match &self.children {
            ChildList::Leaf(children) => children[index].find(key, position, cursor),
            ChildList::Internal(children) => children[index].find(key, position, cursor),
        }
        cursor.push(index);
    }

    /// Insert the given child node at `index` in this node.
    ///
    /// `key` must be the smallest key that occurs in the `child` subtree.
    ///
    /// The caller must ensure that the child is inserted in the correct location.
    fn insert_child(&mut self, index: usize, key: K, child: Node<K, V>) -> InsertResult<K, V> {
        let n = self.children.len();
        if n == NODE_CAPACITY {
            if index == NODE_CAPACITY {
                let mut children = self.children.new_empty();
                children.insert(0, child);
                let right = Self { keys: ArrayVec::new(), children };
                return InsertResult::SplitInternal(key, Arc::new(right));
            }
            let middle = NODE_CAPACITY / 2;
            assert!(middle > 0);
            let mut internal = Self {
                keys: self.keys.drain(middle..).collect(),
                children: self.children.split_off(middle),
            };
            let split_key = self.keys.pop().unwrap();
            if index < middle {
                self.keys.insert(index, key);
                self.children.insert(index + 1, child);
            } else {
                internal.keys.insert(index - middle, key);
                internal.children.insert(index - middle + 1, child);
            }
            debug_assert!(self.keys.len() + 1 == self.children.len());
            debug_assert!(internal.keys.len() + 1 == internal.children.len());
            InsertResult::SplitInternal(split_key, Arc::new(internal))
        } else {
            self.keys.insert(index, key);
            self.children.insert(index + 1, child);
            debug_assert!(self.keys.len() + 1 == self.children.len());
            InsertResult::Inserted
        }
    }

    /// Insert the given entry at the location indicated by `cursor`.
    ///
    /// Inserting a value into an internal node might cause this node to split into two internal
    /// nodes.
    fn insert(&mut self, mut cursor: Cursor, range: Range<K>, value: V) -> InsertResult<K, V> {
        let index = cursor.pop().expect("valid cursor");
        let result = match &mut self.children {
            ChildList::Leaf(children) => {
                Arc::make_mut(&mut children[index]).insert(cursor, range, value)
            }
            ChildList::Internal(children) => {
                Arc::make_mut(&mut children[index]).insert(cursor, range, value)
            }
        };
        match result {
            InsertResult::Inserted => InsertResult::Inserted,
            InsertResult::SplitLeaf(key, right) => self.insert_child(index, key, Node::Leaf(right)),
            InsertResult::SplitInternal(key, right) => {
                self.insert_child(index, key, Node::Internal(right))
            }
        }
    }

    /// Determine whether to rebalance the child with the given index to the left or to the right.
    ///
    /// Given a choice, we will rebalance the child with its larger neighbor.
    ///
    /// The indices returned are always sequential.
    fn select_children_to_rebalance(&self, index: usize) -> (usize, usize) {
        if index == 0 {
            (index, index + 1)
        } else if index == self.children.len() - 1 {
            (index - 1, index)
        } else {
            let left_index = index - 1;
            let left_size = self.children.size_at(left_index);
            let right_index = index + 1;
            let right_size = self.children.size_at(right_index);
            if left_size > right_size {
                (left_index, index)
            } else {
                (index, right_index)
            }
        }
    }

    /// Rebalance the child at the given index.
    ///
    /// If the child and its neighbor are sufficiently small, this function will merge them into a
    /// single node.
    fn rebalance_child(&mut self, index: usize) {
        // Cannot rebalance if we have fewer than two children. This situation occurs only at the
        // root of the tree.
        if self.children.len() < 2 {
            return;
        }
        let (left, right) = self.select_children_to_rebalance(index);
        let n = self.children.size_at(left) + self.children.size_at(right);
        match &mut self.children {
            ChildList::Leaf(children) => {
                let (left_shard_node, right_shared_node) = get_two_mut(children, left, right);
                let left_node = Arc::make_mut(left_shard_node);
                if n <= NODE_CAPACITY {
                    // Merge the right node into the left node.
                    left_node.keys.extend(right_shared_node.keys.iter().cloned());
                    left_node.values.extend(right_shared_node.values.iter().cloned());
                    self.keys.remove(left);
                    self.children.remove(right);
                } else {
                    // Rebalance the elements between the nodes.
                    let split = n / 2;
                    let right_node = Arc::make_mut(right_shared_node);
                    if left_node.values.len() < split {
                        // Move elements from right to left.
                        let move_count = split - left_node.values.len();
                        left_node.keys.extend(right_node.keys.drain(..move_count));
                        left_node.values.extend(right_node.values.drain(..move_count));
                    } else {
                        // Move elements from left to right.
                        let mut keys = ArrayVec::new();
                        keys.extend(left_node.keys.drain(split..));
                        keys.extend(right_node.keys.iter().cloned());
                        right_node.keys = keys;

                        let mut values = ArrayVec::new();
                        values.extend(left_node.values.drain(split..));
                        values.extend(right_node.values.iter().cloned());
                        right_node.values = values;
                    }
                    // Update the split key to reflect the new division between the nodes.
                    self.keys[left] = right_node.keys[0].start;
                }
            }
            ChildList::Internal(children) => {
                let (left_shard_node, right_shared_node) = get_two_mut(children, left, right);
                let left_node = Arc::make_mut(left_shard_node);
                let old_split_key = &self.keys[left];
                if n <= NODE_CAPACITY {
                    // Merge the right node into the left node.
                    left_node.keys.push(old_split_key.clone());
                    left_node.keys.extend(right_shared_node.keys.iter().cloned());
                    left_node.children.extend(&right_shared_node.children);
                    debug_assert!(left_node.keys.len() + 1 == left_node.children.len());
                    self.keys.remove(left);
                    self.children.remove(right);
                } else {
                    // Rebalance the elements between the nodes.
                    let split = n / 2;
                    let split_key;
                    let right_node = Arc::make_mut(right_shared_node);
                    if left_node.children.len() < split {
                        // Move elements from right to left.
                        let move_count = split - left_node.children.len();
                        left_node.keys.push(old_split_key.clone());
                        left_node.keys.extend(right_node.keys.drain(..move_count));
                        split_key =
                            left_node.keys.pop().expect("must have moved at least one element");

                        left_node.children.extend(&right_node.children.split_off_front(move_count));
                        debug_assert!(left_node.keys.len() + 1 == left_node.children.len());
                    } else {
                        // Move elements from left to right.
                        let mut it = left_node.keys.drain((split - 1)..);
                        split_key = it.next().expect("must be moving at least one element");
                        let mut keys = ArrayVec::new();
                        keys.extend(it);
                        keys.push(old_split_key.clone());
                        keys.extend(right_node.keys.iter().cloned());
                        right_node.keys = keys;

                        let mut children = right_node.children.new_empty();
                        children.extend(&left_node.children.split_off(split));
                        children.extend(&right_node.children);
                        right_node.children = children;
                        debug_assert!(left_node.keys.len() + 1 == left_node.children.len());
                        debug_assert!(right_node.keys.len() + 1 == right_node.children.len());
                    }
                    // Update the split key to reflect the new division between the nodes.
                    self.keys[left] = split_key;
                }
            }
        }
    }

    /// Remove the entry indicated by `cursor`.
    fn remove(&mut self, mut cursor: Cursor) -> RemoveResult<V> {
        let index = cursor.pop().expect("valid cursor");
        let result = match &mut self.children {
            ChildList::Leaf(children) => Arc::make_mut(&mut children[index]).remove(cursor),
            ChildList::Internal(children) => Arc::make_mut(&mut children[index]).remove(cursor),
        };
        match result {
            RemoveResult::NotFound => RemoveResult::NotFound,
            RemoveResult::Removed(value) => RemoveResult::Removed(value),
            RemoveResult::Underflow(value) => {
                self.rebalance_child(index);
                if self.children.len() < NODE_CAPACITY / 2 {
                    RemoveResult::Underflow(value)
                } else {
                    RemoveResult::Removed(value)
                }
            }
        }
    }
}

/// A node in the btree.
#[derive(Clone, Debug)]
enum Node<K: Ord + Copy, V: Clone> {
    /// An internal node.
    Internal(Arc<NodeInternal<K, V>>),

    /// A leaf node.
    Leaf(Arc<NodeLeaf<K, V>>),
}

impl<K, V> Node<K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// The number of children stored at this node.
    fn len(&self) -> usize {
        match self {
            Node::Internal(node) => node.children.len(),
            Node::Leaf(node) => node.keys.len(),
        }
    }

    /// Search this node for the given key and return both the key and the value found.
    fn get_key_value(&self, cursor: Cursor) -> Option<(&Range<K>, &V)> {
        match self {
            Node::Leaf(node) => node.get_key_value(cursor),
            Node::Internal(node) => node.get_key_value(cursor),
        }
    }

    /// The last key/value pair stored in this node.
    fn last_key_value(&self) -> Option<(&Range<K>, &V)> {
        match self {
            Node::Leaf(node) => node.last_key_value(),
            Node::Internal(node) => node.last_key_value(),
        }
    }

    /// Converts a reference into a Node into a NodeRef.
    fn as_ref(&self) -> NodeRef<'_, K, V> {
        match self {
            Node::Internal(node) => NodeRef::Internal(node),
            Node::Leaf(node) => NodeRef::Leaf(node),
        }
    }

    /// Returns a reference to the node that contains the entry indicated by the cursor.
    ///
    /// Assumes the cursor is non-empty.
    fn get_containing_node(&self, cursor: Cursor) -> NodeRef<'_, K, V> {
        assert!(cursor.depth > 0);
        if cursor.depth == 1 {
            return self.as_ref();
        }
        match self {
            Node::Internal(node) => node.get_containing_node(cursor),
            Node::Leaf(_) => unreachable!("leaf nodes do not have children"),
        }
    }

    /// Insert the given value at the location indicated by `cursor`.
    ///
    /// If the insertion causes this node to split, the node will always split into two instances
    /// of the same type of node.
    fn insert(&mut self, cursor: Cursor, range: Range<K>, value: V) -> InsertResult<K, V> {
        match self {
            Node::Internal(node) => Arc::make_mut(node).insert(cursor, range, value),
            Node::Leaf(node) => Arc::make_mut(node).insert(cursor, range, value),
        }
    }

    /// Remove the entry indicated by `cursor`.
    fn remove(&mut self, cursor: Cursor) -> RemoveResult<V> {
        match self {
            Node::Internal(node) => Arc::make_mut(node).remove(cursor),
            Node::Leaf(node) => Arc::make_mut(node).remove(cursor),
        }
    }

    /// Find the given key in this node.
    ///
    /// Updates `cursor` to point to the position indicated by `position`.
    fn find(&self, key: &K, position: CursorPosition, cursor: &mut Cursor) {
        match self {
            Node::Internal(node) => node.find(key, position, cursor),
            Node::Leaf(node) => node.find(key, position, cursor),
        }
    }
}

/// A node in the btree.
#[derive(Clone, Debug)]
enum NodeRef<'a, K: Ord + Copy, V: Clone> {
    /// An internal node.
    Internal(&'a Arc<NodeInternal<K, V>>),

    /// A leaf node.
    Leaf(&'a Arc<NodeLeaf<K, V>>),
}

impl<'a, K, V> NodeRef<'a, K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// The number of children stored at this node.
    fn len(&self) -> usize {
        match self {
            NodeRef::Internal(node) => node.children.len(),
            NodeRef::Leaf(node) => node.keys.len(),
        }
    }
}

/// An iterator over the key-value pairs stored in a RangeMap2.
#[derive(Debug)]
pub struct Iter<'a, K: Ord + Copy, V: Clone> {
    /// The state of the forward iteration.
    ///
    /// The cursor points to the next entry to enumerate.
    forward: Cursor,

    /// The state of the backward iteration.
    ///
    /// The cursor points to the child that was most recently iterated or just past the end of the
    /// entry list if no entries have been enumerated from this leaf yet.
    backward: Cursor,

    /// The root node of the tree.
    root: &'a Node<K, V>,
}

impl<'a, K, V> Iter<'a, K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    /// Whether the iterator is complete.
    ///
    /// Iteration stops when the forward and backward cursors meet.
    fn is_done(&self) -> bool {
        self.forward == self.backward
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    type Item = (&'a Range<K>, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        while !self.is_done() {
            match self.root.get_containing_node(self.forward) {
                NodeRef::Leaf(leaf) => {
                    let index = self.forward.back();
                    if index < leaf.keys.len() {
                        let key = &leaf.keys[index];
                        let value = &leaf.values[index];
                        self.forward.increment_back();
                        return Some((key, value));
                    } else {
                        self.forward.pop_back();
                        self.forward.increment_back();
                    }
                }
                NodeRef::Internal(internal) => {
                    let index = self.forward.back();
                    if index < internal.children.len() {
                        self.forward.push_back(0);
                    } else {
                        self.forward.pop_back();
                        self.forward.increment_back();
                    }
                }
            }
        }
        None
    }
}

impl<'a, K, V> DoubleEndedIterator for Iter<'a, K, V>
where
    K: Ord + Copy,
    V: Clone,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        while !self.is_done() {
            match self.root.get_containing_node(self.backward) {
                NodeRef::Leaf(leaf) => {
                    let index = self.backward.back();
                    if index > 0 {
                        let key = &leaf.keys[index - 1];
                        let value = &leaf.values[index - 1];
                        self.backward.decrement_back();
                        return Some((key, value));
                    } else {
                        self.backward.pop_back();
                    }
                }
                NodeRef::Internal(internal) => {
                    let index = self.backward.back();
                    if index > 0 {
                        let child = internal.children.get_ref(index - 1);
                        self.backward.decrement_back();
                        self.backward.push_back(child.len());
                    } else {
                        self.backward.pop_back();
                    }
                }
            }
        }
        None
    }
}

/// A map from ranges to values.
///
/// This map can be cloned efficiently. If the map is modified after being cloned, the relevant
/// parts of the map's internal structure will be copied lazily.
#[derive(Clone, Debug)]
pub struct RangeMap2<K: Ord + Copy, V: Clone + Eq> {
    /// The root node of the tree.
    ///
    /// The root node is either a leaf of an internal node, depending on the number of entries in
    /// the map.
    node: Node<K, V>,
}

impl<K, V> Default for RangeMap2<K, V>
where
    K: Ord + Copy,
    V: Clone + Eq,
{
    fn default() -> Self {
        Self { node: Node::Leaf(Arc::new(NodeLeaf::empty())) }
    }
}

impl<K, V> RangeMap2<K, V>
where
    K: Ord + Copy,
    V: Clone + Eq,
{
    /// Whether this map contains any entries.
    pub fn is_empty(&self) -> bool {
        match &self.node {
            Node::Leaf(node) => node.keys.is_empty(),
            Node::Internal(_) => false,
        }
    }

    /// Find the given key in this node.
    ///
    /// Returns a Cursor that points to the position indicated by `position`.
    fn find(&self, key: &K, position: CursorPosition) -> Cursor {
        let mut cursor = Cursor::default();
        self.node.find(key, position, &mut cursor);
        cursor
    }

    /// If the entry indicated by the cursor contains `key`, returns the range and value stored at
    /// that entry.
    fn get_if_contains_key(&self, key: &K, cursor: Cursor) -> Option<(&Range<K>, &V)> {
        if let Some((range, value)) = self.node.get_key_value(cursor) {
            if range.contains(key) {
                return Some((range, value));
            }
        }
        None
    }

    /// Searches the map for a range that contains the given key.
    ///
    /// Returns the range and value if such a range is found.
    pub fn get(&self, key: K) -> Option<(&Range<K>, &V)> {
        self.get_if_contains_key(&key, self.find(&key, CursorPosition::Left))
    }

    /// The last range stored in this map.
    pub fn last_range(&self) -> Option<&Range<K>> {
        self.node.last_key_value().map(|(key, _)| key)
    }

    /// Searches the map for a range that contains the given key.
    ///
    /// If such a range is found, returns a cursor to that entry, the range, and the value.
    fn get_cursor_key_value(&mut self, key: &K) -> Option<(Cursor, Range<K>, V)> {
        let cursor = self.find(key, CursorPosition::Left);
        self.get_if_contains_key(key, cursor)
            .map(|(range, value)| (cursor, range.clone(), value.clone()))
    }

    /// Remove the entry with the given key from the map.
    ///
    /// If the key was present in the map, returns the value previously stored at the given key.
    pub fn remove(&mut self, range: Range<K>) -> Vec<V> {
        let mut removed_values = vec![];

        if range.is_empty() {
            return removed_values;
        }

        if let Some((cursor, old_range, v)) = self.get_cursor_key_value(&range.start) {
            // Remove that range from the map.
            if let Some(value) = self.remove_at(cursor) {
                removed_values.push(value);
            };

            // If the removed range extends after the end of the given range,
            // re-insert the part of the old range that extends beyond the end
            // of the given range.
            if old_range.end > range.end {
                self.insert_range_internal(range.end..old_range.end, v.clone());
            }

            // If the removed range extends before the start of the given
            // range, re-insert the part of the old range that extends before
            // the start of the given range.
            if old_range.start < range.start {
                self.insert_range_internal(old_range.start..range.start, v);
            }

            // Notice that we can end up splitting the old range into two
            // separate ranges if the old range extends both beyond the given
            // range and before the given range.
        }

        if let Some((cursor, old_range, v)) = self.get_cursor_key_value(&range.end) {
            // If the old range starts before the removed range, we need to trim the old range.
            // TODO: Optimize with replace once available.
            if old_range.start < range.end {
                // Remove that range from the map.
                if let Some(value) = self.remove_at(cursor) {
                    removed_values.push(value);
                }

                // If the removed range extends after the end of the given range,
                // re-insert the part of the old range that extends beyond the end
                // of the given range.
                if old_range.end > range.end {
                    self.insert_range_internal(range.end..old_range.end, v);
                }
            }
        }

        let doomed = self.range(range.start..range.end).map(|(r, _)| r.start).collect::<Vec<_>>();
        for key in doomed {
            let cursor = self.find(&key, CursorPosition::Left);
            removed_values.push(self.remove_at(cursor).expect("entry should exist"));
        }

        removed_values
    }

    /// Insert the given range and value at the location indicated by the cursor.
    fn insert_at(&mut self, cursor: Cursor, range: Range<K>, value: V) -> Option<V> {
        let result = self.node.insert(cursor, range, value);
        match result {
            InsertResult::Inserted => None,
            InsertResult::SplitLeaf(key, right) => {
                let mut keys = ArrayVec::new();
                let mut children = ArrayVec::new();
                keys.push(key);
                let Node::Leaf(left) = self.node.clone() else {
                    unreachable!("non-leaf node split into leaf node");
                };
                children.push(left);
                children.push(right);
                self.node = Node::Internal(Arc::new(NodeInternal {
                    keys,
                    children: ChildList::Leaf(children),
                }));
                None
            }
            InsertResult::SplitInternal(key, right) => {
                let mut keys = ArrayVec::new();
                let mut children = ArrayVec::new();
                keys.push(key);
                let Node::Internal(left) = self.node.clone() else {
                    unreachable!("non-internal node split into internal node");
                };
                children.push(left);
                children.push(right);
                self.node = Node::Internal(Arc::new(NodeInternal {
                    keys,
                    children: ChildList::Internal(children),
                }));
                None
            }
        }
    }

    /// Insert the given range and value.
    ///
    /// Assumes the range is empty and that adjacent ranges have different values.
    fn insert_range_internal(&mut self, range: Range<K>, value: V) -> Option<V> {
        let cursor = self.find(&range.start, CursorPosition::Left);
        self.insert_at(cursor, range, value)
    }

    /// Inserts a range with the given value.
    ///
    /// The keys included in the given range are now associated with the given
    /// value. If those keys were previously associated with another value,
    /// are no longer associated with that previous value.
    ///
    /// This method can cause one or more values in the map to be dropped if
    /// the all of the keys associated with those values are contained within
    /// the given range.
    ///
    /// If the inserted range is directly adjacent to another range with an equal value, the
    /// inserted range will be merged with the adjacent ranges.
    pub fn insert(&mut self, mut range: Range<K>, value: V) {
        if range.is_empty() {
            return;
        }
        self.remove(range.clone());

        // Check for a range directly before this one. If it exists, it will be the last range with
        // start < range.start.
        if let Some((prev_range, prev_value)) = self.range(..range.start).next_back() {
            if prev_range.end == range.start && value == *prev_value {
                let cursor = self.find(&prev_range.start, CursorPosition::Left);
                range.start = prev_range.start;
                self.remove_at(cursor);
            }
        }

        // Check for a range directly after. If it exists, we can look it up by exact start value
        // of range.end.
        if let Some((cursor, next_range, next_value)) = self.get_cursor_key_value(&range.end) {
            if next_range.start == range.end && value == next_value {
                range.end = next_range.end;
                self.remove_at(cursor);
            }
        }

        let cursor = self.find(&range.start, CursorPosition::Left);
        self.insert_at(cursor, range, value);
    }

    /// Remove the entry with the given cursor from the map.
    fn remove_at(&mut self, cursor: Cursor) -> Option<V> {
        let result = self.node.remove(cursor);
        match result {
            RemoveResult::NotFound => None,
            RemoveResult::Removed(value) => Some(value),
            RemoveResult::Underflow(value) => {
                match &mut self.node {
                    Node::Leaf(_) => {
                        // Nothing we can do about an underflow of a single leaf node at the root.
                    }
                    Node::Internal(node) => {
                        // If the root has underflown to a trivial list, we can shrink the tree.
                        if node.children.len() == 1 {
                            self.node = node.children.get(0);
                        }
                    }
                }
                Some(value)
            }
        }
    }

    /// Iterate through the keys and values stored in the map.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            forward: Cursor::with_index(0),
            backward: Cursor::with_index(self.node.len()),
            root: &self.node,
        }
    }

    /// Create the cursor stack for the start bound of the given range.
    fn find_start_bound(&self, bounds: &impl RangeBounds<K>) -> Cursor {
        let key = match bounds.start_bound() {
            Bound::Included(key) => key,
            Bound::Excluded(key) => key,
            Bound::Unbounded => {
                return Cursor::with_index(0);
            }
        };
        self.find(key, CursorPosition::Left)
    }

    /// Create the cursor stack for the end bound of the given range.
    fn find_end_bound(&self, bounds: &impl RangeBounds<K>) -> Cursor {
        let key = match bounds.end_bound() {
            Bound::Included(key) => key,
            Bound::Excluded(key) => key,
            Bound::Unbounded => {
                return Cursor::with_index(self.node.len());
            }
        };
        self.find(key, CursorPosition::Right)
    }

    /// Iterate through the keys and values stored in the given range in the map.
    pub fn range(&self, bounds: impl RangeBounds<K>) -> Iter<'_, K, V> {
        let forward = self.find_start_bound(&bounds);
        let backward = self.find_end_bound(&bounds);
        Iter { forward, backward, root: &self.node }
    }

    /// Iterate over the ranges in the map, starting at the first range starting after or at the given point.
    pub fn iter_starting_at(&self, key: K) -> impl Iterator<Item = (&Range<K>, &V)> {
        self.range(key..).filter(move |(range, _)| key <= range.start)
    }

    /// Iterate over the ranges in the map, starting at the last range starting before or at the given point.
    pub fn iter_ending_at(&self, key: K) -> impl DoubleEndedIterator<Item = (&Range<K>, &V)> {
        self.range(..key)
    }

    pub fn intersection(&self, range: impl Borrow<Range<K>>) -> Iter<'_, K, V> {
        let range = range.borrow();
        self.range(range.start..range.end)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn test_empty() {
        let mut map = RangeMap2::<u32, i32>::default();

        assert!(map.get(12).is_none());
        map.remove(10..34);
        // This is a test to make sure we can handle reversed ranges
        #[allow(clippy::reversed_empty_ranges)]
        map.remove(34..10);
    }

    #[::fuchsia::test]
    fn test_insert_into_empty() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(10..34, -14);

        assert_eq!((&(10..34), &-14), map.get(12).unwrap());
        assert_eq!((&(10..34), &-14), map.get(10).unwrap());
        assert!(map.get(9).is_none());
        assert_eq!((&(10..34), &-14), map.get(33).unwrap());
        assert!(map.get(34).is_none());
    }

    #[::fuchsia::test]
    fn test_iter() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(10..34, -14);
        map.insert(74..92, -12);

        let mut iter = map.iter();

        assert_eq!(iter.next().expect("missing elem"), (&(10..34), &-14));
        assert_eq!(iter.next().expect("missing elem"), (&(74..92), &-12));

        assert!(iter.next().is_none());

        let mut iter = map.iter_starting_at(10);
        assert_eq!(iter.next().expect("missing elem"), (&(10..34), &-14));
        let mut iter = map.iter_starting_at(11);
        assert_eq!(iter.next().expect("missing elem"), (&(74..92), &-12));
        let mut iter = map.iter_starting_at(74);
        assert_eq!(iter.next().expect("missing elem"), (&(74..92), &-12));
        let mut iter = map.iter_starting_at(75);
        assert_eq!(iter.next(), None);

        assert_eq!(map.iter_ending_at(9).collect::<Vec<_>>(), vec![]);
        assert_eq!(map.iter_ending_at(34).collect::<Vec<_>>(), vec![(&(10..34), &-14)]);
        assert_eq!(map.iter_ending_at(74).collect::<Vec<_>>(), vec![(&(10..34), &-14)]);
        assert_eq!(
            map.iter_ending_at(75).collect::<Vec<_>>(),
            vec![(&(10..34), &-14), (&(74..92), &-12)]
        );
        assert_eq!(
            map.iter_ending_at(91).collect::<Vec<_>>(),
            vec![(&(10..34), &-14), (&(74..92), &-12)]
        );
        assert_eq!(
            map.iter_ending_at(92).collect::<Vec<_>>(),
            vec![(&(10..34), &-14), (&(74..92), &-12)]
        );
    }

    #[::fuchsia::test]
    fn test_remove_overlapping_edge() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(10..34, -14);

        map.remove(2..11);
        assert_eq!((&(11..34), &-14), map.get(11).unwrap());

        map.remove(33..42);
        assert_eq!((&(11..33), &-14), map.get(12).unwrap());
    }

    #[::fuchsia::test]
    fn test_remove_middle_splits_range() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(10..34, -14);
        map.remove(15..18);

        assert_eq!((&(10..15), &-14), map.get(12).unwrap());
        assert_eq!((&(18..34), &-14), map.get(20).unwrap());
    }

    #[::fuchsia::test]
    fn test_remove_upper_half_of_split_range_leaves_lower_range() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(10..34, -14);
        map.remove(15..18);
        map.insert(2..7, -21);
        map.remove(20..42);

        assert_eq!((&(2..7), &-21), map.get(5).unwrap());
        assert_eq!((&(10..15), &-14), map.get(12).unwrap());
    }

    #[::fuchsia::test]
    fn test_range_map_overlapping_insert() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(2..7, -21);
        map.insert(5..9, -42);
        map.insert(1..3, -43);
        map.insert(6..8, -44);

        assert_eq!((&(1..3), &-43), map.get(2).unwrap());
        assert_eq!((&(3..5), &-21), map.get(4).unwrap());
        assert_eq!((&(5..6), &-42), map.get(5).unwrap());
        assert_eq!((&(6..8), &-44), map.get(7).unwrap());
    }

    #[::fuchsia::test]
    fn test_intersect_single() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(2..7, -10);

        let mut iter = map.intersection(3..4);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), None);

        let mut iter = map.intersection(2..3);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), None);

        let mut iter = map.intersection(1..4);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), None);

        let mut iter = map.intersection(1..2);
        assert_eq!(iter.next(), None);

        let mut iter = map.intersection(6..7);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), None);
    }

    #[::fuchsia::test]
    fn test_intersect_multiple() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(2..7, -10);
        map.insert(7..9, -20);
        map.insert(10..11, -30);

        let mut iter = map.intersection(3..8);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), Some((&(7..9), &-20)));
        assert_eq!(iter.next(), None);

        let mut iter = map.intersection(3..11);
        assert_eq!(iter.next(), Some((&(2..7), &-10)));
        assert_eq!(iter.next(), Some((&(7..9), &-20)));
        assert_eq!(iter.next(), Some((&(10..11), &-30)));
        assert_eq!(iter.next(), None);
    }

    #[::fuchsia::test]
    fn test_intersect_no_gaps() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(0..1, -10);
        map.insert(1..2, -20);
        map.insert(2..3, -30);

        let mut iter = map.intersection(0..3);
        assert_eq!(iter.next(), Some((&(0..1), &-10)));
        assert_eq!(iter.next(), Some((&(1..2), &-20)));
        assert_eq!(iter.next(), Some((&(2..3), &-30)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_merging() {
        let mut map = RangeMap2::<u32, i32>::default();

        map.insert(1..2, -10);
        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&(1..2), &-10)]);
        map.insert(3..4, -10);
        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&(1..2), &-10), (&(3..4), &-10)]);
        map.insert(2..3, -10);
        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&(1..4), &-10)]);
        map.insert(0..1, -10);
        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&(0..4), &-10)]);
        map.insert(4..5, -10);
        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&(0..5), &-10)]);
        map.insert(2..3, -20);
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            vec![(&(0..2), &-10), (&(2..3), &-20), (&(3..5), &-10)]
        );
    }

    #[test]
    fn test_remove_multiple_ranges_exact_query() {
        let first = 15..21;
        let second = first.end..29;

        let mut map = RangeMap2::default();
        map.insert(first.clone(), 1);
        map.insert(second.clone(), 2);

        assert_eq!(map.remove(first.start..second.end), &[1, 2]);
    }

    #[::fuchsia::test]
    fn test_large_insert_and_remove() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
        }

        // Verify that all inserted entries can be retrieved
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let point = start + 2;
            if let Some((range, value)) = map.get(point) {
                assert!(range.start <= point && point < range.end);
                assert_eq!(*range, start..end);
                assert_eq!(*value, i as i32);
            } else {
                panic!("Expected to find a range for point {}", point);
            }
        }

        // Remove a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            map.remove(start..end);
        }

        // Verify that the map is empty after removing all entries
        assert!(map.is_empty());
    }

    #[::fuchsia::test]
    fn test_large_insert_and_remove_overlapping() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;

        // Insert a large number of entries with overlapping ranges
        for i in 0..num_entries {
            let start = i as u32 * 5;
            let end = start + 20;
            let value = i as i32;
            map.insert(start..end, value);
        }

        // Verify that all inserted entries can be retrieved
        for i in 0..num_entries {
            let point = i as u32 * 5 + 1;
            if let Some((range, value)) = map.get(point) {
                assert!(range.start <= point && point < range.end);
                assert_eq!(*value, i as i32);
            } else {
                panic!("Expected to find a range for point {}", point);
            }
        }

        // Remove a large number of entries with overlapping ranges
        for i in 0..num_entries {
            let start = i as u32 * 5;
            let end = start + 20;
            map.remove(start..end);
        }

        // Verify that the map is empty after removing all entries
        assert!(map.is_empty());
    }

    #[::fuchsia::test]
    fn test_large_insert_and_get_specific_points() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;
        let mut inserted_ranges = Vec::new();

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
            inserted_ranges.push((start..end, value));
        }

        // Verify that specific points can be retrieved correctly
        for (range, value) in &inserted_ranges {
            let point = range.start + 2;
            let (retrieved_range, retrieved_value) = map.get(point).unwrap();
            assert_eq!(retrieved_range, range);
            assert_eq!(retrieved_value, value);
        }
    }

    #[::fuchsia::test]
    fn test_large_insert_and_iter() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;
        let mut inserted_ranges = Vec::new();

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
            inserted_ranges.push((start..end, value));
        }

        // Verify that iter() returns all inserted entries
        let mut iter_ranges: Vec<(&Range<u32>, &i32)> = map.iter().collect();
        iter_ranges.sort_by_key(|(range, _)| range.start);
        let mut inserted_ranges_sorted: Vec<(Range<u32>, i32)> = inserted_ranges.clone();
        inserted_ranges_sorted.sort_by_key(|(range, _)| range.start);

        assert_eq!(iter_ranges.len(), inserted_ranges_sorted.len());
        for (i, (range, value)) in iter_ranges.iter().enumerate() {
            assert_eq!(*range, &inserted_ranges_sorted[i].0);
            assert_eq!(*value, &inserted_ranges_sorted[i].1);
        }
    }

    #[::fuchsia::test]
    fn test_large_insert_and_iter_starting_at() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
        }

        // Verify iter_starting_at()
        let start_point = 5000;
        let mut iter = map.iter_starting_at(start_point);
        while let Some((range, _)) = iter.next() {
            assert!(range.start >= start_point);
        }
    }

    #[::fuchsia::test]
    fn test_large_insert_and_iter_ending_at() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
        }

        // Verify iter_ending_at()
        let end_point = 5000;
        let mut iter = map.iter_ending_at(end_point);
        while let Some((range, _)) = iter.next() {
            assert!(range.start < end_point);
        }
    }

    #[::fuchsia::test]
    fn test_large_insert_and_intersection() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
        }

        // Verify intersection()
        let intersect_start = 4000;
        let intersect_end = 4050;
        let mut iter = map.intersection(intersect_start..intersect_end);
        while let Some((range, _)) = iter.next() {
            assert!((range.start < intersect_end && range.end > intersect_start));
        }
    }

    #[::fuchsia::test]
    fn test_large_insert_and_last_range() {
        let mut map = RangeMap2::<u32, i32>::default();
        let num_entries = 1000;
        let mut last_range = None;

        // Insert a large number of entries
        for i in 0..num_entries {
            let start = i as u32 * 10;
            let end = start + 5;
            let value = i as i32;
            map.insert(start..end, value);
            last_range = Some(start..end);
        }

        // Verify last_range()
        if let Some(expected_last_range) = last_range {
            assert_eq!(map.last_range().unwrap(), &expected_last_range);
        }
    }
}
