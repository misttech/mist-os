// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{BytesFile, FileSystemHandle, FsNodeHandle, FsNodeInfo};
use starnix_uapi::auth::FsCred;
use starnix_uapi::mode;

/// A node that is empty, used as placeholder.
pub fn cgroups_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        BytesFile::new_node(vec![]),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}
