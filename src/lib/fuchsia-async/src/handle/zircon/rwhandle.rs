// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        runtime::{EHandle, PacketReceiver, ReceiverRegistration},
        OnSignals,
    },
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::task::{AtomicWaker, Context, Poll},
    std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

const OBJECT_PEER_CLOSED: zx::Signals = zx::Signals::OBJECT_PEER_CLOSED;
const OBJECT_READABLE: zx::Signals = zx::Signals::OBJECT_READABLE;
const OBJECT_WRITABLE: zx::Signals = zx::Signals::OBJECT_WRITABLE;

/// State of an object when it is ready for reading.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ReadableState {
    /// Received `OBJECT_READABLE`, or optimistically assuming the object is readable.
    Readable,
    /// Received `OBJECT_PEER_CLOSED`.
    Closed,
    /// Both `Readable` and `Closed` apply.
    ReadableAndClosed,
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
/// ready!(self.poll_readable(cx))?;
/// match /* make read syscall */ {
///     Err(zx::Status::SHOULD_WAIT) => {
///         self.need_readable(cx)?;
///         Poll::Pending
///     }
///     status => Poll::Ready(status),
/// }
/// ```
pub trait ReadableHandle {
    /// If the object is ready for reading, returns `Ready` with the readable
    /// state. If the implementor returns Pending, it should first ensure that
    /// `need_readable` is called.
    ///
    /// This should be called in a poll function before making a read syscall.
    /// If the syscall returns `SHOULD_WAIT`, you must call `need_readable` to
    /// schedule wakeup when the object is readable.
    ///
    /// The returned `ReadableState` does not necessarily reflect an observed
    /// `OBJECT_READABLE` signal. We optimistically assume the object remains
    /// readable until `need_readable` is called. If you only want to read once
    /// the object is confirmed to be readable, call `need_readable` with a
    /// no-op waker before the first poll.
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<Result<ReadableState, zx::Status>>;

    /// Arranges for the current task to be woken when the object receives an
    /// `OBJECT_READABLE` or `OBJECT_PEER_CLOSED` signal.
    fn need_readable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status>;
}

/// A `Handle` that receives notifications when it is writable.
///
/// # Examples
///
/// ```
/// ready!(self.poll_writable(cx))?;
/// match /* make write syscall */ {
///     Err(zx::Status::SHOULD_WAIT) => {
///         self.need_writable(cx)?;
///         Poll::Pending
///     }
///     status => Poll::Ready(status),
/// }
/// ```
pub trait WritableHandle {
    /// If the object is ready for writing, returns `Ready` with the writable
    /// state. If the implementor returns Pending, it should first ensure that
    /// `need_writable` is called.
    ///
    /// This should be called in a poll function before making a write syscall.
    /// If the syscall returns `SHOULD_WAIT`, you must call `need_writable` to
    /// schedule wakeup when the object is writable.
    ///
    /// The returned `WritableState` does not necessarily reflect an observed
    /// `OBJECT_WRITABLE` signal. We optimistically assume the object remains
    /// writable until `need_writable` is called. If you only want to write once
    /// the object is confirmed to be writable, call `need_writable` with a
    /// no-op waker before the first poll.
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<Result<WritableState, zx::Status>>;

    /// Arranges for the current task to be woken when the object receives an
    /// `OBJECT_WRITABLE` or `OBJECT_PEER_CLOSED` signal.
    fn need_writable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status>;
}

struct RWPacketReceiver {
    signals: AtomicU32,
    read_task: AtomicWaker,
    write_task: AtomicWaker,
}

impl PacketReceiver for RWPacketReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        let new = if let zx::PacketContents::SignalOne(p) = packet.contents() {
            p.observed()
        } else {
            return;
        };

        let old =
            zx::Signals::from_bits_truncate(self.signals.fetch_or(new.bits(), Ordering::SeqCst));

        let became_readable = new.contains(OBJECT_READABLE) && !old.contains(OBJECT_READABLE);
        let became_writable = new.contains(OBJECT_WRITABLE) && !old.contains(OBJECT_WRITABLE);
        let became_closed = new.contains(OBJECT_PEER_CLOSED) && !old.contains(OBJECT_PEER_CLOSED);

        if became_readable || became_closed {
            self.read_task.wake();
        }
        if became_writable || became_closed {
            self.write_task.wake();
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
    pub fn new(handle: T) -> Result<Self, zx::Status> {
        let ehandle = EHandle::local();

        let initial_signals = OBJECT_READABLE | OBJECT_WRITABLE;
        let receiver = ehandle.register_receiver(Arc::new(RWPacketReceiver {
            // Optimistically assume that the handle is readable and writable.
            // Reads and writes will be attempted before queueing a packet.
            // This makes handles slightly faster to read/write the first time
            // they're accessed after being created, provided they start off as
            // readable or writable. In return, there will be an extra wasted
            // syscall per read/write if the handle is not readable or writable.
            signals: AtomicU32::new(initial_signals.bits()),
            read_task: AtomicWaker::new(),
            write_task: AtomicWaker::new(),
        }));

        Ok(RWHandle { handle, receiver })
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
        let signals =
            zx::Signals::from_bits_truncate(self.receiver().signals.load(Ordering::Relaxed));
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
        match self.handle.wait_handle(OBJECT_PEER_CLOSED, zx::Time::INFINITE_PAST) {
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
    pub fn on_closed<'a>(&'a self) -> OnSignals<'a> {
        OnSignals::new(&self.handle, OBJECT_PEER_CLOSED)
    }

    fn receiver(&self) -> &RWPacketReceiver {
        self.receiver.receiver()
    }

    fn need_signal(
        &self,
        cx: &mut Context<'_>,
        task: &AtomicWaker,
        signal: zx::Signals,
    ) -> Result<(), zx::Status> {
        crate::runtime::need_signal_or_peer_closed(
            cx,
            task,
            &self.receiver.signals,
            signal,
            self.handle.as_handle_ref(),
            self.receiver.port(),
            self.receiver.key(),
        )
    }
}

impl<T> ReadableHandle for RWHandle<T>
where
    T: AsHandleRef,
{
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<Result<ReadableState, zx::Status>> {
        let signals =
            zx::Signals::from_bits_truncate(self.receiver().signals.load(Ordering::SeqCst));
        match (signals.contains(OBJECT_READABLE), signals.contains(OBJECT_PEER_CLOSED)) {
            (true, false) => Poll::Ready(Ok(ReadableState::Readable)),
            (false, true) => Poll::Ready(Ok(ReadableState::Closed)),
            (true, true) => Poll::Ready(Ok(ReadableState::ReadableAndClosed)),
            (false, false) => {
                self.need_signal(cx, &self.receiver.read_task, OBJECT_READABLE)?;
                Poll::Pending
            }
        }
    }

    fn need_readable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status> {
        self.need_signal(cx, &self.receiver.read_task, OBJECT_READABLE)
    }
}

impl<T> WritableHandle for RWHandle<T>
where
    T: AsHandleRef,
{
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<Result<WritableState, zx::Status>> {
        let signals =
            zx::Signals::from_bits_truncate(self.receiver().signals.load(Ordering::SeqCst));
        match (signals.contains(OBJECT_WRITABLE), signals.contains(OBJECT_PEER_CLOSED)) {
            (_, true) => Poll::Ready(Ok(WritableState::Closed)),
            (true, _) => Poll::Ready(Ok(WritableState::Writable)),
            (false, false) => {
                self.need_signal(cx, &self.receiver.write_task, OBJECT_WRITABLE)?;
                Poll::Pending
            }
        }
    }

    fn need_writable(&self, cx: &mut Context<'_>) -> Result<(), zx::Status> {
        self.need_signal(cx, &self.receiver.write_task, OBJECT_WRITABLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestExecutor;
    use fuchsia_zircon as zx;
    use std::task::Context;

    #[test]
    fn is_closed_immediately_after_close() {
        let mut exec = TestExecutor::new();
        let (tx, rx) = zx::Channel::create();
        let rx_rw_handle = RWHandle::new(rx).unwrap();
        let mut noop_ctx = Context::from_waker(futures::task::noop_waker_ref());
        // Clear optimistic readable state
        rx_rw_handle.need_readable(&mut noop_ctx).unwrap();
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
            Poll::Ready(Ok(ReadableState::Closed))
        );
        // And is_closed should still be true, of course.
        assert_eq!(rx_rw_handle.is_closed(), true);
    }
}
