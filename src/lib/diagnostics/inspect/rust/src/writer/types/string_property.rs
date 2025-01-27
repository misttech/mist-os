// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{Inner, InnerPropertyType, InspectType, Property};
use log::error;

/// Inspect String Property data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct StringProperty {
    inner: Inner<InnerPropertyType>,
}

impl<'t> Property<'t> for StringProperty {
    type Type = &'t str;

    fn set(&self, value: &'t str) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.set_string_property(inner_ref.block_index, value))
                .unwrap_or_else(|e| error!("Failed to set property. Error: {:?}", e));
        }
    }
}

impl InspectType for StringProperty {}

crate::impl_inspect_type_internal!(StringProperty);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::testing_utils::{get_state, GetBlockExt};
    use crate::writer::Node;
    use inspect_format::{BlockType, PropertyFormat};

    impl StringProperty {
        fn load_data(&self) -> Option<String> {
            let mut data_index = None;
            self.get_block(|b| data_index = Some(b.property_extent_index().unwrap()));
            self.inner.inner_ref().and_then(|inner_ref| {
                inner_ref
                    .state
                    .try_lock()
                    .and_then(|state| state.load_string(data_index.unwrap()))
                    .ok()
            })
        }
    }

    #[fuchsia::test]
    fn basic_string_property() {
        // Create and use a default value.
        let default = StringProperty::default();
        default.set("test");

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");
        {
            let property = node.create_string("property", "test");
            property.get_block(|property_block| {
                assert_eq!(property_block.block_type(), BlockType::BufferValue);
                assert_eq!(property_block.total_length().unwrap(), 0);
                assert_eq!(
                    property_block.property_format().unwrap(),
                    PropertyFormat::StringReference
                );
            });
            assert_eq!(property.load_data().unwrap(), "test");
            node.get_block(|node_block| {
                assert_eq!(node_block.child_count().unwrap(), 1);
            });

            property.set("test-set");
            property.get_block(|property_block| {
                assert_eq!(property_block.total_length().unwrap(), 0);
            });
            assert_eq!(property.load_data().unwrap(), "test-set");
        }
        node.get_block(|node_block| {
            assert_eq!(node_block.child_count().unwrap(), 0);
        });
    }

    #[fuchsia::test]
    fn string_property_interning() {
        // Create and use a default value.
        let default = StringProperty::default();
        default.set("test");

        let state = get_state(4096);
        let root = Node::new_root(state);
        let node = root.create_child("node");

        let property1 = node.create_string("property", "test");
        let mut name_idx_1 = None;
        let mut payload_idx_1 = None;
        property1.get_block(|property_block| {
            name_idx_1 = Some(property_block.name_index().unwrap());
            payload_idx_1 = Some(property_block.property_extent_index().unwrap());
        });

        let property2 = node.create_string("test", "property");
        let mut name_idx_2 = None;
        let mut payload_idx_2 = None;
        property2.get_block(|property_block| {
            name_idx_2 = Some(property_block.name_index().unwrap());
            payload_idx_2 = Some(property_block.property_extent_index().unwrap());
        });

        assert_eq!(property1.load_data().unwrap(), "test");
        assert_eq!(property2.load_data().unwrap(), "property");
        assert_eq!(name_idx_1.unwrap(), payload_idx_2.unwrap());
        assert_eq!(name_idx_2.unwrap(), payload_idx_1.unwrap());
    }
}
