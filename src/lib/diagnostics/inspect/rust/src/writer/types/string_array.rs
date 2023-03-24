// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{
    private::InspectTypeInternal, ArrayProperty, Inner, InnerValueType, InspectType, State,
    StringReference,
};
use inspect_format::BlockIndex;

#[derive(Debug, PartialEq, Eq, Default)]
pub struct StringArrayProperty {
    inner: Inner<InnerValueType>,
}

impl InspectType for StringArrayProperty {}

impl InspectTypeInternal for StringArrayProperty {
    fn new(state: State, block_index: BlockIndex) -> Self {
        Self { inner: Inner::new(state, block_index) }
    }

    fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    fn new_no_op() -> Self {
        Self { inner: Inner::None }
    }

    fn state(&self) -> Option<State> {
        Some(self.inner.inner_ref()?.state.clone())
    }

    fn block_index(&self) -> Option<BlockIndex> {
        Some(self.inner.inner_ref()?.block_index)
    }
}

impl ArrayProperty for StringArrayProperty {
    type Type = StringReference;

    fn set(&self, index: usize, value: impl Into<Self::Type>) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| {
                    state.set_array_string_slot(inner_ref.block_index, index, value.into())
                })
                .ok();
        }
    }

    fn clear(&self) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.clear_array(inner_ref.block_index, 0))
                .ok();
        }
    }
}

impl Drop for StringArrayProperty {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assert_json_diff,
        hierarchy::DiagnosticsHierarchy,
        writer::{testing_utils::GetBlockExt, Length},
        Inspector,
    };

    impl StringArrayProperty {
        pub fn load_string_slot(&self, slot: usize) -> Option<String> {
            self.inner.inner_ref().and_then(|inner_ref| {
                inner_ref
                    .state
                    .try_lock()
                    .and_then(|mut state| {
                        state.load_string(
                            state
                                .get_block(self.block_index().unwrap())
                                .array_get_string_index_slot(slot)?,
                        )
                    })
                    .ok()
            })
        }
    }

    #[fuchsia::test]
    fn string_array_property() {
        let inspector = Inspector::default();
        let root = inspector.root();
        let node = root.create_child("node");

        {
            let array = node.create_string_array("string_array", 5);
            assert_eq!(array.len().unwrap(), 5);
            node.get_block(|node_block| {
                assert_eq!(node_block.child_count().unwrap(), 1);
            });

            array.set(0, "0");
            array.set(1, "1");
            array.set(2, "2");
            array.set(3, "3");
            array.set(4, "4");

            // this should fail silently
            array.set(5, "5");
            assert!(array.load_string_slot(5).is_none());

            let expected: Vec<String> =
                vec!["0".into(), "1".into(), "2".into(), "3".into(), "4".into()];

            assert_json_diff!(inspector, root: {
                node: {
                    string_array: expected,
                },
            });

            array.clear();

            let expected: Vec<String> = vec![String::new(); 5];

            assert_json_diff!(inspector, root: {
                node: {
                    string_array: expected,
                },
            });

            assert!(array.load_string_slot(5).is_none());
        }

        node.get_block(|node_block| {
            assert_eq!(node_block.child_count().unwrap(), 0);
        });
    }
}
