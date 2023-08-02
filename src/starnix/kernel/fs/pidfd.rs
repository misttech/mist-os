// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::*,
    task::{CurrentTask, Task},
    types::*,
};

pub struct PidFdFileObject {
    _task: WeakRef<Task>,
}

pub fn new_pidfd(current_task: &CurrentTask, task: WeakRef<Task>, flags: OpenFlags) -> FileHandle {
    Anon::new_file(current_task, Box::new(PidFdFileObject { _task: task }), flags)
}

impl FileOps for PidFdFileObject {
    fileops_impl_nonseekable!();
    fileops_impl_dataless!();
}
