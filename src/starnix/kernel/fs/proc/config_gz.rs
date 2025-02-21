// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    DynamicFile, DynamicFileBuf, DynamicFileSource, FileSystemHandle, FsNodeHandle, FsNodeInfo,
    FsNodeOps,
};
use starnix_logging::log_error;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, mode};

pub fn config_gz_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        ConfigFile::new_node(),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}

#[derive(Clone, Debug)]
struct ConfigFile;
impl ConfigFile {
    fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self)
    }
}

impl DynamicFileSource for ConfigFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let contents = std::fs::read("/pkg/data/config.gz").map_err(|e| {
            log_error!("Error reading /pkg/data/config.gz: {e}");
            errno!(EIO)
        })?;
        sink.write(&contents);
        Ok(())
    }
}
