// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::writer::{Heap, State};
use inspect_format::{Block, Container};
use std::sync::Arc;

pub fn get_state(size: usize) -> State {
    let (container, storage) = Container::read_and_write(size).unwrap();
    let heap = Heap::new(container).unwrap();
    State::create(heap, Arc::new(storage)).unwrap()
}

pub trait GetBlockExt: crate::private::InspectTypeInternal {
    fn get_block<F>(&self, callback: F)
    where
        F: FnOnce(&Block<&Container>) -> (),
    {
        let block_index = self.block_index().expect("block index is set");
        let state = self.state().expect("state is set");
        state.get_block(block_index, callback)
    }

    fn get_block_mut<F>(&self, callback: F)
    where
        F: FnOnce(&mut Block<&mut Container>) -> (),
    {
        let block_index = self.block_index().expect("block index is set");
        let state = self.state().expect("state is set");
        state.get_block_mut(block_index, callback)
    }
}

impl<T> GetBlockExt for T where T: crate::private::InspectTypeInternal {}

#[macro_export]
macro_rules! assert_update_is_atomic {
    ($updateable_thing:ident, $($func:tt)+) => {{
        // some types (eg Node) get their state method from InspectTypeInternal,
        // but some (eg Inspector) just have them as regular methods
        #[allow(unused_imports)]
        use crate::writer::types::base::private::InspectTypeInternal;
        let gen = $updateable_thing
            .state()
            .unwrap()
            .with_current_header(|header| header.header_generation_count().unwrap());
        $updateable_thing.atomic_update($($func)+);

        let new_gen = $updateable_thing
            .state()
            .unwrap()
            .with_current_header(|header| header.header_generation_count().unwrap());

        let num_gen_updates = (new_gen - gen) / 2;
        if num_gen_updates != 1 {
            panic!(concat!("update function did not have exactly one transaction.",
                    "\nTransaction count: {}",
                    "\nOriginal generation count: {}",
                    "\nCurrent generation count: {}"),
                    num_gen_updates, gen, new_gen);
        }
    }}
}
