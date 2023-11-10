// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use once_cell::sync::OnceCell;
use std::{ffi::CString, sync::Arc};

use crate::{
    dynamic_thread_spawner::DynamicThreadSpawner,
    fs::FsContext,
    task::{CurrentTask, Kernel, Task},
    types::*,
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
    pub spawner: DynamicThreadSpawner,

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
            spawner: DynamicThreadSpawner::new(2),
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
    pub fn init(&self, kernel: &Arc<Kernel>, fs: Arc<FsContext>) -> Result<(), Errno> {
        self.system_task
            .set(OwnedRefByRef::new(Task::create_kernel_task(
                kernel,
                CString::new("[kthreadd]").unwrap(),
                fs,
            )?))
            .map_err(|_| errno!(EEXIST))?;
        Ok(())
    }

    pub fn system_task(&self) -> &CurrentTask {
        self.system_task.get().as_ref().unwrap()
    }

    pub fn workaround_for_b297439724_new_system_task(
        &self,
    ) -> Result<OwnedRefByRef<CurrentTask>, Errno> {
        let system_task = self.system_task();
        Ok(OwnedRefByRef::new(Task::create_kernel_task(
            system_task.kernel(),
            CString::new("[workaround_for_b297439724]").unwrap(),
            system_task.fs().clone(),
        )?))
    }

    pub fn weak_system_task(&self) -> WeakRef<CurrentTask> {
        self.system_task.get().unwrap().into()
    }

    pub fn new_system_thread(&self) -> Result<CurrentTask, Errno> {
        Task::create_kernel_thread(self.system_task(), CString::new("[kthread]").unwrap())
    }

    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawner.spawn(f)
    }
}
