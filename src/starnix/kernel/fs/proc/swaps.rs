// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::vfs::{DynamicFile, DynamicFileBuf, DynamicFileSource, FsNodeOps};
use starnix_logging::track_stub;
use starnix_uapi::errors::Errno;

#[derive(Clone)]
pub struct SwapsFile;
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
