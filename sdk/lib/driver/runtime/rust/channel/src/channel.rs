// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Safe bindings for the driver runtime channel stable ABI

use core::future::Future;
use std::mem::ManuallyDrop;
use zx::Status;

use crate::arena::{Arena, ArenaBox};
use crate::message::Message;
use fdf_core::dispatcher::OnDispatcher;
use fdf_core::handle::{DriverHandle, MixedHandle};
use fdf_sys::*;

use core::marker::PhantomData;
use core::mem::{size_of_val, MaybeUninit};
use core::num::NonZero;
use core::pin::Pin;
use core::ptr::{null_mut, NonNull};
use core::task::{Context, Poll, Waker};
use std::sync::{Arc, Mutex};

pub use fdf_sys::fdf_handle_t;

/// Implements a message channel through the Fuchsia Driver Runtime
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Channel<T: ?Sized + 'static>(pub(crate) DriverHandle, PhantomData<Message<T>>);

impl<T: ?Sized + 'static> Channel<T> {
    /// Creates a new channel pair that can be used to send messages of type `T`
    /// between threads managed by the driver runtime.
    pub fn create() -> (Self, Self) {
        let mut channel1 = 0;
        let mut channel2 = 0;
        // This call cannot fail as the only reason it would fail is due to invalid
        // option flags, and 0 is a valid option.
        Status::ok(unsafe { fdf_channel_create(0, &mut channel1, &mut channel2) })
            .expect("failed to create channel pair");
        // SAFETY: if fdf_channel_create returned ZX_OK, it will have placed
        // valid channel handles that must be non-zero.
        unsafe {
            (
                Self::from_handle_unchecked(NonZero::new_unchecked(channel1)),
                Self::from_handle_unchecked(NonZero::new_unchecked(channel2)),
            )
        }
    }

    /// Takes the inner handle to the channel. The caller is responsible for ensuring
    /// that the handle is freed.
    pub fn into_driver_handle(self) -> DriverHandle {
        self.0
    }

    /// Initializes a [`Channel`] object from the given non-zero handle.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the handle is not invalid and that it is
    /// part of a driver runtime channel pair of type `T`.
    unsafe fn from_handle_unchecked(handle: NonZero<fdf_handle_t>) -> Self {
        // SAFETY: caller is responsible for ensuring that it is a valid channel
        Self(unsafe { DriverHandle::new_unchecked(handle) }, PhantomData)
    }

    /// Initializes a [`Channel`] object from the given [`DriverHandle`],
    /// assuming that it is a channel of type `T`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the handle is a [`Channel`]-based handle that is
    /// using type `T` as its wire format.
    pub unsafe fn from_driver_handle(handle: DriverHandle) -> Self {
        Self(handle, PhantomData)
    }

    /// Writes the [`Message`] given to the channel. This will complete asynchronously and can't
    /// be cancelled.
    ///
    /// The channel will take ownership of the data and handles passed in,
    pub fn write(&self, message: Message<T>) -> Result<(), Status> {
        // get the sizes while the we still have refs to the data and handles
        let data_len = message.data().map_or(0, |data| size_of_val(&*data) as u32);
        let handles_count = message.handles().map_or(0, |handles| handles.len() as u32);

        let (arena, data, handles) = message.into_raw();

        // transform the `Option<NonNull<T>>` into just `*mut T`
        let data_ptr = data.map_or(null_mut(), |data| data.cast().as_ptr());
        let handles_ptr = handles.map_or(null_mut(), |handles| handles.cast().as_ptr());

        // SAFETY:
        // - Normally, we could be reading uninit bytes here. However, as long as fdf_channel_write
        //   doesn't allow cross-LTO then it won't care whether the bytes are initialized.
        // - The `Message` will generally only construct correctly if the data and handles pointers
        //   inside it are from the arena it holds, but just in case `fdf_channel_write` will check
        //   that we are using the correct arena so we do not need to re-verify that they are from
        //   the same arena.
        Status::ok(unsafe {
            fdf_channel_write(
                self.0.get_raw().get(),
                0,
                arena.as_ptr(),
                data_ptr,
                data_len,
                handles_ptr,
                handles_count,
            )
        })?;

        // SAFETY: this is the valid-by-contruction arena we were passed in through the [`Message`]
        // object, and now that we have completed `fdf_channel_write` it is safe to drop our copy
        // of it.
        unsafe { fdf_arena_drop_ref(arena.as_ptr()) };
        Ok(())
    }

    /// Shorthand for calling [`Self::write`] with the result of [`Message::new_with`]
    pub fn write_with<F>(&self, arena: Arena, f: F) -> Result<(), Status>
    where
        F: for<'a> FnOnce(
            &'a Arena,
        )
            -> (Option<ArenaBox<'a, T>>, Option<ArenaBox<'a, [Option<MixedHandle>]>>),
    {
        self.write(Message::new_with(arena, f))
    }

    /// Shorthand for calling [`Self::write`] with the result of [`Message::new_with`]
    pub fn write_with_data<F>(&self, arena: Arena, f: F) -> Result<(), Status>
    where
        F: for<'a> FnOnce(&'a Arena) -> ArenaBox<'a, T>,
    {
        self.write(Message::new_with_data(arena, f))
    }
}

/// Attempts to read from the channel, returning a [`Message`] object that can be used to
/// access or take the data received if there was any. This is the basic building block
/// on which the other `try_read_*` methods are built.
fn try_read_raw(channel: &DriverHandle) -> Result<Option<Message<[MaybeUninit<u8>]>>, Status> {
    let mut out_arena = null_mut();
    let mut out_data = null_mut();
    let mut out_num_bytes = 0;
    let mut out_handles = null_mut();
    let mut out_num_handles = 0;
    Status::ok(unsafe {
        fdf_channel_read(
            channel.get_raw().get(),
            0,
            &mut out_arena,
            &mut out_data,
            &mut out_num_bytes,
            &mut out_handles,
            &mut out_num_handles,
        )
    })?;
    // if no arena was returned, that means no data was returned.
    if out_arena == null_mut() {
        return Ok(None);
    }
    // SAFETY: we just checked that the `out_arena` is non-null
    let arena = Arena(unsafe { NonNull::new_unchecked(out_arena) });
    let data_ptr = if !out_data.is_null() {
        let ptr = core::ptr::slice_from_raw_parts_mut(out_data.cast(), out_num_bytes as usize);
        // SAFETY: we just checked that the pointer was non-null, the slice version of it should
        // be too.
        Some(unsafe { ArenaBox::new(NonNull::new_unchecked(ptr)) })
    } else {
        None
    };
    let handles_ptr = if !out_handles.is_null() {
        let ptr = core::ptr::slice_from_raw_parts_mut(out_handles.cast(), out_num_handles as usize);
        // SAFETY: we just checked that the pointer was non-null, the slice version of it should
        // be too.
        Some(unsafe { ArenaBox::new(NonNull::new_unchecked(ptr)) })
    } else {
        None
    };
    Ok(Some(unsafe { Message::new_unchecked(arena, data_ptr, handles_ptr) }))
}

/// Reads a message from the channel asynchronously
///
/// # Panic
///
/// Panics if this is not run from a driver framework dispatcher.
fn read_raw<'a, D>(channel: &'a DriverHandle, dispatcher: D) -> ReadMessageRawFut<D> {
    // SAFETY: Since the future's lifetime is bound to the original driver handle and it
    // holds the message state, the message state object can't outlive the handle.
    ReadMessageRawFut { raw_fut: unsafe { ReadMessageState::new(channel) }, dispatcher }
}

impl<T> Channel<T> {
    /// Attempts to read an object of type `T` and a handle set from the channel
    pub fn try_read<'a>(&self) -> Result<Option<Message<T>>, Status> {
        // read a message from the channel
        let Some(message) = try_read_raw(&self.0)? else {
            return Ok(None);
        };
        // SAFETY: It is an invariant of Channel<T> that messages sent or received are always of
        // type T.
        Ok(Some(unsafe { message.cast_unchecked() }))
    }

    /// Reads an object of type `T` and a handle set from the channel asynchronously
    pub async fn read<D: OnDispatcher>(&self, dispatcher: D) -> Result<Option<Message<T>>, Status> {
        let Some(message) = read_raw(&self.0, dispatcher).await? else {
            return Ok(None);
        };
        // SAFETY: It is an invariant of Channel<T> that messages sent or received are always of
        // type T.
        Ok(Some(unsafe { message.cast_unchecked() }))
    }
}

impl Channel<[u8]> {
    /// Attempts to read an object of type `T` and a handle set from the channel
    pub fn try_read_bytes<'a>(&self) -> Result<Option<Message<[u8]>>, Status> {
        // read a message from the channel
        let Some(message) = try_read_raw(&self.0)? else {
            return Ok(None);
        };
        // SAFETY: It is an invariant of Channel<[u8]> that messages sent or received are always of
        // type [u8].
        Ok(Some(unsafe { message.assume_init() }))
    }

    /// Reads a slice of type `T` and a handle set from the channel asynchronously
    pub async fn read_bytes<D: OnDispatcher>(
        &self,
        dispatcher: D,
    ) -> Result<Option<Message<[u8]>>, Status> {
        // read a message from the channel
        let Some(message) = read_raw(&self.0, dispatcher).await? else {
            return Ok(None);
        };
        // SAFETY: It is an invariant of Channel<[u8]> that messages sent or received are always of
        // type [u8].
        Ok(Some(unsafe { message.assume_init() }))
    }
}

impl<T> From<Channel<T>> for MixedHandle {
    fn from(value: Channel<T>) -> Self {
        MixedHandle::from(value.0)
    }
}

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
}

impl ReadMessageStateOp {
    unsafe extern "C" fn handler(
        _dispatcher: *mut fdf_dispatcher,
        read_op: *mut fdf_channel_read,
        _status: i32,
    ) {
        // SAFETY: When setting up the read op, we incremented the refcount of the `Arc` to allow
        // for this handler to reconstitute it.
        let op: Arc<Self> = unsafe { Arc::from_raw(read_op.cast()) };
        let Some(waker) = op.waker.lock().unwrap().take() else {
            // the waker was already taken, presumably because the future was dropped.
            return;
        };
        waker.wake()
    }
}

/// An object for managing the state of an async channel read message operation that can be used to
/// implement futures.
pub(crate) struct ReadMessageState {
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
    pub(crate) unsafe fn new(channel: &DriverHandle) -> Self {
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
    pub(crate) fn poll_with_dispatcher<D: OnDispatcher>(
        self: &mut Self,
        cx: &mut Context<'_>,
        dispatcher: D,
    ) -> Poll<Result<Option<Message<[MaybeUninit<u8>]>>, Status>> {
        let mut waker_lock = self.op.waker.lock().unwrap();

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

struct ReadMessageRawFut<D> {
    raw_fut: ReadMessageState,
    dispatcher: D,
}

impl<D: OnDispatcher> Future for ReadMessageRawFut<D> {
    type Output = Result<Option<Message<[MaybeUninit<u8>]>>, Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let dispatcher = self.dispatcher.clone();
        self.as_mut().raw_fut.poll_with_dispatcher(cx, dispatcher)
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::sync::{mpsc, Weak};

    use fdf_core::dispatcher::{CurrentDispatcher, Dispatcher, DispatcherBuilder, OnDispatcher};
    use fdf_core::handle::MixedHandleType;
    use fdf_env::test::{spawn_in_driver, spawn_in_driver_etc};
    use futures::poll;

    use super::*;
    use crate::test_utils::*;

    #[test]
    fn send_and_receive_bytes_synchronously() {
        let (first, second) = Channel::create();
        let arena = Arena::new();
        assert_eq!(first.try_read_bytes().unwrap_err(), Status::from_raw(ZX_ERR_SHOULD_WAIT));
        first.write_with_data(arena.clone(), |arena| arena.insert_slice(&[1, 2, 3, 4])).unwrap();
        assert_eq!(&*second.try_read_bytes().unwrap().unwrap().data().unwrap(), &[1, 2, 3, 4]);
        assert_eq!(second.try_read_bytes().unwrap_err(), Status::from_raw(ZX_ERR_SHOULD_WAIT));
        second.write_with_data(arena.clone(), |arena| arena.insert_slice(&[5, 6, 7, 8])).unwrap();
        assert_eq!(&*first.try_read_bytes().unwrap().unwrap().data().unwrap(), &[5, 6, 7, 8]);
        assert_eq!(first.try_read_bytes().unwrap_err(), Status::from_raw(ZX_ERR_SHOULD_WAIT));
        assert_eq!(second.try_read_bytes().unwrap_err(), Status::from_raw(ZX_ERR_SHOULD_WAIT));
        drop(second);
        assert_eq!(
            first.write_with_data(arena.clone(), |arena| arena.insert_slice(&[9, 10, 11, 12])),
            Err(Status::from_raw(ZX_ERR_PEER_CLOSED))
        );
    }

    #[test]
    fn send_and_receive_bytes_asynchronously() {
        spawn_in_driver("channel async", async {
            let arena = Arena::new();
            let (first, second) = Channel::create();

            assert!(poll!(pin!(first.read_bytes(CurrentDispatcher))).is_pending());
            second.write_with_data(arena, |arena| arena.insert_slice(&[1, 2, 3, 4])).unwrap();
            assert_eq!(
                first.read_bytes(CurrentDispatcher).await.unwrap().unwrap().data().unwrap(),
                &[1, 2, 3, 4]
            );
        });
    }

    #[test]
    fn send_and_receive_objects_synchronously() {
        let arena = Arena::new();
        let (first, second) = Channel::create();
        let (tx, rx) = mpsc::channel();
        first
            .write_with_data(arena.clone(), |arena| arena.insert(DropSender::new(1, tx.clone())))
            .unwrap();
        rx.try_recv().expect_err("should not drop the object when sent");
        let message = second.try_read().unwrap().unwrap();
        assert_eq!(message.data().unwrap().0, 1);
        rx.try_recv().expect_err("should not drop the object when received");
        drop(message);
        rx.try_recv().expect("dropped when received");
    }

    #[test]
    fn send_and_receive_handles_synchronously() {
        println!("Create channels and write one end of one of the channel pairs to the other");
        let (first, second) = Channel::<()>::create();
        let (inner_first, inner_second) = Channel::<String>::create();
        let message = Message::new_with(Arena::new(), |arena| {
            (None, Some(arena.insert_boxed_slice(Box::new([Some(inner_first.into())]))))
        });
        first.write(message).unwrap();

        println!("Receive the channel back on the other end of the first channel pair.");
        let mut arena = None;
        let message =
            second.try_read().unwrap().expect("Expected a message with contents to be received");
        let (_, received_handles) = message.into_arena_boxes(&mut arena);
        let mut first_handle_received =
            ArenaBox::take_boxed_slice(received_handles.expect("expected handles in the message"));
        let first_handle_received = first_handle_received
            .first_mut()
            .expect("expected one handle in the handle set")
            .take()
            .expect("expected the first handle to be non-null");
        let first_handle_received = first_handle_received.resolve();
        let MixedHandleType::Driver(driver_handle) = first_handle_received else {
            panic!("Got a non-driver handle when we sent a driver handle");
        };
        let inner_first_received = unsafe { Channel::from_driver_handle(driver_handle) };

        println!("Send and receive a string across the now-transmitted channel pair.");
        inner_first_received
            .write_with_data(Arena::new(), |arena| arena.insert("boom".to_string()))
            .unwrap();
        assert_eq!(inner_second.try_read().unwrap().unwrap().data().unwrap(), &"boom".to_string());
    }

    async fn ping(chan: Channel<u8>) {
        println!("starting ping!");
        chan.write_with_data(Arena::new(), |arena| arena.insert(0)).unwrap();
        while let Ok(Some(msg)) = chan.read(CurrentDispatcher).await {
            let next = *msg.data().unwrap();
            println!("ping! {next}");
            chan.write_with_data(msg.take_arena(), |arena| arena.insert(next + 1)).unwrap();
        }
    }

    async fn pong(chan: Channel<u8>) {
        println!("starting pong!");
        while let Some(msg) = chan.read(CurrentDispatcher).await.unwrap() {
            let next = *msg.data().unwrap();
            println!("pong! {next}");
            if next > 10 {
                println!("bye!");
                break;
            }
            chan.write_with_data(msg.take_arena(), |arena| arena.insert(next + 1)).unwrap();
        }
    }

    #[test]
    fn async_ping_pong() {
        spawn_in_driver("async ping pong", async {
            let (ping_chan, pong_chan) = Channel::create();
            CurrentDispatcher.spawn_task(ping(ping_chan)).unwrap();
            pong(pong_chan).await;
        });
    }

    #[test]
    fn async_ping_pong_on_fuchsia_async() {
        spawn_in_driver("async ping pong", async {
            let (ping_chan, pong_chan) = Channel::create();

            let fdf_dispatcher = DispatcherBuilder::new()
                .name("fdf-async")
                .create()
                .expect("failure creating non-blocking dispatcher for fdf operations on rust-async dispatcher")
                .release();

            let rust_async_dispatcher = DispatcherBuilder::new()
                .name("fuchsia-async")
                .allow_thread_blocking()
                .create()
                .expect("failure creating blocking dispatcher for rust async")
                .release();

            rust_async_dispatcher
                .post_task_sync(move |_| {
                    Dispatcher::override_current(fdf_dispatcher, || {
                        let mut executor = fuchsia_async::LocalExecutor::new();
                        executor.run_singlethreaded(ping(ping_chan));
                    });
                })
                .unwrap();

            pong(pong_chan).await
        });
    }

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
