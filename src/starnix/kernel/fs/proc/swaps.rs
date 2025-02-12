// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{
    DynamicFile, DynamicFileBuf, DynamicFileSource, FileSystemHandle, FsNodeHandle, FsNodeInfo,
    FsNodeOps,
};
use starnix_logging::track_stub;
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::mode;

pub fn swaps_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        SwapsFile::new_node(),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}

#[derive(Clone)]
struct SwapsFile;
impl SwapsFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self)
    }
}

impl DynamicFileSource for SwapsFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/322874154"), "/proc/swaps includes Kernel::swap_files");
        writeln!(sink, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority")?;
        Ok(())
    }
}
