// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{fileops_impl_dataless, fileops_impl_nonseekable, Anon, FileHandle, FileOps},
    task::CurrentTask,
    types::*,
};

pub struct PidFdFileObject {
    // In principle, we need some way to designate a Task that is durable for
    // the lifetime of the `PidFdFileObject`. In practice, we never actually
    // reuse pids and have no mechanism for tracking which pids have been freed.
    //
    // For now, we designate the Task using the pid itself. If/when we start
    // reusing pids, we'll need to reconsider this design.
    //
    // See `PidTable::allocate_pid` for a related comment.
    pub pid: pid_t,
}

pub fn new_pidfd(current_task: &CurrentTask, pid: pid_t, flags: OpenFlags) -> FileHandle {
    Anon::new_file(current_task, Box::new(PidFdFileObject { pid }), flags)
}

impl FileOps for PidFdFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_dataless!();
}
