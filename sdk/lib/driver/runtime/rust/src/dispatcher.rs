// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Safe bindings for the driver runtime dispatcher stable ABI

use fdf_sys::*;

use core::cell::UnsafeCell;
use core::ffi;
use core::future::Future;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ptr::{addr_of_mut, null_mut, NonNull};
use core::task::Context;
use std::sync::{Arc, Mutex};

use zx::Status;

use futures::future::{BoxFuture, FutureExt};
use futures::task::{waker_ref, ArcWake};

pub use fdf_sys::fdf_dispatcher_t;

pub trait ShutdownObserverFn: Fn(DispatcherRef<'_>) + Send + Sync + 'static {}
impl<T> ShutdownObserverFn for T where T: Fn(DispatcherRef<'_>) + Send + Sync + 'static {}

/// A builder for [`Dispatcher`]s
#[derive(Default)]
pub struct DispatcherBuilder {
    options: u32,
    name: String,
    scheduler_role: String,
    shutdown_observer: Option<ShutdownObserver>,
}

impl DispatcherBuilder {
    /// See `FDF_DISPATCHER_OPTION_UNSYNCHRONIZED` in the C API
    pub(crate) const UNSYNCHRONIZED: u32 = 0b01;
    /// See `FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS` in the C API
    pub(crate) const ALLOW_THREAD_BLOCKING: u32 = 0b10;

    /// Creates a new [`DispatcherBuilder`] that can be used to configure a new dispatcher.
    /// For more information on the threading-related flags for the dispatcher, see
    /// https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets whether parallel callbacks in the callbacks set in the dispatcher are allowed. May
    /// not be set with [`Self::allow_thread_blocking`].
    ///
    /// See https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads
    /// for more information on the threading model of driver dispatchers.
    pub fn unsynchronized(mut self) -> Self {
        assert!(
            !self.allows_thread_blocking(),
            "you may not create an unsynchronized dispatcher that allows synchronous calls"
        );
        self.options = self.options | Self::UNSYNCHRONIZED;
        self
    }

    /// Whether or not this is an unsynchronized dispatcher
    pub fn is_unsynchronized(&self) -> bool {
        (self.options & Self::UNSYNCHRONIZED) == Self::UNSYNCHRONIZED
    }

    /// This dispatcher may not share zircon threads with other drivers. May not be set with
    /// [`Self::unsynchronized`].
    ///
    /// See https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads
    /// for more information on the threading model of driver dispatchers.
    pub fn allow_thread_blocking(mut self) -> Self {
        assert!(
            !self.is_unsynchronized(),
            "you may not create an unsynchronized dispatcher that allows synchronous calls"
        );
        self.options = self.options | Self::ALLOW_THREAD_BLOCKING;
        self
    }

    // Whether or not this dispatcher allows synchronous calls
    pub fn allows_thread_blocking(&self) -> bool {
        (self.options & Self::ALLOW_THREAD_BLOCKING) == Self::ALLOW_THREAD_BLOCKING
    }

    /// A descriptive name for this dispatcher that is used in debug output and process
    /// lists.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// A hint string for the runtime that may or may not impact the priority the work scheduled
    /// by this dispatcher is handled at. It may or may not impact the ability for other drivers
    /// to share zircon threads with the dispatcher.
    pub fn scheduler_role(mut self, role: &str) -> Self {
        self.scheduler_role = role.to_string();
        self
    }

    /// A callback to be called before after the dispatcher has completed asynchronous shutdown.
    pub fn shutdown_observer<F: ShutdownObserverFn>(mut self, shutdown_observer: F) -> Self {
        self.shutdown_observer = Some(ShutdownObserver::new(shutdown_observer));
        self
    }

    /// Create the dispatcher as configured by this object. This must be called from a
    /// thread managed by the driver runtime. The dispatcher returned is owned by the caller,
    /// and will initiate asynchronous shutdown when the object is dropped unless
    /// [`Dispatcher::release`] is called on it to convert it into an unowned [`DispatcherRef`].
    pub fn create(self) -> Result<Dispatcher, Status> {
        let mut out_dispatcher = null_mut();
        let options = self.options;
        let name = self.name.as_ptr() as *mut ffi::c_char;
        let name_len = self.name.len();
        let scheduler_role = self.scheduler_role.as_ptr() as *mut ffi::c_char;
        let scheduler_role_len = self.scheduler_role.len();
        let observer =
            self.shutdown_observer.unwrap_or_else(|| ShutdownObserver::new(|_| {})).into_ptr();
        // SAFETY: all arguments point to memory that will be available for the duration
        // of the call, except `observer`, which will be available until it is unallocated
        // by the dispatcher exit handler.
        Status::ok(unsafe {
            fdf_dispatcher_create(
                options,
                name,
                name_len,
                scheduler_role,
                scheduler_role_len,
                observer,
                &mut out_dispatcher,
            )
        })?;
        // SAFETY: `out_dispatcher` is valid by construction if `fdf_dispatcher_create` returns
        // ZX_OK.
        Ok(Dispatcher(unsafe { NonNull::new_unchecked(out_dispatcher) }))
    }

    /// As with [`Self::create`], this creates a new dispatcher as configured by this object, but
    /// instead of returning an owned reference it immediately releases the reference to be
    /// managed by the driver runtime.
    pub fn create_released(self) -> Result<DispatcherRef<'static>, Status> {
        self.create().map(Dispatcher::release)
    }
}

#[derive(Debug)]
pub struct Dispatcher(pub(crate) NonNull<fdf_dispatcher_t>);

// SAFETY: The api of fdf_dispatcher_t is thread safe.
unsafe impl Send for Dispatcher {}
unsafe impl Sync for Dispatcher {}

impl Dispatcher {
    /// Creates a dispatcher ref from a raw handle.
    ///
    /// # Safety
    ///
    /// Caller is responsible for ensuring that the given handle is valid and
    /// not owned by any other wrapper that will free it at an arbitrary
    /// time.
    pub(crate) unsafe fn from_raw(handle: NonNull<fdf_dispatcher_t>) -> Self {
        Self(handle)
    }

    fn get_raw_flags(&self) -> u32 {
        // SAFETY: the inner fdf_dispatcher_t is valid by construction
        unsafe { fdf_dispatcher_get_options(self.0.as_ptr()) }
    }

    /// Whether this dispatcher's tasks and futures can run on multiple threads at the same time.
    pub fn is_unsynchronized(&self) -> bool {
        (self.get_raw_flags() & DispatcherBuilder::UNSYNCHRONIZED) != 0
    }

    /// Whether this dispatcher is allowed to call blocking functions or not
    pub fn allows_thread_blocking(&self) -> bool {
        (self.get_raw_flags() & DispatcherBuilder::ALLOW_THREAD_BLOCKING) != 0
    }

    pub fn post_task_sync<'a>(&self, p: impl TaskCallback<'a>) -> Result<(), Status> {
        // SAFETY: the fdf dispatcher is valid by construction and can provide an async dispatcher.
        let async_dispatcher = unsafe { fdf_dispatcher_get_async_dispatcher(self.0.as_ptr()) };
        let task_arc = Arc::new(UnsafeCell::new(TaskFunc {
            task: async_task { handler: Some(TaskFunc::call), ..Default::default() },
            dispatcher: async_dispatcher,
            func: Box::new(p),
        }));

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
            Ok(())
        }
    }

    pub fn spawn_task<'a>(
        &'a self,
        future: impl Future<Output = ()> + 'a + Send,
    ) -> Result<(), Status> {
        let task = Arc::new(Task {
            future: Mutex::new(Some(future.boxed())),
            dispatcher: ManuallyDrop::new(Dispatcher(self.0)),
        });
        task.queue()
    }

    /// Releases ownership over this dispatcher and returns a [`DispatcherRef`]
    /// that can be used to access it. The lifetime of this reference is static because it will
    /// exist so long as this current driver is loaded, but the driver runtime will shut it down
    /// when the driver is unloaded.
    pub fn release(self) -> DispatcherRef<'static> {
        DispatcherRef(ManuallyDrop::new(self), PhantomData)
    }

    /// Returns a [`DispatcherRef`] that references this dispatcher with a lifetime constrained by
    /// `self`.
    pub fn as_dispatcher_ref(&self) -> DispatcherRef<'_> {
        DispatcherRef(ManuallyDrop::new(Dispatcher(self.0)), PhantomData)
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        // SAFETY: we only ever provide an owned `Dispatcher` to one owner, so when
        // that one is dropped we can invoke the shutdown of the dispatcher
        unsafe { fdf_dispatcher_shutdown_async(self.0.as_mut()) }
    }
}

/// An unowned reference to a driver runtime dispatcher such as is produced by calling
/// [`Dispatcher::release`]. When this object goes out of scope it won't shut down the dispatcher,
/// leaving that up to the driver runtime or another owner.
#[derive(Debug)]
pub struct DispatcherRef<'a>(ManuallyDrop<Dispatcher>, PhantomData<&'a Dispatcher>);

impl<'a> DispatcherRef<'a> {
    /// Creates a dispatcher ref from a raw handle.
    ///
    /// # Safety
    ///
    /// Caller is responsible for ensuring that the given handle is valid for
    /// the lifetime `'a`.
    pub unsafe fn from_raw(handle: NonNull<fdf_dispatcher_t>) -> Self {
        // SAFETY: Caller promises the handle is valid.
        Self(ManuallyDrop::new(unsafe { Dispatcher::from_raw(handle) }), PhantomData)
    }
}

impl<'a> Clone for DispatcherRef<'a> {
    fn clone(&self) -> Self {
        Self(ManuallyDrop::new(Dispatcher(self.0 .0)), PhantomData)
    }
}

impl<'a> core::ops::Deref for DispatcherRef<'a> {
    type Target = Dispatcher;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> core::ops::DerefMut for DispatcherRef<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub trait TaskCallback<'a>: FnOnce(Status) + 'a + Send + Sync {}
impl<'a, T> TaskCallback<'a> for T where T: FnOnce(Status) + 'a + Send + Sync {}

struct Task<'a> {
    future: Mutex<Option<BoxFuture<'a, ()>>>,
    dispatcher: ManuallyDrop<Dispatcher>,
}

impl<'a> ArcWake for Task<'a> {
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

impl<'a> Task<'a> {
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
struct TaskFunc<'a> {
    task: async_task,
    dispatcher: *mut async_dispatcher,
    func: Box<dyn TaskCallback<'a>>,
}

impl<'a> TaskFunc<'a> {
    extern "C" fn call(_dispatcher: *mut async_dispatcher, task: *mut async_task, status: i32) {
        // SAFETY: the async api promises that this function will only be called
        // up to once, so we can reconstitute the `Arc` and let it get dropped.
        let task = unsafe { Arc::from_raw(task as *const UnsafeCell<Self>) };
        // SAFETY: if we can't get a mut ref from the arc, then the task is already
        // being cancelled, so we don't want to call it.
        if let Some(task) = Arc::try_unwrap(task).ok() {
            (task.into_inner().func)(Status::from_raw(status));
        }
    }
}

/// A shutdown observer for [`fdf_dispatcher_create`] that can call any kind of callback instead of
/// just a C-compatible function when a dispatcher is shutdown.
///
/// # Safety
///
/// This object relies on a specific layout to allow it to be cast between a
/// `*mut fdf_dispatcher_shutdown_observer` and a `*mut ShutdownObserver`. To that end,
/// it is important that this struct stay both `#[repr(C)]` and that `observer` be its first member.
#[repr(C)]
struct ShutdownObserver {
    observer: fdf_dispatcher_shutdown_observer,
    shutdown_fn: Box<dyn ShutdownObserverFn>,
}

impl ShutdownObserver {
    /// Creates a new [`ShutdownObserver`] with `f` as the callback to run when a dispatcher
    /// finishes shutting down.
    fn new<F: ShutdownObserverFn>(f: F) -> Self {
        let shutdown_fn = Box::new(f);
        Self {
            observer: fdf_dispatcher_shutdown_observer { handler: Some(Self::handler) },
            shutdown_fn,
        }
    }

    /// Turns this object into a stable pointer suitable for passing to [`fdf_dispatcher_create`]
    /// by wrapping it in a [`Box`] and leaking it to be reconstituded by [`Self::handler`] when
    /// the dispatcher is shut down.
    fn into_ptr(self) -> *mut fdf_dispatcher_shutdown_observer {
        // Note: this relies on the assumption that `self.observer` is at the beginning of the
        // struct.
        Box::leak(Box::new(self)) as *mut _ as *mut _
    }

    /// The callback that is registered with the dispatcher that will be called when the dispatcher
    /// is shut down.
    ///
    /// # Safety
    ///
    /// This function should only ever be called by the driver runtime at dispatcher shutdown
    /// time, must only ever be called once for any given [`ShutdownObserver`] object, and
    /// that [`ShutdownObserver`] object must have previously been made into a pointer by
    /// [`Self::into_ptr`].
    unsafe extern "C" fn handler(
        dispatcher: *mut fdf_dispatcher_t,
        observer: *mut fdf_dispatcher_shutdown_observer_t,
    ) {
        // SAFETY: The driver framework promises to only call this function once, so we can
        // safely take ownership of the [`Box`] and deallocate it when this function ends.
        let observer = unsafe { Box::from_raw(observer as *mut ShutdownObserver) };
        // SAFETY: `dispatcher` is the dispatcher being shut down, so it can't be non-null.
        let dispatcher_ref = DispatcherRef(
            ManuallyDrop::new(Dispatcher(unsafe { NonNull::new_unchecked(dispatcher) })),
            PhantomData,
        );
        (observer.shutdown_fn)(dispatcher_ref);
        // SAFETY: we only shutdown the dispatcher when the dispatcher is dropped, and we only ever
        // instantiate one owned copy of `Dispatcher` for a given dispatcher.
        unsafe { fdf_dispatcher_destroy(dispatcher) };
    }
}

pub mod test {
    use core::ffi::{c_char, c_void};
    use core::ptr::null_mut;
    use std::sync::{mpsc, Once};

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
    pub fn with_raw_dispatcher<T>(name: &str, p: impl for<'a> FnOnce(&Arc<Dispatcher>) -> T) -> T {
        with_raw_dispatcher_flags(name, DispatcherBuilder::ALLOW_THREAD_BLOCKING, p)
    }

    pub(crate) fn with_raw_dispatcher_flags<T>(
        name: &str,
        flags: u32,
        p: impl for<'a> FnOnce(&Arc<Dispatcher>) -> T,
    ) -> T {
        ensure_driver_env();

        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let mut dispatcher = null_mut();
        let mut observer = ShutdownObserver::new(move |dispatcher| {
            // SAFETY: we verify that the dispatcher has no tasks left queued in it,
            // just because this is testing code.
            assert!(!unsafe { fdf_env_dispatcher_has_queued_tasks(dispatcher.0 .0.as_ptr()) });
            shutdown_tx.send(()).unwrap();
        })
        .into_ptr();
        let driver_ptr = &mut observer as *mut _ as *mut c_void;
        // SAFETY: The pointers we pass to this function are all stable for the
        // duration of this function, and are not available to copy or clone to
        // client code (only through a ref to the non-`Clone`` `Dispatcher`
        // wrapper).
        let res = unsafe {
            fdf_env_dispatcher_create_with_owner(
                driver_ptr,
                flags,
                name.as_ptr() as *const c_char,
                name.len(),
                "".as_ptr() as *const c_char,
                0 as usize,
                observer,
                &mut dispatcher,
            )
        };
        assert_eq!(res, ZX_OK);
        let dispatcher = Arc::new(Dispatcher(NonNull::new(dispatcher).unwrap()));

        let res = p(&dispatcher);

        // this initiates the dispatcher shutdown on a driver runtime
        // thread. When all tasks on the dispatcher have completed, the wait
        // on the shutdown_rx below will end and we can tear it down.
        let weak_dispatcher = Arc::downgrade(&dispatcher);
        drop(dispatcher);
        shutdown_rx.recv().unwrap();
        assert_eq!(
            0,
            weak_dispatcher.strong_count(),
            "a dispatcher reference escaped the test body"
        );

        res
    }
}

#[cfg(test)]
mod tests {
    use super::test::*;
    use super::*;

    use std::sync::mpsc;

    use futures::channel::mpsc as async_mpsc;
    use futures::{SinkExt, StreamExt};

    #[test]
    fn start_test_dispatcher() {
        with_raw_dispatcher("testing", |dispatcher| {
            println!("hello {dispatcher:?}");
        })
    }

    #[test]
    fn post_task_on_dispatcher() {
        with_raw_dispatcher("testing task", |dispatcher| {
            let (tx, rx) = mpsc::channel();
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
    fn post_task_on_subdispatcher() {
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        with_raw_dispatcher("testing task top level", move |dispatcher| {
            let (tx, rx) = mpsc::channel();
            let (inner_tx, inner_rx) = mpsc::channel();
            dispatcher
                .post_task_sync(move |status| {
                    assert_eq!(status, Status::from_raw(ZX_OK));
                    let inner = DispatcherBuilder::new()
                        .name("testing task second level")
                        .scheduler_role("")
                        .allow_thread_blocking()
                        .shutdown_observer(move |_dispatcher| {
                            println!("shutdown observer called");
                            shutdown_tx.send(1).unwrap();
                        })
                        .create()
                        .unwrap();
                    inner
                        .post_task_sync(move |status| {
                            assert_eq!(status, Status::from_raw(ZX_OK));
                            tx.send(status).unwrap();
                        })
                        .unwrap();
                    // we want to make sure the inner dispatcher lives long
                    // enough to run the task, so we sent it out to the outer
                    // closure.
                    inner_tx.send(inner).unwrap();
                })
                .unwrap();
            assert_eq!(rx.recv().unwrap(), Status::from_raw(ZX_OK));
            inner_rx.recv().unwrap();
        });
        assert_eq!(shutdown_rx.recv().unwrap(), 1);
    }

    async fn ping(mut tx: async_mpsc::Sender<u8>, mut rx: async_mpsc::Receiver<u8>) {
        println!("starting ping!");
        tx.send(0).await.unwrap();
        while let Some(next) = rx.next().await {
            println!("ping! {next}");
            tx.send(next + 1).await.unwrap();
        }
    }

    async fn pong(
        fin_tx: std::sync::mpsc::Sender<()>,
        mut tx: async_mpsc::Sender<u8>,
        mut rx: async_mpsc::Receiver<u8>,
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
        with_raw_dispatcher("async ping pong", |dispatcher| {
            let (fin_tx, fin_rx) = mpsc::channel();
            let (ping_tx, pong_rx) = async_mpsc::channel(10);
            let (pong_tx, ping_rx) = async_mpsc::channel(10);
            dispatcher.spawn_task(ping(ping_tx, ping_rx)).unwrap();
            dispatcher.spawn_task(pong(fin_tx, pong_tx, pong_rx)).unwrap();

            fin_rx.recv().expect("to receive final value");
        });
    }

    async fn slow_pong(
        fin_tx: std::sync::mpsc::Sender<()>,
        mut tx: async_mpsc::Sender<u8>,
        mut rx: async_mpsc::Receiver<u8>,
    ) {
        use zx::MonotonicDuration;
        println!("starting pong!");
        while let Some(next) = rx.next().await {
            println!("pong! {next}");
            fuchsia_async::Timer::new(fuchsia_async::MonotonicInstant::after(
                MonotonicDuration::from_seconds(1),
            ))
            .await;
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
        with_raw_dispatcher("async ping pong", |dispatcher| {
            let (fin_tx, fin_rx) = mpsc::channel();
            let (ping_tx, pong_rx) = async_mpsc::channel(10);
            let (pong_tx, ping_rx) = async_mpsc::channel(10);

            // spawn ping on the driver dispatcher
            dispatcher.spawn_task(ping(ping_tx, ping_rx)).unwrap();

            // and run pong on the fuchsia_async executor
            let mut executor = fuchsia_async::LocalExecutor::new();
            executor.run_singlethreaded(slow_pong(fin_tx, pong_tx, pong_rx));

            fin_rx.recv().expect("to receive final value");
        });
    }
}
