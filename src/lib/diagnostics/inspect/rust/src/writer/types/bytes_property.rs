// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{Inner, InnerPropertyType, InspectType, Property};
use log::error;

/// Inspect Bytes Property data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct BytesProperty {
    inner: Inner<InnerPropertyType>,
}

impl<'t> Property<'t> for BytesProperty {
    type Type = &'t [u8];

    fn set(&self, value: &'t [u8]) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.set_buffer_property(inner_ref.block_index, value))
                .unwrap_or_else(|e| error!("Failed to set property. Error: {:?}", e));
        }
    }
}

impl InspectType for BytesProperty {}

crate::impl_inspect_type_internal!(BytesProperty);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::testing_utils::{get_state, GetBlockExt};
    use crate::writer::Node;
    use inspect_format::{BlockType, PropertyFormat};

    #[fuchsia::test]
    fn bytes_property() {
        // Create and use a default value.
        let default = BytesProperty::default();
        default.set(&[0u8, 3u8]);

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");
        {
            let property = node.create_bytes("property", b"test");
            property.get_block(|block| {
                assert_eq!(block.block_type(), BlockType::BufferValue);
                assert_eq!(block.total_length().unwrap(), 4);
                assert_eq!(block.property_format().unwrap(), PropertyFormat::Bytes);
            });
            node.get_block(|block| {
                assert_eq!(block.child_count().unwrap(), 1);
            });

            property.set(b"test-set");
            property.get_block(|block| {
                assert_eq!(block.total_length().unwrap(), 8);
            });
        }
        node.get_block(|block| {
            assert_eq!(block.child_count().unwrap(), 0);
        });
    }
}
