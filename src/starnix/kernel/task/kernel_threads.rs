// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    dynamic_thread_spawner::DynamicThreadSpawner,
    task::{CurrentTask, Kernel, ThreadGroup},
};
use fragile::Fragile;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use lock_sequence::{Locked, Unlocked};
use once_cell::sync::OnceCell;
use pin_project::pin_project;
use starnix_uapi::{errno, errors::Errno, ownership::ReleasableByRef};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
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
    /// You can spawn tasks on this executor using `spawn_future`. However, those task must not
    /// block. If you need to block, you can spawn a worker thread using `spawner`.
    ehandle: fasync::EHandle,

    /// The thread pool to spawn blocking calls to.
    spawner: OnceCell<DynamicThreadSpawner>,

    /// Information about the main system task that is bound to the kernel main thread.
    system_task: OnceCell<SystemTask>,

    /// A weak reference to the kernel owning this struct.
    kernel: Weak<Kernel>,
}

impl KernelThreads {
    pub fn new(kernel: Weak<Kernel>) -> Self {
        KernelThreads {
            starnix_process: fuchsia_runtime::process_self()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate process self"),
            ehandle: fasync::EHandle::local(),
            spawner: Default::default(),
            system_task: Default::default(),
            kernel,
        }
    }
    pub fn init(&self, system_task: CurrentTask) -> Result<(), Errno> {
        self.system_task.set(SystemTask::new(system_task)).map_err(|_| errno!(EEXIST))?;
        self.spawner
            .set(DynamicThreadSpawner::new(2, self.system_task().weak_task()))
            .map_err(|_| errno!(EEXIST))?;
        Ok(())
    }

    pub fn spawn_future(&self, future: impl Future<Output = ()> + 'static) {
        self.ehandle.spawn_local_detached(WrappedFuture(self.kernel.clone(), future));
    }

    pub fn spawner(&self) -> &DynamicThreadSpawner {
        self.spawner.get().as_ref().unwrap()
    }

    /// Access the `CurrentTask` for the kernel main thread. This can only be called from the
    /// kernel main thread itself.
    pub fn system_task(&self) -> &CurrentTask {
        self.system_task.get().expect("KernelThreads::init must be called").system_task.get()
    }

    /// Access the `ThreadGroup` for the system tasks. This can be safely called from anywhere as
    /// soon as KernelThreads::init has been called.
    pub fn system_thread_group(&self) -> &Arc<ThreadGroup> {
        &self.system_task.get().expect("KernelThreads::init must be called").system_thread_group
    }

    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) + Send + 'static,
    {
        self.spawner().spawn(f)
    }
}

impl Drop for KernelThreads {
    fn drop(&mut self) {
        if let Some(system_task) = self.system_task.take() {
            system_task.system_task.into_inner().release(());
        }
    }
}

struct SystemTask {
    /// The system task is bound to the kernel main thread. `Fragile` ensures a runtime crash if it
    /// is accessed from any other thread.
    system_task: Fragile<CurrentTask>,

    /// The system `ThreadGroup` is accessible from everywhere.
    system_thread_group: Arc<ThreadGroup>,
}

impl SystemTask {
    fn new(system_task: CurrentTask) -> Self {
        let system_thread_group = Arc::clone(&system_task.thread_group);
        Self { system_task: system_task.into(), system_thread_group }
    }
}

#[pin_project]
struct WrappedFuture<F: Future<Output = ()> + 'static>(Weak<Kernel>, #[pin] F);
impl<F: Future<Output = ()> + 'static> Future for WrappedFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let kernel = self.0.clone();
        let result = self.project().1.poll(cx);

        if let Some(kernel) = kernel.upgrade() {
            kernel.kthreads.system_task().trigger_delayed_releaser();
        }
        result
    }
}
