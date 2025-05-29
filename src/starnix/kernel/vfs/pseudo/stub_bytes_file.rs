// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::pseudo::simple_file::{BytesFile, BytesFileOps, SimpleFileNode};
use crate::vfs::FsNodeOps;
use starnix_sync::Mutex;
use starnix_uapi::auth::Capabilities;
use starnix_uapi::errors::Errno;
use std::borrow::Cow;

#[derive(Default)]
pub struct StubBytesFile {
    data: Mutex<Vec<u8>>,
}

impl StubBytesFile {
    #[track_caller]
    pub fn new_node(message: &'static str, bug: starnix_logging::BugRef) -> impl FsNodeOps {
        Self::new_node_with_data(message, bug, vec![])
    }

    #[track_caller]
    pub fn new_node_with_data(
        message: &'static str,
        bug: starnix_logging::BugRef,
        initial_data: impl Into<Vec<u8>>,
    ) -> impl FsNodeOps {
        let location = std::panic::Location::caller();
        let file = BytesFile::new(StubBytesFile {
            data: Mutex::new(initial_data.into()),
        });
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(bug, message, None, location);
            Ok(file.clone())
        })
    }

    #[track_caller]
    pub fn new_node_with_capabilities(
        message: &'static str,
        bug: starnix_logging::BugRef,
        capabilities: Capabilities,
    ) -> impl FsNodeOps {
        let location = std::panic::Location::caller();
        let file = BytesFile::new(Self::default());
        SimpleFileNode::new_with_capabilities(
            move || {
                starnix_logging::__track_stub_inner(bug, message, None, location);
                Ok(file.clone())
            },
            capabilities,
        )
    }
}

impl BytesFileOps for StubBytesFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }
}
