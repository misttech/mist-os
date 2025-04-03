// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Internal helpers for implementing futures against channel objects

use std::mem::ManuallyDrop;
use std::sync::atomic::AtomicBool;
use std::task::Waker;
use zx::Status;

use crate::channel::try_read_raw;
use crate::message::Message;
use fdf_core::dispatcher::OnDispatcher;
use fdf_core::handle::DriverHandle;
use fdf_sys::*;

use core::mem::MaybeUninit;
use core::task::{Context, Poll};
use std::sync::{Arc, Mutex};

pub use fdf_sys::fdf_handle_t;

/// This struct is shared between the future and the driver runtime, with the first field
/// being managed by the driver runtime and the second by the future. It will be held by two
/// [`Arc`]s, one for each of the future and the runtime.
///
/// The future's [`Arc`] will be dropped when the future is either fulfilled or cancelled through
/// normal [`Drop`] of the future.
///
/// The runtime's [`Arc`]'s dropping varies depending on whether the dispatcher it was registered on
/// was synchronized or not, and whether it was cancelled or not. The callback will only ever be
/// called *up to* one time.
///
/// If the dispatcher is synchronized, then the callback will *only* be called on fulfillment of the
/// read wait.
#[repr(C)]
struct ReadMessageStateOp {
    /// This must be at the start of the struct so that `ReadMessageStateOp` can be cast to and from `fdf_channel_read`.
    read_op: fdf_channel_read,
    waker: Mutex<Option<Waker>>,
    cancelled: AtomicBool,
}

impl ReadMessageStateOp {
    unsafe extern "C" fn handler(
        _dispatcher: *mut fdf_dispatcher,
        read_op: *mut fdf_channel_read,
        status: i32,
    ) {
        // SAFETY: When setting up the read op, we incremented the refcount of the `Arc` to allow
        // for this handler to reconstitute it.
        let op: Arc<Self> = unsafe { Arc::from_raw(read_op.cast()) };
        if Status::from_raw(status) == Status::CANCELED {
            op.cancelled.store(true, std::sync::atomic::Ordering::Release);
        }

        let Some(waker) = op.waker.lock().unwrap().take() else {
            // the waker was already taken, presumably because the future was dropped.
            return;
        };
        waker.wake()
    }
}

/// An object for managing the state of an async channel read message operation that can be used to
/// implement futures.
pub struct ReadMessageState {
    op: Arc<ReadMessageStateOp>,
    channel: ManuallyDrop<DriverHandle>,
    callback_drops_arc: bool,
}

impl ReadMessageState {
    /// Creates a new raw read message state that can be used to implement a [`Future`] that reads
    /// data from a channel and then converts it to the appropriate type. It also allows for
    /// different ways of storing and managing the dispatcher we wait on by deferring the
    /// dispatcher used to poll time.
    ///
    /// # Safety
    ///
    /// The caller is responsible for ensuring that `channel` outlives this object.
    pub unsafe fn new(channel: &DriverHandle) -> Self {
        // SAFETY: The caller is responsible for ensuring that the handle is a correct channel handle
        // and that the handle will outlive the created [`ReadMessageState`].
        let channel = unsafe { channel.get_raw() };
        Self {
            op: Arc::new(ReadMessageStateOp {
                read_op: fdf_channel_read {
                    channel: channel.get(),
                    handler: Some(ReadMessageStateOp::handler),
                    ..Default::default()
                },
                waker: Mutex::new(None),
                cancelled: AtomicBool::new(false),
            }),
            // SAFETY: We know this is a valid driver handle by construction and we are
            // storing this handle in a [`ManuallyDrop`] to prevent it from being double-dropped.
            // The caller is responsible for ensuring that the handle outlives this object.
            channel: ManuallyDrop::new(unsafe { DriverHandle::new_unchecked(channel) }),
            // We haven't waited on it yet so we are responsible for dropping the arc for now,
            // regardless of what kind of dispatcher it's intended to be used with.
            callback_drops_arc: false,
        }
    }

    /// Polls this channel read operation against the given dispatcher.
    pub fn poll_with_dispatcher<D: OnDispatcher>(
        self: &mut Self,
        cx: &mut Context<'_>,
        dispatcher: D,
    ) -> Poll<Result<Option<Message<[MaybeUninit<u8>]>>, Status>> {
        let mut waker_lock = self.op.waker.lock().unwrap();

        if self.op.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            // if the dispatcher we were waiting on is shutting down then when we try to go
            // to wait again we'll get ZX_ERR_UNAVAILABLE anyways, so just short circuit that and
            // return it right away.
            return Poll::Ready(Err(Status::UNAVAILABLE));
        }

        match try_read_raw(&self.channel) {
            Ok(res) => Poll::Ready(Ok(res)),
            Err(Status::SHOULD_WAIT) => {
                // if we haven't yet set a waker, that means we haven't started the wait operation
                // yet.
                if waker_lock.replace(cx.waker().clone()).is_none() {
                    // increment the reference count of the read op to account for the copy that will be given to
                    // `fdf_channel_wait_async`.
                    let op = Arc::into_raw(self.op.clone());
                    let res = dispatcher.on_maybe_dispatcher(|dispatcher| {
                        // SAFETY: the `ReadMessageStateOp` starts with an `fdf_channel_read` struct and
                        // has `repr(C)` layout, so is safe to be cast to the latter.
                        let res = Status::ok(unsafe {
                            fdf_channel_wait_async(
                                dispatcher.inner().as_ptr(),
                                op.cast_mut().cast(),
                                0,
                            )
                        });
                        // if the dispatcher we're waiting on is unsynchronized, the callback
                        // will drop the Arc and we need to indicate to our own Drop impl
                        // that it should not.
                        let callback_drops_arc = res.is_ok() && dispatcher.is_unsynchronized();
                        Ok(callback_drops_arc)
                    });

                    match res {
                        Ok(callback_drops_arc) => {
                            self.callback_drops_arc = callback_drops_arc;
                        }
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Drop for ReadMessageState {
    fn drop(&mut self) {
        let mut waker_lock = self.op.waker.lock().unwrap();
        if waker_lock.is_none() {
            // if there's no waker either the callback has already fired or we never waited on this
            // future in the first place, so just leave it be.
            return;
        }

        // SAFETY: since we hold a lifetimed-reference to the channel object here, the channel must
        // be valid.
        let res = Status::ok(unsafe { fdf_channel_cancel_wait(self.channel.get_raw().get()) });
        match res {
            Ok(_) => {}
            Err(Status::NOT_FOUND) => {
                // the callback is already being called or the wait was already cancelled, so just
                // return and leave it.
                return;
            }
            Err(e) => panic!("Unexpected error {e:?} cancelling driver channel read wait"),
        }
        // steal the waker so it doesn't get called, if there is one.
        waker_lock.take();
        // SAFETY: if the channel was waited on by a synchronized dispatcher, and the cancel was
        // successful, the callback will not be called and we will have to free the `Arc` that the
        // callback would have consumed.
        if !self.callback_drops_arc {
            unsafe { Arc::decrement_strong_count(Arc::as_ptr(&self.op)) };
        }
    }
}

#[cfg(test)]
mod test {
    use std::pin::pin;
    use std::sync::Weak;

    use fdf_core::dispatcher::{CurrentDispatcher, OnDispatcher};
    use fdf_env::test::{spawn_in_driver, spawn_in_driver_etc};

    use crate::arena::Arena;
    use crate::channel::{read_raw, Channel};

    use super::*;

    /// assert that the strong count of an arc is correct
    fn assert_strong_count<T>(arc: &Weak<T>, count: usize) {
        assert_eq!(Weak::strong_count(arc), count, "unexpected strong count on arc");
    }

    /// create, poll, and then immediately drop a read future for a channel and verify
    /// that the internal op arc has the right refcount at all steps. Returns a copy
    /// of the op arc at the end so it can be verified that the count goes down
    /// to zero correctly.
    async fn read_and_drop<T: ?Sized + 'static, D: OnDispatcher>(
        channel: &Channel<T>,
        dispatcher: D,
    ) -> Weak<ReadMessageStateOp> {
        let fut = read_raw(&channel.0, dispatcher);
        let op_arc = Arc::downgrade(&fut.raw_fut.op);
        assert_strong_count(&op_arc, 1);
        let mut fut = pin!(fut);
        let Poll::Pending = futures::poll!(fut.as_mut()) else {
            panic!("expected pending state after polling channel read once");
        };
        assert_strong_count(&op_arc, 2);
        op_arc
    }

    #[test]
    fn early_cancel_future() {
        spawn_in_driver("early cancellation", async {
            let (a, b) = Channel::create();

            // create, poll, and then immediately drop a read future for channel `a`
            // so that it properly sets up the wait.
            read_and_drop(&a, CurrentDispatcher).await;
            b.write_with_data(Arena::new(), |arena| arena.insert(1)).unwrap();
            assert_eq!(a.read(CurrentDispatcher).await.unwrap().unwrap().data(), Some(&1));
        })
    }

    #[test]
    fn very_early_cancel_state_drops_correctly() {
        spawn_in_driver("early cancellation drop correctness", async {
            let (a, _b) = Channel::<[u8]>::create();

            // drop before even polling it should drop the arc correctly
            let fut = read_raw(&a.0, CurrentDispatcher);
            let op_arc = Arc::downgrade(&fut.raw_fut.op);
            assert_strong_count(&op_arc, 1);
            drop(fut);
            assert_strong_count(&op_arc, 0);
        })
    }

    #[test]
    fn synchronized_early_cancel_state_drops_correctly() {
        spawn_in_driver("early cancellation drop correctness", async {
            let (a, _b) = Channel::<[u8]>::create();

            assert_strong_count(&read_and_drop(&a, CurrentDispatcher).await, 0);
        });
    }

    #[test]
    fn unsynchronized_early_cancel_state_drops_correctly() {
        // the channel needs to outlive the dispatcher for this test because the channel shouldn't
        // be closed before the read wait has been cancelled.
        let (a, _b) = Channel::<[u8]>::create();
        let (unsync_op, _a) =
            spawn_in_driver_etc("early cancellation drop correctness", false, true, async move {
                // We send the arc out to be checked after the dispatcher has shut down so
                // that we can be sure that the callback has had a chance to be called.
                // We send the channel back out so that it lives long enough for the
                // cancellation to be called on it.
                let res = read_and_drop(&a, CurrentDispatcher).await;
                (res, a)
            });

        // check that there are no more owners of the inner op for the unsynchronized dispatcher.
        assert_strong_count(&unsync_op, 0);
    }
}
