// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, FileOps, FsNodeOps, SimpleFileNode,
};
use std::panic::Location;

#[derive(Clone, Debug)]
pub struct StubEmptyFile;

impl StubEmptyFile {
    #[track_caller]
    pub fn new_node(message: &'static str) -> impl FsNodeOps {
        // This ensures the caller of this fn is recorded instead of the location of the closure.
        let location = Location::caller();
        SimpleFileNode::new(move || {
            starnix_logging::__not_implemented_inner(None, message, None, location);
            Ok(StubEmptyFile)
        })
    }

    #[track_caller]
    pub fn new_node_with_bug(message: &'static str, bug_number: u64) -> impl FsNodeOps {
        // This ensures the caller of this fn is recorded instead of the location of the closure.
        let location = Location::caller();
        SimpleFileNode::new(move || {
            starnix_logging::__not_implemented_inner(Some(bug_number), message, None, location);
            Ok(StubEmptyFile)
        })
    }
}

impl FileOps for StubEmptyFile {
    fileops_impl_dataless!();
    fileops_impl_nonseekable!();
}
