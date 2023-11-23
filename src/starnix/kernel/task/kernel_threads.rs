// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{dynamic_thread_spawner::DynamicThreadSpawner, task::CurrentTask};
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use once_cell::sync::OnceCell;
use starnix_uapi::{
    errno,
    errors::Errno,
    ownership::{OwnedRefByRef, ReleasableByRef},
};

/// The threads that the kernel runs internally.
///
/// These threads run in the main starnix process and outlive any specific userspace process.
pub struct KernelThreads {
    /// The main starnix process. This process is used to create new processes when using the
    /// restricted executor.
    pub starnix_process: zx::Process,

    /// A handle to the async executor running in `starnix_process`.
    ///
    /// You can spawn tasks on this executor using `fasync::EHandle::spawn_detached`.
    /// However, those task must not block. If you need to block, you can spawn a worker thread
    /// using `spawner`.
    pub ehandle: fasync::EHandle,

    /// The thread pool to spawn blocking calls to.
    spawner: OnceCell<DynamicThreadSpawner>,

    /// A task object for the kernel threads.
    system_task: OnceCell<OwnedRefByRef<CurrentTask>>,
}

impl Default for KernelThreads {
    fn default() -> Self {
        KernelThreads {
            starnix_process: fuchsia_runtime::process_self()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate process self"),
            ehandle: fasync::EHandle::local(),
            spawner: OnceCell::new(),
            system_task: OnceCell::new(),
        }
    }
}

impl Drop for KernelThreads {
    fn drop(&mut self) {
        if let Some(system_task) = self.system_task.take() {
            system_task.release(());
        }
    }
}

impl KernelThreads {
    pub fn init(&self, system_task: OwnedRefByRef<CurrentTask>) -> Result<(), Errno> {
        self.system_task.set(system_task).map_err(|_| errno!(EEXIST))?;
        self.spawner
            .set(DynamicThreadSpawner::new(2, self.system_task().weak_task()))
            .map_err(|_| errno!(EEXIST))?;
        Ok(())
    }

    pub fn spawner(&self) -> &DynamicThreadSpawner {
        self.spawner.get().as_ref().unwrap()
    }

    pub fn system_task(&self) -> &CurrentTask {
        self.system_task.get().as_ref().unwrap()
    }

    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&CurrentTask) + Send + 'static,
    {
        self.spawner().spawn(f)
    }
}
