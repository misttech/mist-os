// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Node;
use fuchsia_inspect_contrib::nodes::NodeExt;
use fuchsia_inspect_derive::{Inspect, Unit, WithInspect};

/// A Inspect node that holds a mapping of ID to data. The list of IDs is limited
/// to between 0 and `capacity - 1`. Each item is recorded with the current
/// monotonic timestamp. When a new unique item needs to be recorded and there's
/// no entry left, the oldest recorded item is evicted.
pub struct InspectBoundedSetNode<T: Unit> {
    node: Node,
    index: usize,
    capacity: usize,
    items: Vec<(T, Node, <T as Unit>::Data)>,
    eq_fn: fn(&T, &T) -> bool,
}

impl<T: Unit + PartialEq<T>> InspectBoundedSetNode<T> {
    /// Create an InspectBoundedIdsNode with |node| as a parent and the given
    /// |capacity|. `<T as PartialEq>::eq` is used as a function to determine
    /// whether an item to be recorded has already existed.
    pub fn new(node: Node, capacity: usize) -> Self {
        Self::new_with_eq_fn(node, capacity, <T as PartialEq>::eq)
    }
}

impl<T: Unit> InspectBoundedSetNode<T> {
    /// Create an InspectBoundedIdsNode with |node| as a parent, the given
    /// |capacity|, and |eq| as the function to determine to determine whether
    /// whether an item to be recorded has already existed.
    pub fn new_with_eq_fn(node: Node, capacity: usize, eq_fn: fn(&T, &T) -> bool) -> Self {
        Self {
            node,
            index: 0,
            capacity: std::cmp::max(capacity, 1),
            items: Vec::with_capacity(capacity),
            eq_fn,
        }
    }

    /// Record an item to the Inspect node with the current monotonic timestamp,
    /// then returning the ID associated with the item.
    /// If the item had already exited in the node, this operation is a no-op.
    /// Otherwise, either a new entry is created (if an ID is still available),
    /// or the oldest entry is evicted to make room for the new item.
    pub fn record_item(&mut self, mut item: T) -> u64 {
        for (i, (recorded_item, _node, _data)) in self.items.iter().enumerate() {
            if (self.eq_fn)(&item, recorded_item) {
                return i as u64;
            }
        }

        // Item does not exist. Insert new item.
        // If the buffer is already full, evict the oldest item first
        let child = self.node.create_child(self.index.to_string());
        child.record_time("@time");
        let data = item.inspect_create(&child, "data");
        if self.items.len() >= self.capacity {
            self.items[self.index] = (item, child, data);
        } else {
            self.items.push((item, child, data));
        }

        let return_index = self.index as u64;
        self.index = (self.index + 1) % self.capacity;
        return_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyNumericProperty};
    use fuchsia_inspect::Inspector;
    use fuchsia_inspect_derive::IValue;

    #[fuchsia::test]
    fn test_record_items() {
        let inspector = Inspector::default();
        let ids_node = inspector.root().create_child("ids_node");
        let mut ids_node = InspectBoundedSetNode::new(ids_node, 3);
        // Record unique items
        assert_eq!(ids_node.record_item(111), 0);
        assert_eq!(ids_node.record_item(222), 1);
        assert_eq!(ids_node.record_item(333), 2);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": { "@time": AnyNumericProperty, "data": 111i64},
                "1": { "@time": AnyNumericProperty, "data": 222i64},
                "2": { "@time": AnyNumericProperty, "data": 333i64},
            }
        });

        // Record item that already exists does not change the Inspect data
        assert_eq!(ids_node.record_item(222), 1);
        assert_eq!(ids_node.record_item(111), 0);
        assert_eq!(ids_node.record_item(333), 2);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": { "@time": AnyNumericProperty, "data": 111i64},
                "1": { "@time": AnyNumericProperty, "data": 222i64},
                "2": { "@time": AnyNumericProperty, "data": 333i64},
            }
        });

        // Now that the node is full, recording new item would replace the oldest one
        assert_eq!(ids_node.record_item(444), 0);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": { "@time": AnyNumericProperty, "data": 444i64},
                "1": { "@time": AnyNumericProperty, "data": 222i64},
                "2": { "@time": AnyNumericProperty, "data": 333i64},
            }
        });

        // Value that had been evicted is considered to be a new value if they are recorded again
        assert_eq!(ids_node.record_item(111), 1);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": { "@time": AnyNumericProperty, "data": 444i64},
                "1": { "@time": AnyNumericProperty, "data": 111i64},
                "2": { "@time": AnyNumericProperty, "data": 333i64},
            }
        });
    }

    #[derive(PartialEq, Unit)]
    struct Item {
        num: u64,
        str: String,
    }

    #[fuchsia::test]
    fn test_record_items_custom_struct() {
        let inspector = Inspector::default();
        let ids_node = inspector.root().create_child("ids_node");
        let mut ids_node = InspectBoundedSetNode::new(ids_node, 3);
        assert_eq!(ids_node.record_item(Item { num: 1337u64, str: "42".to_string() }), 0);
        assert_eq!(ids_node.record_item(Item { num: 1337u64, str: "43".to_string() }), 1);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": {
                    "@time": AnyNumericProperty,
                    "data": { num: 1337u64, str: "42".to_string() }
                },
                "1": {
                    "@time": AnyNumericProperty,
                    "data": { num: 1337u64, str: "43".to_string() }
                },
            }
        });
    }

    #[fuchsia::test]
    fn test_new_with_eq_fn() {
        let inspector = Inspector::default();
        let ids_node = inspector.root().create_child("ids_node");
        let mut ids_node =
            InspectBoundedSetNode::<Item>::new_with_eq_fn(ids_node, 3, |left, right| {
                left.num == right.num
            });
        assert_eq!(ids_node.record_item(Item { num: 1337u64, str: "42".to_string() }), 0);
        // Only the `num` field is used to determine whether an item is unique, so the
        // this item is not recorded.
        assert_eq!(ids_node.record_item(Item { num: 1337u64, str: "43".to_string() }), 0);
        assert_data_tree!(inspector, root: {
            ids_node: {
                "0": {
                    "@time": AnyNumericProperty,
                    "data": { num: 1337u64, str: "42".to_string() }
                },
            }
        });
    }
}
