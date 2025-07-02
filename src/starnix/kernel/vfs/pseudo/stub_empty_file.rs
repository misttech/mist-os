// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::pseudo::simple_file::SimpleFileNode;
use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync, FileOps, FsNodeOps,
};
use starnix_logging::BugRef;
use std::panic::Location;

#[derive(Clone, Debug)]
pub struct StubEmptyFile;

impl StubEmptyFile {
    #[track_caller]
    pub fn new_node(message: &'static str, bug: BugRef) -> impl FsNodeOps {
        // This ensures the caller of this fn is recorded instead of the location of the closure.
        let location = Location::caller();
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(bug, message, None, location);
            Ok(StubEmptyFile)
        })
    }
}

impl FileOps for StubEmptyFile {
    fileops_impl_dataless!();
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
}
