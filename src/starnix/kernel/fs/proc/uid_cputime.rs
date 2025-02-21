// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::{FileSystemHandle, FsNodeHandle, StaticDirectoryBuilder, StubEmptyFile};
use starnix_logging::bug_ref;
use starnix_uapi::mode;

pub fn uid_cputime_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.entry(
        current_task,
        "remove_uid_range",
        StubEmptyFile::new_node(
            "/proc/uid_cputime/remove_uid_range",
            bug_ref!("https://fxbug.dev/322894025"),
        ),
        mode!(IFREG, 0o222),
    );
    dir.entry(
        current_task,
        "show_uid_stat",
        StubEmptyFile::new_node(
            "/proc/uid_cputime/show_uid_stat",
            bug_ref!("https://fxbug.dev/322893886"),
        ),
        mode!(IFREG, 0444),
    );
    dir.build(current_task)
}
