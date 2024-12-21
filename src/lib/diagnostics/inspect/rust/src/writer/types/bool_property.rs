// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{Inner, InnerValueType, InspectType, Property};
use log::error;

/// Inspect API Bool Property data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct BoolProperty {
    inner: Inner<InnerValueType>,
}

impl Property<'_> for BoolProperty {
    type Type = bool;

    fn set(&self, value: bool) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.set_bool(inner_ref.block_index, value))
                .unwrap_or_else(|e| {
                    error!("Failed to set property. Error: {:?}", e);
                });
        }
    }
}

impl InspectType for BoolProperty {}

crate::impl_inspect_type_internal!(BoolProperty);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::testing_utils::{get_state, GetBlockExt};
    use crate::writer::Node;
    use inspect_format::BlockType;

    #[fuchsia::test]
    fn bool_property() {
        // Create and use a default value.
        let default = BoolProperty::default();
        default.set(true);

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");
        {
            let property = node.create_bool("property", true);
            property.get_block(|block| {
                assert_eq!(block.block_type(), BlockType::BoolValue);
                assert!(block.bool_value().unwrap());
            });
            node.get_block(|block| {
                assert_eq!(block.child_count().unwrap(), 1);
            });

            property.set(false);
            property.get_block(|block| {
                assert!(!block.bool_value().unwrap());
            });
        }
        node.get_block(|block| {
            assert_eq!(block.child_count().unwrap(), 0);
        });
    }
}
