// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Safe bindings for the driver runtime dispatcher stable ABI

use crate::fdf_sys::*;

use core::future::Future;
use core::ptr::{addr_of_mut, NonNull};
use core::task::Context;
use std::cell::UnsafeCell;
use std::sync::{Arc, Mutex, Weak};

use fuchsia_zircon::Status;

use futures::future::{BoxFuture, FutureExt};
use futures::task::{waker_ref, ArcWake};
use futures::{SinkExt, StreamExt};

#[derive(Debug)]
pub struct Dispatcher(NonNull<fdf_dispatcher_t>);

// SAFETY: The api of fdf_dispatcher_t is thread safe.
unsafe impl Send for Dispatcher {}
unsafe impl Sync for Dispatcher {}

impl Dispatcher {
    pub fn post_task_sync(&self, p: impl TaskCallback) -> Result<Task, Status> {
        // SAFETY: the fdf dispatcher is valid by construction and can provide an async dispatcher.
        let async_dispatcher = unsafe { fdf_dispatcher_get_async_dispatcher(self.0.as_ptr()) };
        let task_arc = Arc::new(UnsafeCell::new(TaskFunc {
            task: async_task { handler: Some(Task::call), ..Default::default() },
            dispatcher: async_dispatcher,
            func: Box::new(p),
        }));
        let task = Task(Arc::downgrade(&task_arc));

        let task_cell = Arc::into_raw(task_arc);
        // SAFETY: we need a raw mut pointer to give to async_post_task. From
        // when we call that function to when the task is cancelled or the
        // callback is called, the driver runtime owns the contents of that
        // object and we will not manipulate it. So even though the Arc only
        // gives us a shared reference, it's fine to give the runtime a
        // mutable pointer to it.
        let res = unsafe {
            let task_ptr = addr_of_mut!((*UnsafeCell::raw_get(task_cell)).task);
            async_post_task(async_dispatcher, task_ptr)
        };
        if res != ZX_OK {
            // SAFETY: `TaskFunc::call` will never be called now so dispose of
            // the long-lived reference we just created.
            unsafe { Arc::decrement_strong_count(task_cell) }
            Err(Status::from_raw(res))
        } else {
            Ok(task)
        }
    }

    pub fn spawn_task(
        &self,
        future: impl Future<Output = ()> + 'static + Send,
    ) -> Result<(), Status> {
        let task = Arc::new(TaskFuture {
            future: Mutex::new(Some(future.boxed())),
            dispatcher: Dispatcher(self.0),
        });
        task.queue()
    }
}

pub trait TaskCallback: FnOnce(Status) + 'static + Send + Sync {}
impl<T> TaskCallback for T where T: FnOnce(Status) + 'static + Send + Sync {}

struct TaskFuture {
    future: Mutex<Option<BoxFuture<'static, ()>>>,
    dispatcher: Dispatcher,
}

impl ArcWake for TaskFuture {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        match arc_self.queue() {
            Err(e) if e == Status::from_raw(ZX_ERR_BAD_STATE) => {
                // the dispatcher is shutting down so drop the future, if there
                // is one, to cancel it.
                let mut future_slot = arc_self.future.lock().unwrap();
                core::mem::drop(future_slot.take());
            }
            res => res.expect("Unexpected error waking dispatcher task"),
        }
    }
}

impl TaskFuture {
    /// Posts a task to progress the currently stored future. The task will
    /// consume the future if the future is ready after the next poll.
    /// Otherwise, the future is kept to be polled again after being woken.
    fn queue(self: &Arc<Self>) -> Result<(), Status> {
        let arc_self = self.clone();
        self.dispatcher
            .post_task_sync(move |status| {
                let mut future_slot = arc_self.future.lock().unwrap();
                // if we're cancelled, drop the future we're waiting on.
                if status != Status::from_raw(ZX_OK) {
                    core::mem::drop(future_slot.take());
                    return;
                }

                let Some(mut future) = future_slot.take() else {
                    return;
                };
                let waker = waker_ref(&arc_self);
                let context = &mut Context::from_waker(&waker);
                if future.as_mut().poll(context).is_pending() {
                    *future_slot = Some(future);
                }
            })
            .map(|_| ())
    }
}

#[repr(C)]
struct TaskFunc {
    task: async_task,
    dispatcher: *mut async_dispatcher,
    func: Box<dyn TaskCallback>,
}

pub struct Task(Weak<UnsafeCell<TaskFunc>>);

impl Task {
    /// Attempts to cancel the task represented by this object, if it hasn't already
    /// been cancelled or started running its callback.
    pub fn cancel(self) -> Result<(), Status> {
        let Some(task_func) = self.0.upgrade() else {
            // the task must have already been cancelled or finished.
            return Err(Status::from_raw(ZX_ERR_NOT_FOUND));
        };
        // SAFETY: At this point, there are three components that have access
        // to the contents of the `TaskFunc`:
        // - this function, which just upgraded its weak reference to a strong one,
        // unless the callback has already been called in which case it will have
        // failed to upgrade above.
        // - the callback, which will fail to `try_unwrap` its strong reference
        // because we just upgraded to a second strong reference.
        // - the driver runtime async executor that is in primary control of the
        // object and the only writer to the object.
        //
        // Here we are essentially recovering the mutable pointer to the `async_task`
        // struct contained in the `TaskFunc` for the driver runtime to pass it to
        // `async_cancel_task`. Because of the interlock provided by the Arc
        // upgrade/try_unwrap dance mentioned above, we know that this is the only
        // mutable pointer to the object in rust and can pull it out of the
        // `UnsafeCell`.
        let task_raw = UnsafeCell::raw_get(&*task_func);

        // SAFETY: since we hold a strong reference to the task struct, we can
        // assume that it will exist here.
        let res = unsafe {
            let task = addr_of_mut!((*task_raw).task);
            let dispatcher = (*task_raw).dispatcher;
            async_cancel_task(dispatcher, task)
        };
        if res == ZX_OK {
            // SAFETY: according to the async library's promises, if cancel task
            // returned ZX_OK, the task was cancelled and the callback will
            // never be called so we need to let the instance of the Arc
            // that would have been used by the callback get dropped.
            unsafe { Arc::decrement_strong_count(task_raw) }
            Ok(())
        } else {
            // SAFETY: if it returned an error, the task has already started
            // running its callback handler (`Self::call`) and that will dispose
            // of the reconstituted `Arc`.
            Err(Status::from_raw(res))
        }
    }

    extern "C" fn call(_dispatcher: *mut async_dispatcher, task: *mut async_task, status: i32) {
        // SAFETY: the async api promises that this function will only be called
        // up to once, so we can reconstitute the `Arc` and let it get dropped.
        let task = unsafe { Arc::from_raw(task as *const UnsafeCell<TaskFunc>) };
        // SAFETY: if we can't get a mut ref from the arc, then the task is already
        // being cancelled, so we don't want to call it.
        if let Some(task) = Arc::try_unwrap(task).ok() {
            (task.into_inner().func)(Status::from_raw(status));
        }
    }
}

#[cfg(test)]
pub(crate) mod test {
    use core::ffi::c_void;
    use core::ptr::null_mut;

    use std::ffi::c_char;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Once;

    use super::*;

    static GLOBAL_DRIVER_ENV: Once = Once::new();

    pub fn ensure_driver_env() {
        GLOBAL_DRIVER_ENV.call_once(|| {
            // SAFETY: calling fdf_env_start, which does not have any soundness
            // concerns for rust code, and this is only used in tests.
            unsafe {
                assert_eq!(fdf_env_start(), ZX_OK);
            }
        });
    }

    #[repr(C)]
    struct SendingShutdownObserver {
        observer: fdf_dispatcher_shutdown_observer,
        shutdown_tx: Sender<()>,
    }

    // SAFETY: this callback function must only be used in situations where the
    // observer will live past when it is called, and it must not be moved.
    unsafe extern "C" fn shutdown_observer(
        _dispatcher: *mut fdf_dispatcher_t,
        observer: *mut fdf_dispatcher_shutdown_observer_t,
    ) {
        let observer = observer as *mut SendingShutdownObserver;
        // SAFETY: this function is called before the observer has been destroyed.
        unsafe { &*observer }.shutdown_tx.send(()).unwrap();
    }

    pub fn with_raw_dispatcher<T>(name: &str, flags: u32, p: impl FnOnce(&Dispatcher) -> T) -> T {
        ensure_driver_env();

        let (shutdown_tx, shutdown_rx) = channel();
        let mut dispatcher = null_mut();
        let mut observer = SendingShutdownObserver {
            observer: fdf_dispatcher_shutdown_observer { handler: Some(shutdown_observer) },
            shutdown_tx,
        };
        // SAFETY: The pointers we pass to this function are all stable for the
        // duration of this function, and are not available to copy or clone to
        // client code (only through a ref to the non-`Clone`` `Dispatcher`
        // wrapper).
        let res = unsafe {
            fdf_env_dispatcher_create_with_owner(
                &mut observer as *mut _ as *mut c_void,
                flags,
                name.as_ptr() as *const c_char,
                name.len(),
                "".as_ptr() as *const c_char,
                0 as usize,
                &mut observer.observer,
                &mut dispatcher,
            )
        };
        assert_eq!(res, ZX_OK);
        let dispatcher = Dispatcher(NonNull::new(dispatcher).unwrap());

        let res = p(&dispatcher);

        // SAFETY: this initiates the dispatcher shutdown on a driver runtime
        // thread. When all tasks on the dispatcher have completed, the wait
        // on the shutdown_rx below will end and we can tear it down.
        unsafe {
            fdf_dispatcher_shutdown_async(dispatcher.0.as_ptr());
        }

        shutdown_rx.recv().unwrap();

        // SAFETY: we verify that the dispatcher has no tasks left queued in it,
        // just because this is testing code.
        assert!(!unsafe { fdf_env_dispatcher_has_queued_tasks(dispatcher.0.as_ptr()) });
        // SAFETY: the dispatcher has shut down and has no more tasks on it, so
        // destroy it.
        unsafe { fdf_dispatcher_destroy(dispatcher.0.as_ptr()) };

        res
    }

    #[test]
    fn start_test_dispatcher() {
        with_raw_dispatcher("testing", 0, |dispatcher| {
            println!("hello {dispatcher:?}");
        })
    }

    #[test]
    fn post_task_on_dispatcher() {
        with_raw_dispatcher("testing task", 0, |dispatcher| {
            let (tx, rx) = channel();
            dispatcher
                .post_task_sync(move |status| {
                    assert_eq!(status, Status::from_raw(ZX_OK));
                    tx.send(status).unwrap();
                })
                .unwrap();
            assert_eq!(rx.recv().unwrap(), Status::from_raw(ZX_OK));
        });
    }

    #[test]
    fn cancel_task_on_dispatcher() {
        with_raw_dispatcher("testing task", 0, move |dispatcher| {
            // we want to retry this a bunch because sometimes the task might
            // fire before we get a chance to cancel it. We will panic out of
            // this if:
            // - the task can't be cancelled due to NOT_FOUND, but it doesn't send a message
            // - the task can't be cancelled for any other reason
            // - the task was cancelled but did send a message
            // - we never successfully cancelled the task
            // we will pass the test when we successfully cancel the task
            for i in 0..50 {
                println!("cancel attempt #{i}");
                let (tx, rx) = channel();
                let task = dispatcher.post_task_sync(move |status| {
                    tx.send(status).unwrap();
                })?;

                match task.cancel() {
                    Err(e) if e == Status::from_raw(ZX_ERR_NOT_FOUND) => {
                        println!("not the case we were looking for (cancelled too late)");
                        assert_eq!(rx.recv().unwrap(), Status::from_raw(ZX_OK));
                    }
                    Ok(_) => {
                        // since `tx` should be dropped when the task is
                        // cancelled, the recv should error with no messages.
                        rx.recv()
                            .expect_err("no message should have been sent from the cancelled task");
                        println!(
                            "successfully cancelled without receiving a message from the task!"
                        );
                        return Ok(());
                    }
                    res @ Err(_) => res.unwrap(),
                }
            }
            println!("did not manage to cancel the task in few enough attempts");
            Err(Status::from_raw(ZX_ERR_TIMED_OUT))
        })
        .unwrap()
    }

    async fn ping(
        mut tx: futures::channel::mpsc::Sender<u8>,
        mut rx: futures::channel::mpsc::Receiver<u8>,
    ) {
        println!("starting ping!");
        tx.send(0).await.unwrap();
        while let Some(next) = rx.next().await {
            println!("ping! {next}");
            tx.send(next + 1).await.unwrap();
        }
    }

    async fn pong(
        fin_tx: std::sync::mpsc::Sender<()>,
        mut tx: futures::channel::mpsc::Sender<u8>,
        mut rx: futures::channel::mpsc::Receiver<u8>,
    ) {
        println!("starting pong!");
        while let Some(next) = rx.next().await {
            println!("pong! {next}");
            if next > 10 {
                println!("bye!");
                break;
            }
            tx.send(next + 1).await.unwrap();
        }
        fin_tx.send(()).unwrap();
    }

    #[test]
    fn async_ping_pong() {
        with_raw_dispatcher("async ping pong", 0, |dispatcher| {
            let (fin_tx, fin_rx) = std::sync::mpsc::channel();
            let (ping_tx, pong_rx) = futures::channel::mpsc::channel(10);
            let (pong_tx, ping_rx) = futures::channel::mpsc::channel(10);
            dispatcher.spawn_task(ping(ping_tx, ping_rx)).unwrap();
            dispatcher.spawn_task(pong(fin_tx, pong_tx, pong_rx)).unwrap();

            fin_rx.recv().expect("to receive final value");
        });
    }

    async fn slow_pong(
        fin_tx: std::sync::mpsc::Sender<()>,
        mut tx: futures::channel::mpsc::Sender<u8>,
        mut rx: futures::channel::mpsc::Receiver<u8>,
    ) {
        use fuchsia_zircon::Duration;
        println!("starting pong!");
        while let Some(next) = rx.next().await {
            println!("pong! {next}");
            fuchsia_async::Timer::new(fuchsia_async::Time::after(Duration::from_seconds(1))).await;
            if next > 10 {
                println!("bye!");
                break;
            }
            tx.send(next + 1).await.unwrap();
        }
        fin_tx.send(()).unwrap();
    }

    #[test]
    fn mixed_executor_async_ping_pong() {
        with_raw_dispatcher("async ping pong", 0, |dispatcher| {
            let (fin_tx, fin_rx) = std::sync::mpsc::channel();
            let (ping_tx, pong_rx) = futures::channel::mpsc::channel(10);
            let (pong_tx, ping_rx) = futures::channel::mpsc::channel(10);

            // spawn ping on the driver dispatcher
            dispatcher.spawn_task(ping(ping_tx, ping_rx)).unwrap();

            // and run pong on the fuchsia_async executor
            let mut executor = fuchsia_async::LocalExecutor::new();
            executor.run_singlethreaded(slow_pong(fin_tx, pong_tx, pong_rx));

            fin_rx.recv().expect("to receive final value");
        });
    }
}
