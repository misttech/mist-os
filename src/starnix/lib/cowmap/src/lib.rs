// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use arrayvec::ArrayVec;
use std::fmt;
use std::fmt::{Debug, Formatter};
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
        match self.keys.binary_search(key) {
            Ok(i) => Some(&self.values[i]),
            Err(_) => None,
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
                    let middle = self.keys.len() / 2;
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

    fn size_at(&self, i: usize) -> usize {
        match self {
            ChildList::Leaf(children) => children[i].keys.len(),
            ChildList::Internal(children) => children[i].children.len(),
        }
    }

    fn get(&self, i: usize) -> Node<K, V, N> {
        match self {
            ChildList::Leaf(children) => Node::Leaf(children[i].clone()),
            ChildList::Internal(children) => Node::Internal(children[i].clone()),
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

    /// Insert the given child node at index `i` in this node.
    ///
    /// `key` must be the smallest key that occurs in the `child` subtree.
    ///
    /// The caller must ensure that the child is inserted in the correct location.
    fn insert_child(&mut self, i: usize, key: K, child: Node<K, V, N>) -> InsertResult<K, V, N> {
        let n = self.children.len();
        if n == N {
            let middle = n / 2;
            assert!(middle > 0);
            let mut internal = Self {
                keys: self.keys.drain(middle..).collect(),
                children: self.children.split_off(middle),
            };
            if i <= middle {
                self.keys.insert(i, key);
                self.children.insert(i + 1, child);
            } else {
                internal.keys.insert(i - middle, key);
                internal.children.insert(i - middle + 1, child);
            }
            InsertResult::SplitInternal(self.keys.pop().unwrap(), Arc::new(internal))
        } else {
            self.keys.insert(i, key);
            self.children.insert(i + 1, child);
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
                    assert!(left_node.keys.len() + 1 == left_node.children.len());
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
                        assert!(left_node.keys.len() + 1 == left_node.children.len());
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
                        assert!(left_node.keys.len() + 1 == left_node.children.len());
                        assert!(right_node.keys.len() + 1 == right_node.children.len());
                    }
                    // Update the split key to reflect the new division between the nodes.
                    self.keys[left] = split_key;
                }
            }
        }
    }

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
}
