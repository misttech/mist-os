// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{BytesFile, FileSystemHandle, FsNodeHandle, FsNodeInfo};
use starnix_uapi::auth::FsCred;
use starnix_uapi::mode;

pub fn cmdline_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut cmdline = Vec::from(current_task.kernel().cmdline.clone());
    cmdline.push(b'\n');
    fs.create_node(
        current_task,
        BytesFile::new_node(cmdline),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}
