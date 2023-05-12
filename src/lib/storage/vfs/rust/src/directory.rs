// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module holding different kinds of pseudo directories and their building blocks.

use {crate::common::io2_conversions, fidl_fuchsia_io as fio};

#[macro_use]
pub mod test_utils;

pub mod common;

pub mod immutable;
pub mod mutable;

pub mod simple;

pub mod connection;
pub mod dirents_sink;
pub mod entry;
pub mod entry_container;
pub mod helper;
pub mod read_dirents;
pub mod traversal_position;
pub mod watchers;

/// A directory can be open either as a directory or a node.
pub struct DirectoryOptions {
    pub(crate) node: bool,
    pub(crate) rights: fio::Operations,
}

impl DirectoryOptions {
    pub(crate) fn to_io1(&self) -> fio::OpenFlags {
        let Self { node, rights } = self;
        let mut flags = io2_conversions::io2_to_io1(*rights);
        if *node {
            flags |= fio::OpenFlags::NODE_REFERENCE;
        }
        flags
    }
}
