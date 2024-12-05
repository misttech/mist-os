// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dynamic_thread_spawner::DynamicThreadSpawner;
use crate::task::{CurrentTask, Kernel, Task, ThreadGroup};
use fragile::Fragile;
use fuchsia_async as fasync;
use pin_project::pin_project;
use starnix_sync::{Locked, Unlocked};
use starnix_types::ownership::{OwnedRef, TempRef, WeakRef};
use starnix_uapi::errors::Errno;
use starnix_uapi::{errno, error};
use std::cell::{RefCell, RefMut};
use std::ffi::CString;
use std::future::Future;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::{OnceLock, Weak};
use std::task::{Context, Poll};

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
    spawner: OnceLock<DynamicThreadSpawner>,

    /// Information about the main system task that is bound to the kernel main thread.
    system_task: OnceLock<SystemTask>,

    /// A `RefCell` containing an `Unlocked` state for the lock ordering purposes.
    unlocked_for_async: UnlockedForAsync,

    /// A weak reference to the kernel owning this struct.
    kernel: Weak<Kernel>,
}

impl KernelThreads {
    /// Create a KernelThreads object for the given Kernel.
    ///
    /// Must be called in the initial Starnix process on a thread with an async executor. This
    /// function captures the async executor for this thread for use with spawned futures.
    ///
    /// Used during kernel boot.
    pub fn new(kernel: Weak<Kernel>) -> Self {
        KernelThreads {
            starnix_process: fuchsia_runtime::process_self()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate process self"),
            ehandle: fasync::EHandle::local(),
            spawner: Default::default(),
            system_task: Default::default(),
            unlocked_for_async: UnlockedForAsync::new(),
            kernel,
        }
    }

    /// Initialize this object with the system task that will be used for spawned threads.
    ///
    /// This function must be called before this object is used to spawn threads.
    pub fn init(&self, system_task: CurrentTask) -> Result<(), Errno> {
        self.system_task.set(SystemTask::new(system_task)).map_err(|_| errno!(EEXIST))?;
        self.spawner
            .set(DynamicThreadSpawner::new(2, self.system_task().weak_task()))
            .map_err(|_| errno!(EEXIST))?;
        Ok(())
    }

    /// Spawn an async task in the main async executor to await the given future.
    ///
    /// Use this function to run async tasks in the background. These tasks cannot block or else
    /// they will starve the main async executor.
    ///
    /// Prefer this function to `spawn` for non-blocking work.
    pub fn spawn_future(&self, future: impl Future<Output = ()> + 'static) {
        self.ehandle.spawn_local_detached(WrappedFuture(self.kernel.clone(), future));
    }

    /// Spawn a thread in the main starnix process to run the given function.
    ///
    /// Use this function to work in the background that involves blocking. Prefer `spawn_future`
    /// for non-blocking work.
    ///
    /// The threads spawned by this function come from the `spawner()` thread pool, which means
    /// they can be used either for long-lived work or for short-lived work. The thread pool keeps
    /// a few idle threads around to reduce the overhead for spawning threads for short-lived work.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) + Send + 'static,
    {
        self.spawner().spawn(f)
    }

    /// The dynamic thread spawner used to spawn threads.
    ///
    /// To spawn a thread in this thread pool, use `spawn()`.
    pub fn spawner(&self) -> &DynamicThreadSpawner {
        self.spawner.get().as_ref().unwrap()
    }

    /// Access the `CurrentTask` for the kernel main thread.
    ///
    /// This function can only be called from the kernel main thread itself.
    pub fn system_task(&self) -> &CurrentTask {
        self.system_task.get().expect("KernelThreads::init must be called").system_task.get()
    }

    /// Access the `Unlocked` state.
    ///
    /// This function is intended for limited use in async contexts and can only be called from the
    /// kernel main thread.
    pub fn unlocked_for_async(&self) -> RefMut<'_, Locked<'static, Unlocked>> {
        self.unlocked_for_async.unlocked.get().borrow_mut()
    }

    /// Access the `ThreadGroup` for the system tasks.
    ///
    /// This function can be safely called from anywhere as soon as `KernelThreads::init` has been
    /// called.
    pub fn system_thread_group(&self) -> TempRef<'_, ThreadGroup> {
        TempRef::into_static(
            self.system_task
                .get()
                .expect("KernelThreads::init must be called")
                .system_thread_group
                .upgrade()
                .expect("System task must be still alive"),
        )
    }
}

impl Drop for KernelThreads {
    fn drop(&mut self) {
        let mut locked = Unlocked::new(); // TODO: Replace with .release
        if let Some(system_task) = self.system_task.take() {
            system_task.system_task.into_inner().release(&mut locked);
        }
    }
}

/// Create a new system task, register it on the thread and run the given closure with it.
pub fn with_new_current_task<F, R>(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &WeakRef<Task>,
    f: F,
) -> Result<R, Errno>
where
    F: FnOnce(&mut Locked<'_, Unlocked>, &CurrentTask) -> R,
{
    let current_task = {
        let Some(system_task) = system_task.upgrade() else {
            return error!(ESRCH);
        };
        CurrentTask::create_kernel_thread(
            locked,
            &system_task,
            CString::new("[kthreadd]").unwrap(),
        )?
    };
    let result = f(locked, &current_task);
    current_task.release(locked);
    Ok(result)
}

struct SystemTask {
    /// The system task is bound to the kernel main thread. `Fragile` ensures a runtime crash if it
    /// is accessed from any other thread.
    system_task: Fragile<CurrentTask>,

    /// The system `ThreadGroup` is accessible from everywhere.
    system_thread_group: WeakRef<ThreadGroup>,
}

struct UnlockedForAsync {
    unlocked: Fragile<RefCell<Locked<'static, Unlocked>>>,
}

impl UnlockedForAsync {
    fn new() -> Self {
        Self { unlocked: Fragile::new(RefCell::new(Unlocked::new())) }
    }
}

impl SystemTask {
    fn new(system_task: CurrentTask) -> Self {
        let system_thread_group = OwnedRef::downgrade(&system_task.thread_group);
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
            kernel
                .kthreads
                .system_task()
                .trigger_delayed_releaser(kernel.kthreads.unlocked_for_async().deref_mut());
        }
        result
    }
}
