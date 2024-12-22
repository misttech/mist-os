// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{Inner, InnerValueType, InspectType, NumericProperty, Property};
use log::error;

/// Inspect uint property data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct UintProperty {
    inner: Inner<InnerValueType>,
}

impl InspectType for UintProperty {}

crate::impl_inspect_type_internal!(UintProperty);

impl Property<'_> for UintProperty {
    type Type = u64;

    fn set(&self, value: u64) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.set_uint_metric(inner_ref.block_index, value))
                .unwrap_or_else(|err| {
                    error!(err:?; "Failed to set property");
                });
        }
    }
}

impl NumericProperty<'_> for UintProperty {
    fn add(&self, value: u64) -> Option<u64> {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.add_uint_metric(inner_ref.block_index, value))
                .map(Option::from)
                .unwrap_or_else(|err| {
                    error!(err:?; "Failed to set property");
                    None
                })
        } else {
            None
        }
    }

    fn subtract(&self, value: u64) -> Option<u64> {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.subtract_uint_metric(inner_ref.block_index, value))
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
    use crate::writer::testing_utils::{get_state, GetBlockExt};
    use crate::writer::Node;
    use inspect_format::BlockType;

    #[fuchsia::test]
    fn uint_property() {
        // Create and use a default value.
        let default = UintProperty::default();
        default.add(1);

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");
        {
            let property = node.create_uint("property", 1);
            property.get_block(|block| {
                assert_eq!(block.block_type(), BlockType::UintValue);
                assert_eq!(block.uint_value().unwrap(), 1);
            });
            node.get_block(|block| {
                assert_eq!(block.child_count().unwrap(), 1);
            });

            property.set(5);
            property.get_block(|block| {
                assert_eq!(block.uint_value().unwrap(), 5);
            });

            assert_eq!(property.subtract(3).unwrap(), 2);
            property.get_block(|block| {
                assert_eq!(block.uint_value().unwrap(), 2);
            });

            assert_eq!(property.add(8).unwrap(), 10);
            property.get_block(|block| {
                assert_eq!(block.uint_value().unwrap(), 10);
            });
        }
        node.get_block(|block| {
            assert_eq!(block.child_count().unwrap(), 0);
        });
    }
}
