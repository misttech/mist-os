// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::writer::{Heap, State};
use inspect_format::{Block, Container, WritableBlockContainer};

pub fn get_state(size: usize) -> State {
    let (container, storage) = Container::read_and_write(size).unwrap();
    let heap = Heap::new(container).unwrap();
    State::create(heap, storage).unwrap()
}

pub trait GetBlockExt: crate::private::InspectTypeInternal {
    fn get_block<F>(&self, callback: F)
    where
        F: FnOnce(&Block<Container>) -> (),
    {
        let block_index = self.block_index().expect("block index is set");
        let state = self.state().expect("state is set");
        state.get_block(block_index, callback)
    }
}

impl<T> GetBlockExt for T where T: crate::private::InspectTypeInternal {}
