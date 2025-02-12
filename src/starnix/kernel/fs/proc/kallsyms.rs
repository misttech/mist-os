// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{BytesFile, FileSystemHandle, FsNodeHandle, FsNodeInfo, SimpleFileNode};
use starnix_logging::track_stub;
use starnix_uapi::auth::FsCred;
use starnix_uapi::mode;

pub fn kallsyms_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    fs.create_node(
        current_task,
        SimpleFileNode::new(|| {
            track_stub!(TODO("https://fxbug.dev/369067922"), "Provide a real /proc/kallsyms");
            Ok(BytesFile::new(b"0000000000000000 T security_inode_copy_up".to_vec()))
        }),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
}
