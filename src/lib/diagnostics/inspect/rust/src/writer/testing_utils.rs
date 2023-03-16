// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::writer::{Heap, State};
use inspect_format::{Container, WritableBlockContainer};

pub fn get_state(size: usize) -> State {
    let (container, storage) = Container::read_and_write(size).unwrap();
    let heap = Heap::new(container).unwrap();
    State::create(heap, storage).unwrap()
}
