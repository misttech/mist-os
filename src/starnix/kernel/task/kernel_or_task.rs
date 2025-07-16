// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, Kernel, Task};

/// An object that can be either a Kernel or a CurrentTask.
///
/// This allows to retrieve the Kernel from it, and the task if it is available.
pub trait KernelOrTask<'a>: std::fmt::Debug + Clone + Copy {
    fn kernel(&self) -> &'a Kernel;
    fn maybe_task(&self) -> Option<&'a CurrentTask>;
}

impl<'a> KernelOrTask<'a> for &'a Kernel {
    fn kernel(&self) -> &'a Kernel {
        self
    }
    fn maybe_task(&self) -> Option<&'a CurrentTask> {
        None
    }
}

impl<'a> KernelOrTask<'a> for &'a CurrentTask {
    fn kernel(&self) -> &'a Kernel {
        (self as &Task).kernel()
    }
    fn maybe_task(&self) -> Option<&'a CurrentTask> {
        Some(self)
    }
}
