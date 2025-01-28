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
    /// The number of children for this node.
    fn len(&self) -> usize {
        match self {
            ChildList::Leaf(children) => children.len(),
            ChildList::Internal(children) => children.len(),
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
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_empty() {
        let map = CowMap::<u32, i64, 8>::default();
        assert!(map.is_empty());
        assert!(map.get(&12).is_none());
    }

    #[test]
    fn test_insert() {
        let mut map = CowMap::<u32, i64, 8>::default();
        assert!(map.is_empty());
        assert!(map.insert(12, 42).is_none());
        assert!(!map.is_empty());
        assert_eq!(map.insert(12, 43), Some(42));
        assert!(map.get(&3).is_none());
        assert!(map.get(&30).is_none());
        assert_eq!(map.get(&12), Some(&43));
    }

    #[test]
    fn test_split() {
        let mut map = CowMap::<u32, i64, 8>::default();
        assert!(map.is_empty());
        assert!(map.insert(10, -234).is_none());
        assert!(map.insert(130, -437).is_none());
        assert!(map.insert(60, -213).is_none());
        assert!(map.insert(90, -632).is_none());
        assert!(map.insert(120, -853).is_none());
        assert!(map.insert(70, -873).is_none());
        assert!(map.insert(110, -981).is_none());
        assert!(map.insert(30, -621).is_none());
        assert!(map.insert(80, -324).is_none());
        assert!(map.insert(100, -169).is_none());
        assert!(map.insert(20, -642).is_none());
        assert!(map.insert(50, -641).is_none());
        assert!(map.insert(40, -584).is_none());
        assert!(!map.is_empty());

        assert_eq!(map.insert(30, 874), Some(-621));
        assert_eq!(map.insert(110, 713), Some(-981));

        assert_eq!(map.get(&10), Some(&-234));
        assert_eq!(map.get(&20), Some(&-642));
        assert_eq!(map.get(&30), Some(&874));
        assert_eq!(map.get(&40), Some(&-584));
        assert_eq!(map.get(&50), Some(&-641));
        assert_eq!(map.get(&60), Some(&-213));
        assert_eq!(map.get(&70), Some(&-873));
        assert_eq!(map.get(&80), Some(&-324));
        assert_eq!(map.get(&90), Some(&-632));
        assert_eq!(map.get(&100), Some(&-169));
        assert_eq!(map.get(&110), Some(&713));
        assert_eq!(map.get(&120), Some(&-853));

        assert!(map.get(&15).is_none());
        assert!(map.get(&15).is_none());
        assert!(map.get(&25).is_none());
        assert!(map.get(&35).is_none());
        assert!(map.get(&45).is_none());
        assert!(map.get(&55).is_none());
        assert!(map.get(&65).is_none());
        assert!(map.get(&75).is_none());
        assert!(map.get(&85).is_none());
        assert!(map.get(&95).is_none());
        assert!(map.get(&105).is_none());
        assert!(map.get(&115).is_none());
        assert!(map.get(&125).is_none());
    }
}
