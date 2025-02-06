// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use arrayvec::ArrayVec;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

/// A leaf node in the btree.
///
/// Stores a flat map of keys to values, with the `i`th entry in the keys array corresponding to
/// the `i`th entry in the values array. The balancing rules of the btree ensure that every
/// non-root leaf has between N and N/2 entries populated.
#[derive(Clone)]
struct NodeLeaf<K: Ord + Clone, V: Clone, const N: usize> {
    /// The keys stored in this leaf node.
    ///
    /// We store the key in a dense array to improve cache performance during lookups. We often
    /// need to binary-search the keys in a given leaf node, which means having those keys close
    /// together improves cache performance.
    keys: ArrayVec<K, N>,

    /// The value stored in this leaf node.
    values: ArrayVec<V, N>,
}

/// Shows the map structure of the leaf node.
impl<K, V, const N: usize> Debug for NodeLeaf<K, V, N>
where
    K: Debug + Ord + Clone,
    V: Debug + Clone,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.keys.iter().zip(self.values.iter())).finish()
    }
}

/// The result of performing an insertion into a btree node.
enum InsertResult<K: Ord + Clone, V: Clone, const N: usize> {
    /// The value was successfully inserted into an empty slot.
    Inserted,

    /// The value was successfully inserted into a non-empty slot. Returns the value that
    /// previously occupied that slot.
    Replaced(V),

    /// The value was inserted into an empty slot in a leaf node but that insertion caused the
    /// leaf node to exceed its capacity and split into two leaf nodes. The existing leaf node
    /// now holds the entries to the left of the split and the entries to the right of the split
    /// are returned. The split occurred at the returned key.
    SplitLeaf(K, Arc<NodeLeaf<K, V, N>>),

    /// The value was inserted into an empty slot in a subtree but that insertion caused the
    /// internal node to exceed its capacity and split into two internal nodes. The internal node
    /// now holds the entries to the left of the split and the entries to the right of the split
    /// are returned. The split occurred at the returned key.
    SplitInternal(K, Arc<NodeInternal<K, V, N>>),
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

/// Where to place the iteration cursor relative to the specified key.
///
/// This enum lets us distinguish between `Bound::Excluded` and `Bound::Included` when initializing
/// an iterator for a range query. We use `At` and `After` instead of `Excluded` and `Included`
/// because the cursor position we want for `Excluded` and `Included` depends on whether we are
/// initializing the start or end bound of the iterator.
enum CursorPosition {
    /// Place the cursor on the index at which the key is stored.
    At,

    /// Place the cursor on the index after index at which the key is stored.
    After,
}

impl<K, V, const N: usize> NodeLeaf<K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    /// Create an empty leaf node.
    ///
    /// Empty leaf nodes are used only at the root of the tree.
    fn empty() -> Self {
        Self { keys: ArrayVec::new(), values: ArrayVec::new() }
    }

    /// Search this leaf node for the given key.
    fn get(&self, key: &K) -> Option<&V> {
        self.get_key_value(key).map(|(_, v)| v)
    }

    /// Search this leaf for the given key and return both the key and the value found.
    fn get_key_value(&self, key: &K) -> Option<(&K, &V)> {
        match self.keys.binary_search(key) {
            Ok(i) => Some((&self.keys[i], &self.values[i])),
            Err(_) => None,
        }
    }

    /// The last key/value pair stored in this leaf.
    fn last_key_value(&self) -> Option<(&K, &V)> {
        let key = self.keys.last()?;
        let value = self.values.last()?;
        Some((key, value))
    }

    /// Find the given key in this leaf and record its position in the given stack.
    fn find_key<'a>(
        self: &'a Arc<Self>,
        key: &K,
        position: CursorPosition,
        stack: &mut Vec<Cursor<'a, K, V, N>>,
    ) {
        match self.keys.binary_search(key) {
            Ok(i) => {
                let i = match position {
                    CursorPosition::At => i,
                    CursorPosition::After => i + 1,
                };
                stack.push(Cursor { node: NodeRef::Leaf(self), index: i });
            }
            Err(i) => {
                stack.push(Cursor { node: NodeRef::Leaf(self), index: i });
            }
        }
    }

    /// Insert the given value at the given key into this leaf node.
    ///
    /// Inserting a value into a leaf node might cause this node to split into two leaf nodes.
    fn insert(&mut self, key: K, value: V) -> InsertResult<K, V, N> {
        match self.keys.binary_search(&key) {
            Ok(i) => {
                let old_value = self.values[i].clone();
                self.values[i] = value;
                InsertResult::Replaced(old_value)
            }
            Err(i) => {
                if self.keys.len() == N {
                    let middle = N / 2;
                    assert!(middle > 0);
                    let mut right = Self {
                        keys: self.keys.drain(middle..).collect(),
                        values: self.values.drain(middle..).collect(),
                    };
                    if i <= middle {
                        self.keys.insert(i, key);
                        self.values.insert(i, value);
                    } else {
                        right.keys.insert(i - middle, key);
                        right.values.insert(i - middle, value);
                    }
                    InsertResult::SplitLeaf(right.keys[0].clone(), Arc::new(right))
                } else {
                    self.keys.insert(i, key);
                    self.values.insert(i, value);
                    InsertResult::Inserted
                }
            }
        }
    }

    /// From the entry with the given key from this leaf node.
    fn remove(&mut self, key: &K) -> RemoveResult<V> {
        match self.keys.binary_search(key) {
            Ok(i) => {
                self.keys.remove(i);
                let value = self.values.remove(i);
                if self.keys.len() < N / 2 {
                    RemoveResult::Underflow(value)
                } else {
                    RemoveResult::Removed(value)
                }
            }
            Err(_) => RemoveResult::NotFound,
        }
    }
}

/// The children of an internal node in the btree.
#[derive(Clone, Debug)]
enum ChildList<K: Ord + Clone, V: Clone, const N: usize> {
    /// Used when an internal node has leaf nodes as children.
    Leaf(ArrayVec<Arc<NodeLeaf<K, V, N>>, N>),

    /// Used when an internal node has other internal nodes as children.
    Internal(ArrayVec<Arc<NodeInternal<K, V, N>>, N>),
}

impl<K, V, const N: usize> ChildList<K, V, N>
where
    K: Ord + Clone,
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
    fn get(&self, i: usize) -> Node<K, V, N> {
        match self {
            ChildList::Leaf(children) => Node::Leaf(children[i].clone()),
            ChildList::Internal(children) => Node::Internal(children[i].clone()),
        }
    }

    /// Get a reference to the child located at the given index.
    fn get_ref(&self, i: usize) -> NodeRef<'_, K, V, N> {
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
    fn insert(&mut self, index: usize, child: Node<K, V, N>) {
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
struct NodeInternal<K: Ord + Clone, V: Clone, const N: usize> {
    /// A cache of the keys that partition the keys in the children.
    /// The key at index `i` is the smallest key stored in the subtree
    /// of the `i`+1 child.
    ///
    /// We only ever store N-1 keys in this array.
    keys: ArrayVec<K, N>,

    /// The children of this node.
    children: ChildList<K, V, N>,
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

impl<K, V, const N: usize> NodeInternal<K, V, N>
where
    K: Ord + Clone,
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

    /// Search this subtree for the given key.
    fn get(&self, key: &K) -> Option<&V> {
        let i = self.get_child_index(&key);
        match &self.children {
            ChildList::Leaf(children) => children[i].get(key),
            ChildList::Internal(children) => children[i].get(key),
        }
    }

    /// Search this subtree for the given key and return both the key and the value found.
    fn get_key_value(&self, key: &K) -> Option<(&K, &V)> {
        match &self.children {
            ChildList::Leaf(children) => {
                children.last().expect("child lists are always non-empty").get_key_value(key)
            }
            ChildList::Internal(children) => {
                children.last().expect("child lists are always non-empty").get_key_value(key)
            }
        }
    }

    /// The last key/value pair stored in this subtree.
    fn last_key_value(&self) -> Option<(&K, &V)> {
        match &self.children {
            ChildList::Leaf(children) => {
                children.last().expect("child lists are always non-empty").last_key_value()
            }
            ChildList::Internal(children) => {
                children.last().expect("child lists are always non-empty").last_key_value()
            }
        }
    }

    /// Find the given key in this subtree and record its position in the given stack.
    fn find_key<'a>(
        self: &'a Arc<Self>,
        key: &K,
        position: CursorPosition,
        stack: &mut Vec<Cursor<'a, K, V, N>>,
    ) {
        let i = self.get_child_index(&key);
        stack.push(Cursor { node: NodeRef::Internal(self), index: i });
        match &self.children {
            ChildList::Leaf(children) => children[i].find_key(key, position, stack),
            ChildList::Internal(children) => children[i].find_key(key, position, stack),
        }
    }

    /// Insert the given child node at index `i` in this node.
    ///
    /// `key` must be the smallest key that occurs in the `child` subtree.
    ///
    /// The caller must ensure that the child is inserted in the correct location.
    fn insert_child(&mut self, i: usize, key: K, child: Node<K, V, N>) -> InsertResult<K, V, N> {
        let n = self.children.len();
        if n == N {
            let middle = N / 2;
            assert!(middle > 0);
            let mut internal = Self {
                keys: self.keys.drain(middle..).collect(),
                children: self.children.split_off(middle),
            };
            let split_key = self.keys.pop().unwrap();
            if i < middle {
                self.keys.insert(i, key);
                self.children.insert(i + 1, child);
            } else {
                internal.keys.insert(i - middle, key);
                internal.children.insert(i - middle + 1, child);
            }
            debug_assert!(self.keys.len() + 1 == self.children.len());
            debug_assert!(internal.keys.len() + 1 == internal.children.len());
            InsertResult::SplitInternal(split_key, Arc::new(internal))
        } else {
            self.keys.insert(i, key);
            self.children.insert(i + 1, child);
            debug_assert!(self.keys.len() + 1 == self.children.len());
            InsertResult::Inserted
        }
    }

    /// Insert the given value at the given key into this internal node.
    ///
    /// Inserting a value into an internal node might cause this node to split into two internal
    /// nodes.
    fn insert(&mut self, key: K, value: V) -> InsertResult<K, V, N> {
        let i = self.get_child_index(&key);
        let result = match &mut self.children {
            ChildList::Leaf(children) => Arc::make_mut(&mut children[i]).insert(key, value),
            ChildList::Internal(children) => Arc::make_mut(&mut children[i]).insert(key, value),
        };
        match result {
            InsertResult::Inserted => InsertResult::Inserted,
            InsertResult::Replaced(old_value) => InsertResult::Replaced(old_value),
            InsertResult::SplitLeaf(key, right) => self.insert_child(i, key, Node::Leaf(right)),
            InsertResult::SplitInternal(key, right) => {
                self.insert_child(i, key, Node::Internal(right))
            }
        }
    }

    /// Determine whether to rebalance the child with the given index to the left or to the right.
    ///
    /// Given a choice, we will rebalance the child with its larger neighbor.
    ///
    /// The indices returned are always sequential.
    fn select_children_to_rebalance(&self, i: usize) -> (usize, usize) {
        if i == 0 {
            (i, i + 1)
        } else if i == self.children.len() - 1 {
            (i - 1, i)
        } else {
            let left_index = i - 1;
            let left_size = self.children.size_at(left_index);
            let right_index = i + 1;
            let right_size = self.children.size_at(right_index);
            if left_size > right_size {
                (left_index, i)
            } else {
                (i, right_index)
            }
        }
    }

    /// Rebalance the child at the given index.
    ///
    /// If the child and its neighbor are sufficiently small, this function will merge them into a
    /// single node.
    fn rebalance_child(&mut self, i: usize) {
        // Cannot rebalance if we have fewer than two children. This situation occurs only at the
        // root of the tree.
        if self.children.len() < 2 {
            return;
        }
        let (left, right) = self.select_children_to_rebalance(i);
        let n = self.children.size_at(left) + self.children.size_at(right);
        match &mut self.children {
            ChildList::Leaf(children) => {
                let (left_shard_node, right_shared_node) = get_two_mut(children, left, right);
                let left_node = Arc::make_mut(left_shard_node);
                if n <= N {
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
                    self.keys[left] = right_node.keys[0].clone();
                }
            }
            ChildList::Internal(children) => {
                let (left_shard_node, right_shared_node) = get_two_mut(children, left, right);
                let left_node = Arc::make_mut(left_shard_node);
                let old_split_key = &self.keys[left];
                if n <= N {
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

    /// Remove the child with the given key from this subtree.
    fn remove(&mut self, key: &K) -> RemoveResult<V> {
        let i = self.get_child_index(&key);
        let result = match &mut self.children {
            ChildList::Leaf(children) => Arc::make_mut(&mut children[i]).remove(key),
            ChildList::Internal(children) => Arc::make_mut(&mut children[i]).remove(key),
        };
        match result {
            RemoveResult::NotFound => RemoveResult::NotFound,
            RemoveResult::Removed(value) => RemoveResult::Removed(value),
            RemoveResult::Underflow(value) => {
                self.rebalance_child(i);
                if self.children.len() < N / 2 {
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
enum Node<K: Ord + Clone, V: Clone, const N: usize> {
    /// An internal node.
    Internal(Arc<NodeInternal<K, V, N>>),

    /// A leaf node.
    Leaf(Arc<NodeLeaf<K, V, N>>),
}

impl<K, V, const N: usize> Node<K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    /// Search this node for the given key.
    fn get(&self, key: &K) -> Option<&V> {
        match self {
            Node::Internal(node) => node.get(key),
            Node::Leaf(node) => node.get(key),
        }
    }

    /// The number of children stored at this node.
    fn len(&self) -> usize {
        match self {
            Node::Internal(node) => node.children.len(),
            Node::Leaf(node) => node.keys.len(),
        }
    }

    /// Search this node for the given key and return both the key and the value found.
    fn get_key_value(&self, key: &K) -> Option<(&K, &V)> {
        match self {
            Node::Leaf(node) => node.get_key_value(key),
            Node::Internal(node) => node.get_key_value(key),
        }
    }

    /// The last key/value pair stored in this node.
    fn last_key_value(&self) -> Option<(&K, &V)> {
        match self {
            Node::Leaf(node) => node.last_key_value(),
            Node::Internal(node) => node.last_key_value(),
        }
    }

    /// Converts a reference into a Node into a NodeRef.
    fn as_ref(&self) -> NodeRef<'_, K, V, N> {
        match self {
            Node::Internal(node) => NodeRef::Internal(node),
            Node::Leaf(node) => NodeRef::Leaf(node),
        }
    }

    /// Insert the given value at the given key into this node.
    ///
    /// If the insertion causes this node to split, the node will always split into two instances
    /// of the same type of node.
    fn insert(&mut self, key: K, value: V) -> InsertResult<K, V, N> {
        match self {
            Node::Internal(node) => Arc::make_mut(node).insert(key, value),
            Node::Leaf(node) => Arc::make_mut(node).insert(key, value),
        }
    }

    fn remove(&mut self, key: &K) -> RemoveResult<V> {
        match self {
            Node::Internal(node) => Arc::make_mut(node).remove(key),
            Node::Leaf(node) => Arc::make_mut(node).remove(key),
        }
    }

    /// Find the given key in this node and record its position in the given stack.
    fn find_key<'a>(
        &'a self,
        key: &K,
        position: CursorPosition,
        stack: &mut Vec<Cursor<'a, K, V, N>>,
    ) {
        match self {
            Node::Internal(node) => node.find_key(key, position, stack),
            Node::Leaf(node) => node.find_key(key, position, stack),
        }
    }
}

/// A node in the btree.
#[derive(Clone, Debug)]
enum NodeRef<'a, K: Ord + Clone, V: Clone, const N: usize> {
    /// An internal node.
    Internal(&'a Arc<NodeInternal<K, V, N>>),

    /// A leaf node.
    Leaf(&'a Arc<NodeLeaf<K, V, N>>),
}

impl<'a, K, V, const N: usize> NodeRef<'a, K, V, N>
where
    K: Ord + Clone,
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

impl<'a, K, V, const N: usize> PartialEq for NodeRef<'a, K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NodeRef::Internal(lhs), NodeRef::Internal(rhs)) => Arc::ptr_eq(lhs, rhs),
            (NodeRef::Leaf(lhs), NodeRef::Leaf(rhs)) => Arc::ptr_eq(lhs, rhs),
            _ => false,
        }
    }
}

/// A cursor into a tree node.
#[derive(Debug)]
struct Cursor<'a, K: Ord + Clone, V: Clone, const N: usize> {
    /// The node pointed to by the cursor.
    node: NodeRef<'a, K, V, N>,

    /// The index of the child within `node` pointed to by the cursor.
    index: usize,
}

/// An iterator over the key-value pairs stored in a CowMap.
#[derive(Debug)]
pub struct Iter<'a, K: Ord + Clone, V: Clone, const N: usize> {
    /// The state of the forward iteration.
    ///
    /// Represents a stack of cursors into the tree. For internal nodes, the cursor points to the
    /// child currently being iterated. For leaf nodes, the cursor points to the next entry to
    /// enumerate.
    forward: Vec<Cursor<'a, K, V, N>>,

    /// The state of the backward iteration.
    ///
    /// Represents a stack of cursors into the tree. For internal nodes, the cursor points to the
    /// child currently being iterated. For leaf nodes, the cursor points to the child that was
    /// most recently iterated or just past the end of the entry list if no entries have been
    /// enumerated from this leaf yet.
    backward: Vec<Cursor<'a, K, V, N>>,
}

impl<'a, K, V, const N: usize> Iter<'a, K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    /// Whether the iterator is complete.
    ///
    /// Iteration stops when the forward and backward iterators meet.
    fn is_done(&self) -> bool {
        if let (Some(lhs), Some(rhs)) = (self.forward.last(), self.backward.last()) {
            lhs.node == rhs.node && lhs.index == rhs.index
        } else {
            true
        }
    }

    /// The cursor at the top of the forward stack.
    fn forward_cursor(&mut self) -> Option<&mut Cursor<'a, K, V, N>> {
        if self.is_done() {
            None
        } else {
            self.forward.last_mut()
        }
    }

    /// The cursor at the top of the backward stack.
    fn backward_cursor(&mut self) -> Option<&mut Cursor<'a, K, V, N>> {
        if self.is_done() {
            None
        } else {
            self.backward.last_mut()
        }
    }

    /// Pop the forward cursor stack.
    ///
    /// This function exists because we need to advance the cursor at the top of the forward stack
    /// whenever we pop in order to proceed to the next child.
    fn forward_pop(&mut self) {
        self.forward.pop();
        if let Some(cursor) = self.forward.last_mut() {
            cursor.index += 1;
        }
    }
}

impl<'a, K, V, const N: usize> Iterator for Iter<'a, K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(cursor) = self.forward_cursor() {
            match cursor.node {
                NodeRef::Leaf(node) => {
                    if cursor.index < node.keys.len() {
                        let key = &node.keys[cursor.index];
                        let value = &node.values[cursor.index];
                        cursor.index += 1;
                        return Some((key, value));
                    } else {
                        self.forward_pop();
                    }
                }
                NodeRef::Internal(node) => {
                    if cursor.index < node.children.len() {
                        let child = node.children.get_ref(cursor.index);
                        self.forward.push(Cursor { node: child, index: 0 });
                    } else {
                        self.forward_pop();
                    }
                }
            }
        }
        None
    }
}

impl<'a, K, V, const N: usize> DoubleEndedIterator for Iter<'a, K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(cursor) = self.backward_cursor() {
            match cursor.node {
                NodeRef::Leaf(node) => {
                    if cursor.index > 0 {
                        cursor.index -= 1;
                        let key = &node.keys[cursor.index];
                        let value = &node.values[cursor.index];
                        return Some((key, value));
                    } else {
                        self.backward.pop();
                    }
                }
                NodeRef::Internal(node) => {
                    if cursor.index > 0 {
                        cursor.index -= 1;
                        let child = node.children.get_ref(cursor.index);
                        let index = child.len();
                        self.backward.push(Cursor { node: child, index });
                    } else {
                        self.backward.pop();
                    }
                }
            }
        }
        None
    }
}

/// A copy-on-write map.
///
/// This map can be cloned efficiently. If the map is modified after being cloned, the relevant
/// parts of the map's internal structure will be copied lazily.
///
/// `N` is the number of entries to store at each leaf of the map's underlying tree. `N` must be at
/// least 2 for the map to function properly.
#[derive(Clone, Debug)]
pub struct CowMap<K: Ord + Clone, V: Clone, const N: usize> {
    /// The root node of the tree.
    ///
    /// The root node is either a leaf of an internal node, depending on the number of entries in
    /// the map.
    node: Node<K, V, N>,
}

impl<K, V, const N: usize> Default for CowMap<K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    fn default() -> Self {
        Self { node: Node::Leaf(Arc::new(NodeLeaf::empty())) }
    }
}

impl<K, V, const N: usize> CowMap<K, V, N>
where
    K: Ord + Clone,
    V: Clone,
{
    /// Whether this map contains any entries.
    pub fn is_empty(&self) -> bool {
        match &self.node {
            Node::Leaf(node) => node.keys.is_empty(),
            Node::Internal(_) => false,
        }
    }

    /// Get the value corresponding to the given key from the map.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.node.get(key)
    }

    /// Insert the given value with the given key into this map.
    ///
    /// If the map already contained a value with the given key, that value is returned.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.node.insert(key, value) {
            InsertResult::Inserted => None,
            InsertResult::Replaced(old_value) => Some(old_value),
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

    /// Remove the entry with the given key from the map.
    ///
    /// If the key was present in the map, returns the value previously stored at the given key.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        match self.node.remove(key) {
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
    pub fn iter(&self) -> Iter<'_, K, V, N> {
        Iter {
            forward: vec![Cursor { node: self.node.as_ref(), index: 0 }],
            backward: vec![Cursor { node: self.node.as_ref(), index: self.node.len() }],
        }
    }

    /// Create the cursor stack for the start bound of the given range.
    fn find_start_bound(&self, bounds: &impl RangeBounds<K>) -> Vec<Cursor<'_, K, V, N>> {
        let mut stack = vec![];
        let (key, position) = match bounds.start_bound() {
            Bound::Included(key) => (key, CursorPosition::At),
            Bound::Excluded(key) => (key, CursorPosition::After),
            Bound::Unbounded => {
                stack.push(Cursor { node: self.node.as_ref(), index: 0 });
                return stack;
            }
        };
        self.node.find_key(key, position, &mut stack);
        stack
    }

    /// Create teh cursor stack for the end bound of the given range.
    fn find_end_bound(&self, bounds: &impl RangeBounds<K>) -> Vec<Cursor<'_, K, V, N>> {
        let mut stack = vec![];
        let (key, position) = match bounds.end_bound() {
            Bound::Included(key) => (key, CursorPosition::After),
            Bound::Excluded(key) => (key, CursorPosition::At),
            Bound::Unbounded => {
                stack.push(Cursor { node: self.node.as_ref(), index: self.node.len() });
                return stack;
            }
        };
        self.node.find_key(key, position, &mut stack);
        stack
    }

    /// Iterate through the keys and values stored in the given range in the map.
    pub fn range(&self, bounds: impl RangeBounds<K>) -> Iter<'_, K, V, N> {
        let forward = self.find_start_bound(&bounds);
        let backward = self.find_end_bound(&bounds);
        Iter { forward, backward }
    }

    /// The key and value stored in the map at the given key.
    ///
    /// Useful if not all key objects that are equal in the total ordering are actually identical.
    pub fn get_key_value(&self, key: &K) -> Option<(&K, &V)> {
        self.node.get_key_value(key)
    }

    /// The key and value for the largest key value stored in the map.
    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        self.node.last_key_value()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_empty() {
        let map = CowMap::<u32, i64, 4>::default();
        assert!(map.is_empty());
        assert!(map.get(&12).is_none());
    }

    #[test]
    fn test_insert() {
        let mut map = CowMap::<u32, i64, 4>::default();
        assert!(map.is_empty());
        assert!(map.insert(12, 42).is_none());
        assert!(!map.is_empty());
        assert_eq!(map.insert(12, 43), Some(42));
        assert!(map.get(&3).is_none());
        assert!(map.get(&30).is_none());
        assert_eq!(map.get(&12), Some(&43));
    }

    #[test]
    fn test_split_leaf() {
        let mut map = CowMap::<u32, i64, 4>::default();

        // Fill leaf node.
        for i in 0..4 {
            map.insert(i * 10 + 5, i as i64);
        }

        // Insert a new child in each position relative to the existing entries.
        for i in 0..5 {
            let mut map2 = map.clone();
            let slot = i * 10;
            map2.insert(slot, 100);

            assert_eq!(map2.get(&slot), Some(&100));
            assert_eq!(map2.get(&5), Some(&0));
            assert_eq!(map2.get(&15), Some(&1));
            assert_eq!(map2.get(&25), Some(&2));
            assert_eq!(map2.get(&35), Some(&3));
        }
    }

    #[test]
    fn test_split_internal() {
        let mut map = CowMap::<u32, i64, 4>::default();

        // Create four leaf nodes that are half full.
        for i in 0..8 {
            map.insert(i * 100, i as i64);
        }

        // Insert a new child in each position relative to the existing leaf nodes.
        for i in 0..4 {
            let mut map2 = map.clone();
            // The key for the smallest entry in leaf i.
            let start = i * 200;

            // Fill leaf i.
            map2.insert(start + 10, 100);
            map2.insert(start + 20, 200);

            // Split leaf i.
            map2.insert(start + 30, 300);

            assert_eq!(map2.get(&(start + 10)), Some(&100));
            assert_eq!(map2.get(&(start + 20)), Some(&200));
            assert_eq!(map2.get(&(start + 30)), Some(&300));
            for i in 0..8 {
                assert_eq!(map.get(&(i * 100)), Some(&(i as i64)));
            }
        }
    }

    #[test]
    fn test_insert_many() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64 * 2);
        }
        for i in 0..100 {
            assert_eq!(map.get(&i), Some(&(i as i64 * 2)));
        }
    }

    #[test]
    fn test_remove() {
        let mut map = CowMap::<u32, i64, 4>::default();
        assert!(map.is_empty());
        assert!(map.insert(12, 42).is_none());
        assert!(!map.is_empty());
        assert_eq!(map.remove(&12), Some(42));
        assert!(map.get(&12).is_none());
        assert!(map.is_empty());
    }

    #[test]
    fn test_remove_many() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64 * 2);
        }

        for i in (0..100).step_by(2) {
            assert_eq!(map.remove(&i), Some(i as i64 * 2));
        }

        for i in (0..100).step_by(2) {
            assert!(map.get(&i).is_none());
        }

        for i in (1..100).step_by(2) {
            assert_eq!(map.get(&i), Some(&(i as i64 * 2)));
        }
    }

    #[test]
    fn test_split_merge_rebalance() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i * 2, i as i64);
        }
        // Remove every other element, forcing merges and rebalances.
        for i in (0..100).step_by(4) {
            assert_eq!(map.remove(&(i * 2)), Some(i as i64));
            assert_eq!(map.remove(&((i + 1) * 2)), Some((i + 1) as i64));
        }
        // Ensure remaining values are correct after churn.
        for i in (2..100).step_by(4) {
            assert_eq!(map.get(&(i * 2)), Some(&(i as i64)));
            assert_eq!(map.get(&((i + 1) * 2)), Some(&((i + 1) as i64)));
        }
    }

    #[test]
    fn test_clone() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64);
        }
        let map2 = map.clone();

        for i in 0..100 {
            assert_eq!(map.get(&i), Some(&(i as i64)));
            assert_eq!(map2.get(&i), Some(&(i as i64)));
        }
    }

    #[test]
    fn test_clone_modify() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64);
        }
        let mut map2 = map.clone();

        for i in 0..50 {
            map2.insert(i, i as i64 * 2);
        }

        for i in 0..50 {
            assert_eq!(map.get(&i), Some(&(i as i64)));
            assert_eq!(map2.get(&i), Some(&(i as i64 * 2)));
        }

        for i in 50..100 {
            assert_eq!(map.get(&i), Some(&(i as i64)));
            assert_eq!(map2.get(&i), Some(&(i as i64)));
        }
    }

    #[test]
    fn test_remove_root_single_child() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        map.insert(2, 2);
        map.insert(3, 3);
        map.insert(4, 4);
        map.insert(5, 5); // Causes a split, creating a root internal node.

        map.remove(&1);
        map.remove(&2);
        map.remove(&3);
        map.remove(&4);
        // Now only a single child remains, the root should shrink to become that child.
        map.remove(&5);

        assert!(map.is_empty());
    }

    #[test]
    fn test_remove_internal_rebalance_left_heavy() {
        let mut map = CowMap::<u32, i64, 4>::default();

        for i in 0..10 {
            map.insert(i, i as i64);
        }

        // Remove elements in a way to force left-heavy rebalancing.
        map.remove(&9);
        map.remove(&8);

        // Check map integrity
        for i in 0..8 {
            assert_eq!(map.get(&i), Some(&(i as i64)));
        }

        assert!(map.get(&8).is_none());
        assert!(map.get(&9).is_none());
    }

    #[test]
    fn test_remove_internal_rebalance_right_heavy() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        // Remove elements to force right-heavy rebalancing within internal node(s)
        map.remove(&0);
        map.remove(&1);

        for i in 2..10 {
            assert_eq!(map.get(&i), Some(&(i as i64)));
        }

        assert!(map.get(&0).is_none());
        assert!(map.get(&1).is_none());
    }

    #[test]
    fn test_remove_all_from_full_map() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64);
        }

        for i in 0..100 {
            assert_eq!(map.remove(&i), Some(i as i64));
        }

        assert!(map.is_empty());
    }

    #[test]
    fn test_interleaved_insert_remove() {
        let mut map = CowMap::<u32, i64, 4>::default();

        for i in 0..50 {
            map.insert(i, i as i64);
        }
        for i in 0..25 {
            assert_eq!(map.remove(&(i * 2)), Some((i * 2) as i64));
        }

        for i in 0..25 {
            map.insert(i + 50, i as i64);
        }

        for i in 0..25 {
            assert_eq!(map.get(&((i * 2) + 1)), Some(&((i * 2 + 1) as i64)));
            assert_eq!(map.get(&(i + 50)), Some(&(i as i64)));
        }
    }

    #[test]
    fn test_remove_from_cloned_map() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let mut map2 = map.clone();
        map2.remove(&5);
        assert!(map2.get(&5).is_none()); // map2 should not have 5
        assert_eq!(map.get(&5), Some(&5)); // map should still have 5

        map.remove(&5);
        assert!(map.get(&5).is_none());
    }

    #[test]
    fn test_iter() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        map.insert(2, 2);
        map.insert(3, 3);
        map.insert(4, 4);
        map.insert(5, 5);

        let mut iter = map.iter();
        assert_eq!(iter.next(), Some((&1, &1)));
        assert_eq!(iter.next(), Some((&2, &2)));
        assert_eq!(iter.next(), Some((&3, &3)));
        assert_eq!(iter.next(), Some((&4, &4)));
        assert_eq!(iter.next(), Some((&5, &5)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_order() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        map.insert(5, 5);
        map.insert(2, 2);
        map.insert(3, 3);
        map.insert(4, 4);

        let collected = map.iter().map(|(&k, &v)| (k, v)).collect::<Vec<_>>();

        assert_eq!(collected, vec![(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
    }

    #[test]
    fn test_iter_large() {
        let mut map = CowMap::<u32, i64, 4>::default();

        for i in 0..100 {
            map.insert(i, i as i64 * 2);
        }

        let mut iter = map.iter();

        for i in 0..100 {
            assert_eq!(iter.next(), Some((&i, &(i as i64 * 2))));
        }
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_empty() {
        let map = CowMap::<u32, i64, 4>::default();
        let mut iter = map.iter();
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_after_remove_all() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..100 {
            map.insert(i, i as i64 * 2);
        }

        for i in 0..100 {
            map.remove(&i);
        }

        let mut iter = map.iter();
        assert_eq!(iter.next(), None); // Should be empty after removing everything
    }

    #[test]
    fn test_iter_after_split_and_remove() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }
        // Force splits.
        for i in 0..5 {
            map.remove(&i);
        }

        let collected: Vec<_> = map.iter().map(|(&k, &v)| (k, v)).collect();
        assert_eq!(collected, vec![(5, 5), (6, 6), (7, 7), (8, 8), (9, 9)]);
    }

    #[test]
    fn test_iter_after_clone_and_remove() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let mut map2 = map.clone();
        map2.remove(&5);

        let collected: Vec<_> = map.iter().map(|(&k, &v)| (k, v)).collect();
        assert_eq!(
            collected,
            vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 7), (8, 8), (9, 9)]
        );

        let collected2: Vec<_> = map2.iter().map(|(&k, &v)| (k, v)).collect();
        assert_eq!(
            collected2,
            vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (6, 6), (7, 7), (8, 8), (9, 9)]
        );
    }

    #[test]
    fn test_range_basic() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let range: Vec<_> = map.range(3..7).map(|(&k, &v)| (k, v)).collect();
        assert_eq!(range, vec![(3, 3), (4, 4), (5, 5), (6, 6)]);

        let range: Vec<_> = map.range(3..=7).map(|(&k, &v)| (k, v)).collect();
        assert_eq!(range, vec![(3, 3), (4, 4), (5, 5), (6, 6), (7, 7)]);

        let range = map.range(..).map(|(&k, &v)| (k, v)).collect::<Vec<_>>();
        assert_eq!(range.len(), 10);
        for i in 0..10 {
            assert_eq!(range[i], (i as u32, i as i64));
        }
    }

    #[test]
    fn test_range_empty() {
        let map = CowMap::<u32, i64, 4>::default();
        let range: Vec<_> = map.range(3..7).map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());
    }

    #[test]
    fn test_range_out_of_bounds() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let range: Vec<_> = map.range(10..20).map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());

        let range: Vec<_> = map.range(..0).map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());
    }

    #[test]
    fn test_range_exclusive_start_inclusive_end() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }
        let range: Vec<_> =
            map.range((Bound::Excluded(2), Bound::Included(5))).map(|(&k, &v)| (k, v)).collect();
        assert_eq!(range, vec![(3, 3), (4, 4), (5, 5)]);
    }

    #[test]
    fn test_get_key_value_basic() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        assert_eq!(map.get_key_value(&1), Some((&1, &1)));
        assert_eq!(map.get_key_value(&2), None);

        map.insert(2, 2);
        map.insert(3, 3);

        assert_eq!(map.get_key_value(&2), Some((&2, &2)));
        assert_eq!(map.get_key_value(&3), Some((&3, &3)));
    }

    #[test]
    fn test_get_key_value_empty() {
        let map = CowMap::<u32, i64, 4>::default();
        assert_eq!(map.get_key_value(&1), None);
    }

    #[test]
    fn test_last_key_value_basic() {
        let mut map = CowMap::<u32, i64, 4>::default();
        assert_eq!(map.last_key_value(), None);

        map.insert(1, 1);
        assert_eq!(map.last_key_value(), Some((&1, &1)));

        map.insert(2, 2);
        assert_eq!(map.last_key_value(), Some((&2, &2)));
        map.insert(3, 3);
        map.insert(0, 0);

        assert_eq!(map.last_key_value(), Some((&3, &3)));
    }

    #[test]
    fn test_last_key_value_after_remove() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        map.insert(2, 2);
        map.insert(3, 3);

        map.remove(&3);
        assert_eq!(map.last_key_value(), Some((&2, &2)));

        map.remove(&2);
        assert_eq!(map.last_key_value(), Some((&1, &1)));

        map.remove(&1);
        assert_eq!(map.last_key_value(), None); // Should be empty after removing everything
    }

    #[test]
    fn test_last_key_value_empty() {
        let map = CowMap::<u32, i64, 4>::default();
        assert_eq!(map.last_key_value(), None);
    }

    #[test]
    fn test_range_complex_large() {
        let mut map = CowMap::<u32, i64, 4>::default();

        let keys = (0..200).collect::<Vec<_>>();

        for &key in &keys {
            map.insert(key, key as i64 * 2);
        }

        // Choose tricky start and end bounds for the range
        let start = 32;
        let end = 164;

        let expected: Vec<_> = keys
            .iter()
            .cloned()
            .filter(|&k| k > start && k <= end)
            .map(|k| (k, k as i64 * 2))
            .collect();

        let range: Vec<_> = map
            .range((Bound::Excluded(start), Bound::Included(end)))
            .map(|(&k, &v)| (k, v))
            .collect();

        assert_eq!(range, expected);
    }

    #[test]
    fn test_iter_backward() {
        let mut map = CowMap::<u32, i64, 4>::default();
        map.insert(1, 1);
        map.insert(2, 2);
        map.insert(3, 3);
        map.insert(4, 4);
        map.insert(5, 5);

        let mut iter = map.iter();
        assert_eq!(iter.next_back(), Some((&5, &5)));
        assert_eq!(iter.next_back(), Some((&4, &4)));
        assert_eq!(iter.next_back(), Some((&3, &3)));
        assert_eq!(iter.next_back(), Some((&2, &2)));
        assert_eq!(iter.next_back(), Some((&1, &1)));
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn test_iter_backward_large() {
        let mut map = CowMap::<u32, i64, 4>::default();

        for i in 0..100 {
            map.insert(i, i as i64 * 2);
        }

        let mut iter = map.iter();

        for i in (0..100).rev() {
            assert_eq!(iter.next_back(), Some((&i, &(i as i64 * 2))));
        }
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn test_iter_forward_and_backward_interleaved() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let mut iter = map.iter();
        assert_eq!(iter.next(), Some((&0, &0)));
        assert_eq!(iter.next_back(), Some((&9, &9)));
        assert_eq!(iter.next(), Some((&1, &1)));
        assert_eq!(iter.next_back(), Some((&8, &8)));
        assert_eq!(iter.next(), Some((&2, &2)));
        assert_eq!(iter.next_back(), Some((&7, &7)));
        assert_eq!(iter.next(), Some((&3, &3)));
        assert_eq!(iter.next_back(), Some((&6, &6)));
        assert_eq!(iter.next(), Some((&4, &4)));
        assert_eq!(iter.next_back(), Some((&5, &5)));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);

        // Test with odd number of elements
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..9 {
            map.insert(i, i as i64);
        }

        let mut iter = map.iter();
        assert_eq!(iter.next(), Some((&0, &0)));
        assert_eq!(iter.next_back(), Some((&8, &8)));
        assert_eq!(iter.next(), Some((&1, &1)));
        assert_eq!(iter.next_back(), Some((&7, &7)));
        assert_eq!(iter.next(), Some((&2, &2)));
        assert_eq!(iter.next_back(), Some((&6, &6)));
        assert_eq!(iter.next(), Some((&3, &3)));
        assert_eq!(iter.next_back(), Some((&5, &5)));
        assert_eq!(iter.next(), Some((&4, &4)));

        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn test_range_backward() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let range: Vec<_> = map.range(3..7).rev().map(|(&k, &v)| (k, v)).collect();
        assert_eq!(range, vec![(6, 6), (5, 5), (4, 4), (3, 3)]);

        let range: Vec<_> = map.range(3..=7).rev().map(|(&k, &v)| (k, v)).collect();
        assert_eq!(range, vec![(7, 7), (6, 6), (5, 5), (4, 4), (3, 3)]);
    }

    #[test]
    fn test_range_backward_edge_cases() {
        let mut map = CowMap::<u32, i64, 4>::default();
        for i in 0..10 {
            map.insert(i, i as i64);
        }

        let range: Vec<_> = map.range(..0).rev().map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());

        let range: Vec<_> = map.range(10..).rev().map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());

        let range: Vec<_> = map.range(5..5).rev().map(|(&k, &v)| (k, v)).collect();
        assert!(range.is_empty());
    }

    #[test]
    fn test_range_backward_complex_tree() {
        let mut map = CowMap::<u32, i64, 4>::default();
        // Create a more complex tree structure by interleaving insertions and removals.
        for i in 0..100 {
            map.insert(i, i as i64);
        }
        for i in 0..50 {
            map.remove(&(i * 2));
        }
        for i in 0..25 {
            map.insert(i + 100, (i + 100) as i64);
        }

        let range: Vec<_> = map.range(20..120).rev().map(|(&k, &v)| (k, v)).collect();
        let mut expected = Vec::new();
        for i in 21..100 {
            if i % 2 != 0 {
                expected.push((i, i as i64));
            }
        }
        for i in 100..120 {
            expected.push((i, i as i64));
        }
        expected.reverse(); // Reverse expected range since we iterate backward on the map.

        assert_eq!(range, expected);
    }
}
