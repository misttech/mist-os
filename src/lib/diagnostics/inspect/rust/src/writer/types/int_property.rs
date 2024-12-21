// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{Inner, InnerValueType, InspectType, NumericProperty, Property};
use log::error;

/// Inspect int property data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct IntProperty {
    inner: Inner<InnerValueType>,
}

impl InspectType for IntProperty {}

crate::impl_inspect_type_internal!(IntProperty);

impl Property<'_> for IntProperty {
    type Type = i64;

    fn set(&self, value: i64) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.set_int_metric(inner_ref.block_index, value))
                .unwrap_or_else(|err| {
                    error!(err:?; "Failed to set property");
                });
        }
    }
}

impl NumericProperty<'_> for IntProperty {
    fn add(&self, value: i64) -> Option<i64> {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.add_int_metric(inner_ref.block_index, value))
                .map(Option::from)
                .unwrap_or_else(|err| {
                    error!(err:?; "Failed to set property");
                    None
                })
        } else {
            None
        }
    }

    fn subtract(&self, value: i64) -> Option<i64> {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.subtract_int_metric(inner_ref.block_index, value))
                .map(Option::from)
                .unwrap_or_else(|err| {
                    error!(err:?; "Failed to set property");
                    None
                })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assert_update_is_atomic;
    use crate::writer::testing_utils::{get_state, GetBlockExt};
    use crate::writer::{Inspector, Node};
    use inspect_format::BlockType;

    #[fuchsia::test]
    fn int_property() {
        // Create and use a default value.
        let default = IntProperty::default();
        default.add(1);

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");
        {
            let property = node.create_int("property", 1);
            property.get_block(|property_block| {
                assert_eq!(property_block.block_type(), BlockType::IntValue);
                assert_eq!(property_block.int_value().unwrap(), 1);
            });
            node.get_block(|node_block| {
                assert_eq!(node_block.child_count().unwrap(), 1);
            });

            property.set(2);
            property.get_block(|property_block| {
                assert_eq!(property_block.int_value().unwrap(), 2);
            });

            assert_eq!(property.subtract(5).unwrap(), -3);
            property.get_block(|property_block| {
                assert_eq!(property_block.int_value().unwrap(), -3);
            });

            assert_eq!(property.add(8).unwrap(), 5);
            property.get_block(|property_block| {
                assert_eq!(property_block.int_value().unwrap(), 5);
            });
        }
        node.get_block(|node_block| {
            assert_eq!(node_block.child_count().unwrap(), 0);
        });
    }

    #[fuchsia::test]
    fn property_atomics() {
        let inspector = Inspector::default();
        let property = inspector.root().create_int("property", 5);

        assert_update_is_atomic!(property, |property| {
            property.subtract(1);
            property.subtract(2);
        });
    }
}
