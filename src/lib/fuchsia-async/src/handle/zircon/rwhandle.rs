// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::runtime::{EHandle, PacketReceiver, ReceiverRegistration};
use crate::OnSignalsRef;
use std::sync::{Arc, Mutex};
use std::task::{ready, Context, Poll, Waker};
use zx::{self as zx, AsHandleRef};

const OBJECT_PEER_CLOSED: zx::Signals = zx::Signals::OBJECT_PEER_CLOSED;
const OBJECT_READABLE: zx::Signals = zx::Signals::OBJECT_READABLE;
const OBJECT_WRITABLE: zx::Signals = zx::Signals::OBJECT_WRITABLE;

/// State of an object when it is ready for reading.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ReadableState {
    /// Received `OBJECT_READABLE`, or optimistically assuming the object is readable.
    Readable,
    /// Received `OBJECT_PEER_CLOSED`.  The object might also be readable.
    MaybeReadableAndClosed,
}

/// State of an object when it is ready for writing.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum WritableState {
    /// Received `OBJECT_WRITABLE`, or optimistically assuming the object is writable.
    Writable,
    /// Received `OBJECT_PEER_CLOSED`.
    Closed,
}

/// A `Handle` that receives notifications when it is readable.
///
/// # Examples
///
/// ```
/// loop {
///     ready!(self.poll_readable(cx))?;
///     match /* make read syscall */ {
///         Err(zx::Status::SHOULD_WAIT) => ready!(self.need_readable(cx)?),
///         status => return Poll::Ready(status),
///     }
/// }
/// ```
pub trait ReadableHandle {
    /// If the object is ready for reading, returns `Ready` with the readable
    /// state. If the implementor returns Pending, it should first ensure that
    /// `need_readable` is called.
    ///
    /// This should be called in a poll function. If the syscall returns
    /// `SHOULD_WAIT`, you must call `need_readable` to schedule wakeup when the
    /// object is readable.
    ///
    /// The returned `ReadableState` does not necessarily reflect an observed
    /// `OBJECT_READABLE` signal. We optimistically assume the object remains
    /// readable until `need_readable` is called.
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<Result<ReadableState, zx::Status>>;

    /// Arranges for the current task to be woken when the object receives an
    /// `OBJECT_READABLE` or `OBJECT_PEER_CLOSED` signal.  This can return
    /// Poll::Ready if the object has already been signaled in which case the
    /// waker *will* not be woken and it is the caller's responsibility to not
    /// lose the signal.
    fn need_readable(&self, cx: &mut Context<'_>) -> Poll<Result<(), zx::Status>>;
}

/// A `Handle` that receives notifications when it is writable.
///
/// # Examples
///
/// ```
/// loop {
///     ready!(self.poll_writable(cx))?;
///     match /* make write syscall */ {
///         Err(zx::Status::SHOULD_WAIT) => ready!(self.need_writable(cx)?),
///         status => Poll::Ready(status),
///     }
/// }
/// ```
pub trait WritableHandle {
    /// If the object is ready for writing, returns `Ready` with the writable
    /// state. If the implementor returns Pending, it should first ensure that
    /// `need_writable` is called.
    ///
    /// This should be called in a poll function. If the syscall returns
    /// `SHOULD_WAIT`, you must call `need_writable` to schedule wakeup when the
    /// object is writable.
    ///
    /// The returned `WritableState` does not necessarily reflect an observed
    /// `OBJECT_WRITABLE` signal. We optimistically assume the object remains
    /// writable until `need_writable` is called.
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<Result<WritableState, zx::Status>>;

    /// Arranges for the current task to be woken when the object receives an
    /// `OBJECT_WRITABLE` or `OBJECT_PEER_CLOSED` signal. This can return
    /// Poll::Ready if the object has already been signaled in which case the
    /// waker *will* not be woken and it is the caller's responsibility to not
    /// lose the signal.
    fn need_writable(&self, cx: &mut Context<'_>) -> Poll<Result<(), zx::Status>>;
}

struct RWPacketReceiver(Mutex<Inner>);

struct Inner {
    signals: zx::Signals,
    read_task: Option<Waker>,
    write_task: Option<Waker>,
}

impl PacketReceiver for RWPacketReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        let new = if let zx::PacketContents::SignalOne(p) = packet.contents() {
            p.observed()
        } else {
            return;
        };

        // We wake the tasks when the lock isn't held in case the wakers need the same lock.
        let mut read_task = None;
        let mut write_task = None;
        {
            let mut inner = self.0.lock().unwrap();
            let old = inner.signals;
            inner.signals |= new;

            let became_readable = new.contains(OBJECT_READABLE) && !old.contains(OBJECT_READABLE);
            let became_writable = new.contains(OBJECT_WRITABLE) && !old.contains(OBJECT_WRITABLE);
            let became_closed =
                new.contains(OBJECT_PEER_CLOSED) && !old.contains(OBJECT_PEER_CLOSED);

            if became_readable || became_closed {
                read_task = inner.read_task.take();
            }
            if became_writable || became_closed {
                write_task = inner.write_task.take();
            }
        }
        // *NOTE*: This is the only safe place to wake wakers.  In any other location, there is a
        // risk that locks are held which might be required when the waker is woken.  It is safe to
        // wake here because this is called from the executor when no locks are held.
        if let Some(read_task) = read_task {
            read_task.wake();
        }
        if let Some(write_task) = write_task {
            write_task.wake();
        }
    }
}

/// A `Handle` that receives notifications when it is readable/writable.
pub struct RWHandle<T> {
    handle: T,
    receiver: ReceiverRegistration<RWPacketReceiver>,
}

impl<T> RWHandle<T>
where
    T: AsHandleRef,
{
    /// Creates a new `RWHandle` object which will receive notifications when
    /// the underlying handle becomes readable, writable, or closes.
    ///
    /// # Panics
    ///
    /// If called outside the context of an active async executor.
    pub fn new(handle: T) -> Self {
        let ehandle = EHandle::local();

        let initial_signals = OBJECT_READABLE | OBJECT_WRITABLE;
        let receiver = ehandle.register_receiver(Arc::new(RWPacketReceiver(Mutex::new(Inner {
            // Optimistically assume that the handle is readable and writable.
            // Reads and writes will be attempted before queueing a packet.
            // This makes handles slightly faster to read/write the first time
            // they're accessed after being created, provided they start off as
            // readable or writable. In return, there will be an extra wasted
            // syscall per read/write if the handle is not readable or writable.
            signals: initial_signals,
            read_task: None,
            write_task: None,
        }))));

        RWHandle { handle, receiver }
    }

    /// Returns a reference to the underlying handle.
    pub fn get_ref(&self) -> &T {
        &self.handle
    }

    /// Returns a mutable reference to the underlying handle.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.handle
    }

    /// Consumes `self` and returns the underlying handle.
    pub fn into_inner(self) -> T {
        self.handle
    }

    /// Returns true if the object received the `OBJECT_PEER_CLOSED` signal.
    pub fn is_closed(&self) -> bool {
        let signals = self.receiver().0.lock().unwrap().signals;
        if signals.contains(OBJECT_PEER_CLOSED) {
            return true;
        }

        // The signals bitset might not be updated if we haven't gotten around to processing the
        // packet telling us that yet. To provide an up-to-date response, we query the current
        // state of the signal.
        //
        // Note: we _could_ update the bitset with what we find here, if we're careful to also
        // update READABLE + WRITEABLE at the same time, and also wakeup the tasks as necessary.
        // But having `is_closed` wakeup tasks if it discovered a signal change seems too weird, so
        // we just leave the bitset as-is and let the regular notification mechanism get around to
        // it when it gets around to it.
        match self
            .handle
            .wait_handle(OBJECT_PEER_CLOSED, zx::MonotonicInstant::INFINITE_PAST)
            .to_result()
        {
            Ok(_) => true,
            Err(zx::Status::TIMED_OUT) => false,
            Err(status) => {
                // None of the other documented error statuses should be possible, either the type
                // system doesn't allow it or the wait from `RWHandle::new()` would have already
                // failed.
                unreachable!("status: {status}")
            }
        }
    }

    /// Returns a future that completes when `is_closed()` is true.
    pub fn on_closed(&self) -> OnSignalsRef<'_> {
        OnSignalsRef::new(self.handle.as_handle_ref(), OBJECT_PEER_CLOSED)
    }

    fn receiver(&self) -> &RWPacketReceiver {
        self.receiver.receiver()
    }

    fn need_signal(
        &self,
        cx: &mut Context<'_>,
        for_read: bool,
        signal: zx::Signals,
    ) -> Poll<Result<(), zx::Status>> {
        let mut inner = self.receiver.0.lock().unwrap();
        let old = inner.signals;
        if old.contains(zx::Signals::OBJECT_PEER_CLOSED) {
            // We don't want to return an error here because even though the peer has closed, the
            // object could still have queued messages that can be read.
            Poll::Ready(Ok(()))
        } else {
            let waker = cx.waker().clone();
            if for_read {
                inner.read_task = Some(waker);
            } else {
                inner.write_task = Some(waker);
            }
            if old.contains(signal) {
                inner.signals &= !signal;
                std::mem::drop(inner);
                self.handle.wait_async_handle(
                    self.receiver.port(),
                    self.receiver.key(),
                    signal | zx::Signals::OBJECT_PEER_CLOSED,
                    zx::WaitAsyncOpts::empty(),
                )?;
            }
            Poll::Pending
        }
    }
}

impl<T> ReadableHandle for RWHandle<T>
where
    T: AsHandleRef,
{
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<Result<ReadableState, zx::Status>> {
        loop {
            let signals = self.receiver().0.lock().unwrap().signals;
            match (signals.contains(OBJECT_READABLE), signals.contains(OBJECT_PEER_CLOSED)) {
                (true, false) => return Poll::Ready(Ok(ReadableState::Readable)),
                (_, true) => return Poll::Ready(Ok(ReadableState::MaybeReadableAndClosed)),
                (false, false) => {
                    ready!(self.need_signal(cx, true, OBJECT_READABLE)?)
                }
            }
        }
    }

    fn need_readable(&self, cx: &mut Context<'_>) -> Poll<Result<(), zx::Status>> {
        self.need_signal(cx, true, OBJECT_READABLE)
    }
}

impl<T> WritableHandle for RWHandle<T>
where
    T: AsHandleRef,
{
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<Result<WritableState, zx::Status>> {
        loop {
            let signals = self.receiver().0.lock().unwrap().signals;
            match (signals.contains(OBJECT_WRITABLE), signals.contains(OBJECT_PEER_CLOSED)) {
                (_, true) => return Poll::Ready(Ok(WritableState::Closed)),
                (true, _) => return Poll::Ready(Ok(WritableState::Writable)),
                (false, false) => {
                    ready!(self.need_signal(cx, false, OBJECT_WRITABLE)?)
                }
            }
        }
    }

    fn need_writable(&self, cx: &mut Context<'_>) -> Poll<Result<(), zx::Status>> {
        self.need_signal(cx, false, OBJECT_WRITABLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestExecutor;

    #[test]
    fn is_closed_immediately_after_close() {
        let mut exec = TestExecutor::new();
        let (tx, rx) = zx::Channel::create();
        let rx_rw_handle = RWHandle::new(rx);
        let mut noop_ctx = Context::from_waker(futures::task::noop_waker_ref());
        // Clear optimistic readable state
        assert!(rx_rw_handle.need_readable(&mut noop_ctx).is_pending());
        // Starting state: the channel is not closed (because we haven't closed it yet)
        assert_eq!(rx_rw_handle.is_closed(), false);
        // we will never set readable, so this should be Pending until we close
        assert_eq!(rx_rw_handle.poll_readable(&mut noop_ctx), Poll::Pending);

        drop(tx);

        // Implementation note: the cached state will not be updated yet
        assert_eq!(rx_rw_handle.poll_readable(&mut noop_ctx), Poll::Pending);
        // But is_closed should return true immediately
        assert_eq!(rx_rw_handle.is_closed(), true);
        // Still not updated, and won't be until we let the executor process port packets
        assert_eq!(rx_rw_handle.poll_readable(&mut noop_ctx), Poll::Pending);
        // So we do
        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        // And now it is updated, so we observe Closed
        assert_eq!(
            rx_rw_handle.poll_readable(&mut noop_ctx),
            Poll::Ready(Ok(ReadableState::MaybeReadableAndClosed))
        );
        // And is_closed should still be true, of course.
        assert_eq!(rx_rw_handle.is_closed(), true);
    }
}
