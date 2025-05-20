// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common utilities used by directory related tests.
//!
//! Most assertions are macros as they need to call async functions themselves.  As a typical test
//! will have multiple assertions, it save a bit of typing to write `assert_something!(arg)`
//! instead of `assert_something(arg).await`.

use byteorder::{LittleEndian, WriteBytesExt};
use fidl_fuchsia_io as fio;
use std::convert::TryInto as _;
use std::io::Write;

/// A helper to build the "expected" output for a `ReadDirents` call from the Directory protocol in
/// fuchsia.io.
pub struct DirentsSameInodeBuilder {
    expected: Vec<u8>,
    inode: u64,
}

impl DirentsSameInodeBuilder {
    pub fn new(inode: u64) -> Self {
        DirentsSameInodeBuilder { expected: vec![], inode }
    }

    pub fn add(&mut self, type_: fio::DirentType, name: &[u8]) -> &mut Self {
        assert!(
            name.len() <= fio::MAX_NAME_LENGTH as usize,
            "Expected entry name should not exceed MAX_FILENAME ({}) bytes.\n\
             Got: {:?}\n\
             Length: {} bytes",
            fio::MAX_NAME_LENGTH,
            name,
            name.len()
        );

        self.expected.write_u64::<LittleEndian>(self.inode).unwrap();
        self.expected.write_u8(name.len().try_into().unwrap()).unwrap();
        self.expected.write_u8(type_.into_primitive()).unwrap();
        self.expected.write_all(name).unwrap();

        self
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.expected
    }
}
