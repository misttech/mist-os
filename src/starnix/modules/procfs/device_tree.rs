// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::task::Kernel;
use starnix_core::vfs::pseudo::simple_directory::SimpleDirectory;
use starnix_core::vfs::{FileSystemHandle, FsNodeHandle};

pub fn device_tree_directory(kernel: &Kernel, fs: &FileSystemHandle) -> FsNodeHandle {
    let dir = SimpleDirectory::new();
    dir.edit(fs, |dir| {
        for setup_function in &kernel.procfs_device_tree_setup {
            setup_function(dir);
        }
    });
    // TODO: Validate the mode bits are correct.
    dir.into_node(fs, 0o777)
}
