// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{FileSystemHandle, FsNodeHandle, StaticDirectoryBuilder};

pub fn device_tree_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut directory = StaticDirectoryBuilder::new(fs);
    for setup_function in &current_task.kernel().procfs_device_tree_setup {
        setup_function(&mut directory, current_task);
    }
    directory.build(current_task)
}
