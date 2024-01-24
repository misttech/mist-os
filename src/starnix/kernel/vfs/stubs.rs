// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{
    fileops_impl_dataless, fileops_impl_nonseekable, CurrentTask, FileOps, FsNode, FsNodeOps,
    SimpleFileNode,
};
use starnix_uapi::{device_type::DeviceType, errors::Errno, open_flags::OpenFlags};
use std::panic::Location;

#[derive(Clone, Debug)]
pub struct StubEmptyFile;

impl StubEmptyFile {
    #[track_caller]
    pub fn new_node(message: &'static str) -> impl FsNodeOps {
        // This ensures the caller of this fn is recorded instead of the location of the closure.
        let location = Location::caller();
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(None, message, None, location);
            Ok(StubEmptyFile)
        })
    }

    #[track_caller]
    pub fn new_node_with_bug(message: &'static str, bug_url: &'static str) -> impl FsNodeOps {
        // This ensures the caller of this fn is recorded instead of the location of the closure.
        let location = Location::caller();
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(Some(bug_url), message, None, location);
            Ok(StubEmptyFile)
        })
    }
}

impl FileOps for StubEmptyFile {
    fileops_impl_dataless!();
    fileops_impl_nonseekable!();
}

#[track_caller]
pub fn create_stub_device_with_bug(
    message: &'static str,
    bug_url: &'static str,
) -> impl Fn(&CurrentTask, DeviceType, &FsNode, OpenFlags) -> Result<Box<dyn FileOps>, Errno> + Clone
{
    // This ensures the caller of this fn is recorded instead of the location of the closure.
    let location = Location::caller();
    move |_current_task: &CurrentTask, _id: DeviceType, _node: &FsNode, _flags: OpenFlags| {
        starnix_logging::__track_stub_inner(Some(bug_url), message, None, location);
        starnix_uapi::errors::error!(ENODEV)
    }
}
