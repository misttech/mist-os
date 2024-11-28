// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Stream socket buffer support types.

use std::fmt::Debug;
use std::mem::MaybeUninit;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;

use assert_matches::assert_matches;
use async_ringbuf::traits::{
    AsyncConsumer as _, AsyncProducer, Consumer as _, Observer as _, Producer as _, Split as _,
};
use fuchsia_async::{self as fasync, ReadableHandle as _, WritableHandle as _};

use futures::channel::{mpsc, oneshot};
use futures::{FutureExt as _, StreamExt as _};
use netstack3_core::socket::ShutdownType;
use netstack3_core::tcp::{
    Buffer, BufferLimits, FragmentedPayload, NoConnection, Payload, ReceiveBuffer, SendBuffer,
};
use netstack3_core::IpExt;

use super::TcpSocketId;
use crate::bindings::{BindingsCtx, Ctx};

/// Error emitted when new buffers are instantiated but there's no send/recv
/// task listening on the other side.
#[derive(Debug)]
pub(super) struct TaskStoppedError;

pub(crate) enum CoreReceiveBuffer {
    // TODO(https://fxbug.dev/364698967): We might be able to replace the zero
    // buffer with lazy allocations so we don't have this weird variant standing
    // out because of startup races.
    Zero,
    Ready {
        buffer: async_ringbuf::AsyncHeapProd<u8>,
        new_buffer_sender: mpsc::UnboundedSender<ReceiveBufferReader>,
        empty: bool,
        pending_capacity: Option<usize>,
    },
}

pub(super) type ReceiveBufferReader = async_ringbuf::AsyncHeapCons<u8>;

impl CoreReceiveBuffer {
    /// The minimum receive buffer size, in bytes.
    ///
    /// Borrowed from Linux: https://man7.org/linux/man-pages/man7/socket.7.html
    const MIN_CAPACITY: usize = 256;
    /// The maximum receive buffer size, in bytes.
    ///
    /// Borrowed from netstack2.
    const MAX_CAPACITY: usize = 4 << 20;

    pub(super) fn new_ready(
        rx_task_sender: mpsc::UnboundedSender<ReceiveBufferReader>,
        capacity: usize,
    ) -> Result<Self, TaskStoppedError> {
        let ring_buffer = async_ringbuf::AsyncHeapRb::new(capacity);
        let (prod, cons) = ring_buffer.split();
        rx_task_sender.unbounded_send(cons).map_err(|_: mpsc::TrySendError<_>| TaskStoppedError)?;
        Ok(Self::Ready {
            buffer: prod,
            new_buffer_sender: rx_task_sender,
            empty: true,
            pending_capacity: None,
        })
    }

    fn maybe_update_capacity(&mut self) {
        match self {
            Self::Zero => (),
            Self::Ready { buffer: _, new_buffer_sender, empty, pending_capacity } => {
                // When we turn to empty and we have a pending capacity request,
                // update our buffers.
                if let (true, Some(cap)) = (*empty, pending_capacity.as_ref()) {
                    match Self::new_ready(new_buffer_sender.clone(), *cap) {
                        Ok(r) => *self = r,
                        // If the task is stopped give up attempting to update
                        // the capacity.
                        Err(TaskStoppedError) => {
                            *pending_capacity = None;
                        }
                    }
                }
            }
        }
    }
}

impl Debug for CoreReceiveBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => write!(f, "CoreReceiveBuffer::Zero"),
            Self::Ready { buffer, new_buffer_sender: _, empty, pending_capacity } => f
                .debug_struct("CoreReceiveBuffer::Ready")
                .field("buffer_capacity", &buffer.capacity())
                .field("empty", empty)
                .field("pending_capacity", pending_capacity)
                .finish_non_exhaustive(),
        }
    }
}

impl Buffer for CoreReceiveBuffer {
    fn capacity_range() -> (usize, usize) {
        (Self::MIN_CAPACITY, Self::MAX_CAPACITY)
    }

    fn limits(&self) -> BufferLimits {
        match self {
            Self::Zero => BufferLimits { capacity: 0, len: 0 },
            Self::Ready { buffer, new_buffer_sender: _, empty: _, pending_capacity: _ } => {
                let len = buffer.occupied_len();
                let capacity = buffer.capacity().into();
                BufferLimits { capacity, len }
            }
        }
    }

    fn target_capacity(&self) -> usize {
        match self {
            Self::Zero => 0,
            Self::Ready { buffer, new_buffer_sender: _, empty: _, pending_capacity } => {
                pending_capacity.as_ref().copied().unwrap_or_else(|| buffer.capacity().into())
            }
        }
    }

    fn request_capacity(&mut self, size: usize) {
        match self {
            Self::Zero => {}
            Self::Ready { pending_capacity, .. } => {
                *pending_capacity = Some(size);
                self.maybe_update_capacity();
            }
        }
    }
}

/// Helper function to implement [`ReceiveBuffer`] for [`CoreReceiveBuffer`].
///
/// Returns the number of bytes from `payload` at `payload_offset` written into
/// `target` at `target_offset`.
fn write_payload<P: Payload>(
    target_offset: usize,
    payload_offset: usize,
    target: &mut [MaybeUninit<u8>],
    payload: &P,
) -> usize {
    if target_offset > target.len() {
        return 0;
    }
    let end = target.len().min(target_offset + payload.len() - payload_offset);
    let target = &mut target[target_offset..end];
    if target.is_empty() {
        // Avoid calling into payload if we have nothing to offer as a target.
        return 0;
    }
    payload.partial_copy_uninit(payload_offset, target);
    target.len()
}

impl ReceiveBuffer for CoreReceiveBuffer {
    fn write_at<P: Payload>(&mut self, offset: usize, data: &P) -> usize {
        match self {
            Self::Zero => 0,
            Self::Ready { buffer, new_buffer_sender: _, empty, pending_capacity: _ } => {
                let (a, b) = buffer.vacant_slices_mut();
                let mut written = write_payload(offset, 0, a, data);
                if let Some(offset) = (offset + written).checked_sub(a.len()) {
                    written += write_payload(offset, written, b, data);
                }
                // We must update `empty` here since we can't guarantee
                // `make_readable` will be called to update `empty`. So until it
                // does, we must assume that empty can only go from
                // `true->false` here whenever we write any bytes to the buffer.
                *empty &= written == 0;
                written
            }
        }
    }

    fn make_readable(&mut self, count: usize, has_outstanding: bool) {
        match self {
            Self::Zero => (),
            Self::Ready { buffer, new_buffer_sender: _, empty, pending_capacity: _ } => {
                // `empty` is tracking whether the *producer* side of the buffer
                // is empty, i.e., no outstanding bytes so we can always
                // overwrite with what the assembler tells us. The receive task
                // is responsible to drain every *consumer* side before moving
                // on to the next in case of capacity updates.
                *empty = !has_outstanding;
                // SAFETY:
                // - Not called concurrently (we hold a mutable reference here).
                // - Per ReceiveBuffer contract, all the bytes must've been
                //   initialized by write_at prior to marking them as readable,
                //   otherwise we'd be sending garbage to the application either
                //   way.
                unsafe { buffer.advance_write_index(count) };
                self.maybe_update_capacity();
            }
        }
    }
}

/// Abstracts [`Ctx`] and socket operations so [`receive_task`] can be tested
/// without core.
///
/// [`ReceiveTaskArgs`] is the proper production impl.
pub(super) trait ReceiveTaskOps {
    fn shutdown_recv(&mut self) -> Result<bool, NoConnection>;
    fn on_receive_buffer_read(&mut self);
}

pub(super) struct ReceiveTaskArgs<I: IpExt> {
    pub(super) ctx: Ctx,
    pub(super) id: TcpSocketId<I>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt> ReceiveTaskOps for ReceiveTaskArgs<I> {
    fn shutdown_recv(&mut self) -> Result<bool, NoConnection> {
        let Self { ctx, id } = self;
        ctx.api().tcp().shutdown(id, ShutdownType::Receive)
    }

    fn on_receive_buffer_read(&mut self) {
        let Self { ctx, id } = self;
        ctx.api().tcp().on_receive_buffer_read(id)
    }
}

/// Shuttles bytes from the core buffers into a zircon socket.
pub(super) async fn receive_task<O: ReceiveTaskOps>(
    socket: Arc<zx::Socket>,
    mut ops: O,
    mut receiver: mpsc::UnboundedReceiver<ReceiveBufferReader>,
) {
    let handle = fasync::RWHandle::new(&*socket);
    while let Some(mut buffer) = receiver.next().await {
        loop {
            let (a, b) = buffer.as_mut_slices();
            let avail = a.len() + b.len();
            // If there is no data for us to write into the zx socket, wait for
            // data to be written into the buffer by core as it receives
            // segments.
            if avail == 0 {
                if buffer.is_closed() {
                    // Curb a possible ring buffer race here. The producer side
                    // can write some data and then close so we need to
                    // double-check that the buffer is empty before dropping the
                    // consumer side to avoid data loss.
                    if buffer.is_empty() {
                        break;
                    } else {
                        continue;
                    }
                }
                buffer.wait_occupied(1).await;
                continue;
            }
            let a_written = if a.len() != 0 {
                futures::future::poll_fn(|ctx| loop {
                    let res = handle.get_ref().write(a).map_err(SocketErrorAction::from);
                    match res {
                        Err(SocketErrorAction::Wait) => {
                            futures::ready!(handle
                                .need_writable(ctx)
                                .map(|r| r.expect("waiting for writable")))
                        }
                        Err(SocketErrorAction::Shutdown) => return Poll::Ready(None),
                        Ok(written) => return Poll::Ready(Some(written)),
                    }
                })
                .await
            } else {
                Some(0)
            };
            let b_written = if a_written == Some(a.len()) && b.len() != 0 {
                // If we wrote everything into the socket then attempt a
                // non-waiting write from b.
                match handle.get_ref().write(b).map_err(SocketErrorAction::from) {
                    Ok(v) => Some(v),
                    Err(SocketErrorAction::Wait) => Some(0),
                    Err(SocketErrorAction::Shutdown) => None,
                }
            } else {
                Some(0)
            };

            let total_written = match (a_written, b_written) {
                (Some(a_written), Some(b_written)) => a_written + b_written,
                _ => {
                    // No more bytes can be written. Close the receive buffer, so our peer
                    // knows we can't read anymore. Then shutdown the task.
                    let _ = ops.shutdown_recv();
                    return;
                }
            };

            // Notify core that we've dequeued some bytes in case it wants to send an
            // updated window.
            if total_written > 0 {
                // NB: `skip` is a weird name for this method, it calls drop for all
                // the consumed bytes and then advances the read index.
                assert_eq!(
                    async_ringbuf::traits::Consumer::skip(&mut buffer, total_written),
                    total_written
                );

                ops.on_receive_buffer_read();
            }
        }
    }
    // If core has dropped its sender it means we're not receiving data anymore.
    // Notify applications so they're aware of it.
    socket
        .set_disposition(
            /* disposition */ Some(zx::SocketWriteDisposition::Disabled),
            /* peer_disposition */ None,
        )
        .expect("failed to set socket disposition");
}

#[derive(Debug)]
pub(crate) enum SendBufferPendingCapacity {
    Idle(oneshot::Sender<()>),
    Requested(usize),
}

pub(crate) enum CoreSendBuffer {
    // TODO(https://fxbug.dev/364698967): We might be able to replace the zero
    // buffer with lazy allocations so we don't have this weird variant standing
    // out because of startup races (see IntoBuffers impl for
    // UnconnectedSocketData).
    Zero,
    Ready {
        buffer: async_ringbuf::AsyncHeapCons<u8>,
        pending_capacity: SendBufferPendingCapacity,
    },
    /// Socket is shutting down send.
    ///
    /// When shutting down, the buffer may carry an additional vec `extra` that
    /// is used to read _once_ from the zircon socket so that all the data is
    /// readily available to core as the shutdown is processed. `extra` is
    /// treated as data that is read _after_ `buffer`. Once in `ShuttingDown`
    /// the `CoreSendBuffer` must _not_ take in any more bytes, only reading is
    /// allowed.
    ShuttingDown {
        buffer: async_ringbuf::AsyncHeapCons<u8>,
        extra: Vec<u8>,
        extra_offset: usize,
        target_capacity: usize,
    },
}

pub(crate) struct SendBufferWriter {
    pub(super) producer: async_ringbuf::AsyncHeapProd<u8>,
    pub(super) capacity_change_signal: oneshot::Receiver<()>,
}

impl Debug for CoreSendBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => write!(f, "CoreSendBuffer::Zero"),
            Self::Ready { buffer, pending_capacity } => f
                .debug_struct("CoreSendBuffer::Ready")
                .field("buffer_capacity", &buffer.capacity())
                .field("pending_capacity", pending_capacity)
                .finish_non_exhaustive(),
            Self::ShuttingDown { buffer, extra, extra_offset, target_capacity } => f
                .debug_struct("CoreSendBuffer::ShuttingDown")
                .field("buffer_capacity", &buffer.capacity())
                .field("target_capacity", target_capacity)
                .field("extra", &extra.len())
                .field("extra_offset", extra_offset)
                .finish_non_exhaustive(),
        }
    }
}

impl CoreSendBuffer {
    /// The minimum send buffer size, in bytes.
    ///
    /// Borrowed from Linux: https://man7.org/linux/man-pages/man7/socket.7.html
    const MIN_CAPACITY: usize = 2048;
    /// The maximum send buffer size, in bytes.
    ///
    /// Borrowed from netstack2.
    const MAX_CAPACITY: usize = 4 << 20;

    pub(super) fn new_ready(capacity: usize) -> (Self, SendBufferWriter) {
        let ring_buffer = async_ringbuf::AsyncHeapRb::new(capacity);
        let (producer, cons) = ring_buffer.split();
        let (capacity_change_sender, capacity_change_signal) = oneshot::channel();
        (
            Self::Ready {
                buffer: cons,
                pending_capacity: SendBufferPendingCapacity::Idle(capacity_change_sender),
            },
            SendBufferWriter { producer, capacity_change_signal },
        )
    }

    /// Applies a pending capacity request to the buffer.
    ///
    /// # Panics
    ///
    /// Panics if the buffer doesn't have a pending capacity request or if the
    /// buffer has not been fully flushed out to the network, if the buffer is
    /// shutting down or is the zero buffer.
    pub(super) fn apply_new_capacity(&mut self) -> SendBufferWriter {
        match self {
            Self::Zero => panic!("attempted to apply new capacity to zero buffer"),
            Self::Ready { buffer, pending_capacity } => {
                let new_capacity =
                    assert_matches!(pending_capacity, SendBufferPendingCapacity::Requested(n) => n);
                // Must not be called by the send task before waiting for the
                // buffer to be fully drained.
                assert!(buffer.is_empty());
                let (new_buffer, writer) = Self::new_ready(*new_capacity);
                *self = new_buffer;
                writer
            }
            Self::ShuttingDown { .. } => {
                panic!("attempted to apply new capacity to shutting down buffer")
            }
        }
    }
}

impl Buffer for CoreSendBuffer {
    fn capacity_range() -> (usize, usize) {
        (Self::MIN_CAPACITY, Self::MAX_CAPACITY)
    }

    fn limits(&self) -> BufferLimits {
        match self {
            Self::Zero => BufferLimits { capacity: 0, len: 0 },
            Self::Ready { buffer, pending_capacity: _ } => {
                let len = buffer.occupied_len();
                let capacity = buffer.capacity().get();
                BufferLimits { capacity, len }
            }
            Self::ShuttingDown { buffer, extra, extra_offset, target_capacity: _ } => {
                let len = buffer.occupied_len() + extra.len() - *extra_offset;
                let capacity = buffer.capacity().get() + extra.len();
                BufferLimits { capacity, len }
            }
        }
    }

    fn target_capacity(&self) -> usize {
        match self {
            Self::Zero => 0,
            Self::Ready { buffer, pending_capacity } => match pending_capacity {
                SendBufferPendingCapacity::Idle(_) => buffer.capacity().into(),
                SendBufferPendingCapacity::Requested(r) => *r,
            },
            Self::ShuttingDown { buffer: _, extra: _, extra_offset: _, target_capacity } => {
                *target_capacity
            }
        }
    }

    fn request_capacity(&mut self, size: usize) {
        match self {
            Self::Zero => (),
            Self::Ready { buffer: _, pending_capacity } => {
                match core::mem::replace(
                    pending_capacity,
                    SendBufferPendingCapacity::Requested(size),
                ) {
                    SendBufferPendingCapacity::Idle(s) => {
                        // Notify that we'd like to close the current ring
                        // buffer and replace it with a different capacity. The
                        // send task is responsible to listen for this signal
                        // and update the capacity when ready.
                        //
                        // If the send task has dropped the notifier assume
                        // we're racing with some shutdown. Keeping the new
                        // buffer sender around is not enough to guarantee the
                        // task is alive since it can be aborted.
                        let _: Result<(), ()> = s.send(());
                    }
                    SendBufferPendingCapacity::Requested(_) => {}
                }
            }
            Self::ShuttingDown { buffer: _, extra: _, extra_offset: _, target_capacity } => {
                // When shutting down just store the user request to prevent
                // weirdeness over the API but there's no way to fulfill it.
                *target_capacity = size;
            }
        }
    }
}

impl SendBuffer for CoreSendBuffer {
    type Payload<'a> = FragmentedPayload<'a, 3>;
    fn mark_read(&mut self, count: usize) {
        match self {
            Self::Zero => (),
            Self::Ready { buffer, pending_capacity: _ } => {
                assert_eq!(async_ringbuf::traits::Consumer::skip(buffer, count), count);
            }
            Self::ShuttingDown { buffer, extra, extra_offset, target_capacity: _ } => {
                // The producer must've been closed by the send task before
                // anything happens on this buffer. This ensures when we're
                // reading the buffer length we're not racing with anything
                // else.
                assert!(buffer.is_closed());
                let count = count - async_ringbuf::traits::Consumer::skip(buffer, count);
                *extra_offset += count;
                assert!(
                    *extra_offset <= extra.len(),
                    "{extra_offset} exceed available length {}",
                    extra.len()
                );
            }
        }
    }

    fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
    where
        F: FnOnce(Self::Payload<'a>) -> R,
    {
        match self {
            Self::Zero => {
                return f(FragmentedPayload::new_empty());
            }
            Self::Ready { buffer, pending_capacity: _ } => {
                let (a, b) = buffer.as_slices();
                if let Some(offset) = offset.checked_sub(a.len()) {
                    f(FragmentedPayload::new_contiguous(&b[offset..]))
                } else {
                    f(FragmentedPayload::from_iter([&a[offset..], b]))
                }
            }
            Self::ShuttingDown { buffer, extra, extra_offset, target_capacity: _ } => {
                let (a, b) = buffer.as_slices();
                let extra = &extra[*extra_offset..];
                match offset.checked_sub(a.len()) {
                    None => {
                        // Offset is in a.
                        f(FragmentedPayload::new([&a[offset..], b, extra]))
                    }
                    Some(b_offset) => match b_offset.checked_sub(b.len()) {
                        // Offset is in b.
                        None => f(FragmentedPayload::from_iter([&b[b_offset..], extra])),
                        // Offset is in extra.
                        Some(extra_offset) => {
                            f(FragmentedPayload::new_contiguous(&extra[extra_offset..]))
                        }
                    },
                }
            }
        }
    }
}

/// Abstracts [`Ctx`] and socket operations so [`send_task`] can be tested
/// without core.
///
/// [`SendTaskArgs`] is the proper production impl.
pub(super) trait SendTaskOps {
    fn do_send(&mut self);
    fn with_send_buffer<R, F: FnOnce(&mut CoreSendBuffer) -> R>(&mut self, f: F) -> Option<R>;
}

pub(super) struct SendTaskArgs<I: IpExt> {
    pub(super) ctx: Ctx,
    pub(super) id: TcpSocketId<I>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt> SendTaskOps for SendTaskArgs<I> {
    fn do_send(&mut self) {
        let Self { ctx, id } = self;
        ctx.api().tcp().do_send(id);
    }

    fn with_send_buffer<R, F: FnOnce(&mut CoreSendBuffer) -> R>(&mut self, f: F) -> Option<R> {
        let Self { ctx, id } = self;
        ctx.api().tcp().with_send_buffer(id, f)
    }
}

/// Shuttles bytes from the zircon socket into core buffers.
pub(super) async fn send_task<O: SendTaskOps>(
    socket: Arc<zx::Socket>,
    mut ops: O,
    shutdown_receiver: oneshot::Receiver<oneshot::Sender<()>>,
    first_buffer_receiver: oneshot::Receiver<SendBufferWriter>,
) {
    let handle = fasync::RWHandle::new(&*socket);
    let mut cur_buffer = None;

    let send_loop = async {
        match first_buffer_receiver.await {
            Ok(b) => {
                cur_buffer = Some(b);
            }
            Err(oneshot::Canceled) => {}
        };
        loop {
            let Some(SendBufferWriter { producer: buffer, capacity_change_signal }) =
                cur_buffer.as_mut()
            else {
                // We looped without a buffer, break.
                break;
            };

            // We fuse the capacity change signal here because observing the
            // canceled oneshot is load bearing here and, unfortunately,
            // oneshot::Receiver's FusedFuture implementation will avoid polling
            // when the sender side is closed which will eat the Canceled signal
            // we use to process core-side shutdown.
            //
            // Using `fuse` we sidestep the direct FusedFuture implementation
            // and guarantee we'll always see the future complete.
            //
            // TODO(https://github.com/rust-lang/futures-rs/issues/2455): Remove
            // this if it gets fixed upstream.
            let mut capacity_change_signal = capacity_change_signal.fuse();

            loop {
                // If the buffer we're looking at has been closed by core we
                // should no longer be attempting to put more bytes into it,
                // even if there's space available.
                if buffer.is_closed() {
                    return;
                }
                let (a, b) = buffer.vacant_slices_mut();
                let avail = a.len() + b.len();
                // If we don't have any space to use, wait for free space as we
                // send segments out.
                if avail == 0 {
                    buffer.wait_vacant(1).await;
                    continue;
                }
                let a_read = if a.len() != 0 {
                    let mut poll_socket = futures::future::poll_fn(|ctx| loop {
                        let res = handle.get_ref().read_uninit(a).map_err(SocketErrorAction::from);
                        match res {
                            Err(SocketErrorAction::Wait) => {
                                futures::ready!(handle
                                    .need_readable(ctx)
                                    // `need_readable` should not fail for valid
                                    // sockets with the correct rights.
                                    .map(|r| r.expect("waiting for readable")))
                            }
                            Err(SocketErrorAction::Shutdown) => return Poll::Ready(None),
                            Ok(read) => return Poll::Ready(Some(read.len())),
                        }
                    })
                    .fuse();
                    futures::select! {
                        r = poll_socket => match r {
                            Some(read) => read,
                            None => {
                                // No more bytes expected, shutdown.
                                return;
                            }
                        },
                        r = capacity_change_signal => match r {
                            Ok(()) => {
                                // Break out of the loop to flush the current
                                // buffer.
                                break;
                            },
                            // If the signal is dropped it means core closed the
                            // send buffer and we're being closed from the peer
                            // side (like network RST) so we should bail.
                            Err(oneshot::Canceled) => {
                                return;
                            },
                        },
                    }
                } else {
                    0
                };
                let b_read = if a_read == a.len() && b.len() != 0 {
                    // If we wrote everything into the first slice then attempt
                    // a non-waiting read into b.
                    match handle.get_ref().read_uninit(b).map_err(SocketErrorAction::from) {
                        Ok(read) => read.len(),
                        Err(SocketErrorAction::Wait) => 0,
                        Err(SocketErrorAction::Shutdown) => {
                            return;
                        }
                    }
                } else {
                    0
                };

                let total_read = a_read + b_read;
                // SAFETY: slices a and b have been initialized by zircon socket
                // reading up to the returned slice length. Buffer is
                // exclusively owned by this function.
                unsafe { buffer.advance_write_index(total_read) }

                assert!(total_read != 0);
                ops.do_send();
            }

            // When there's a pending buffer capacity update, wait for the
            // network flush without polling the zircon socket anymore and then
            // update the send buffer in the socket.

            // If all the capacity is vacant, that means everything has been
            // flushed to the network.
            buffer.wait_vacant(buffer.capacity().into()).await;
            cur_buffer = ops.with_send_buffer(|b| b.apply_new_capacity());
        }
    }
    .fuse();

    // We don't want to react to bindings dropping the shutdown sender, so
    // ensure we can't observe that future terminating in case that was dropped.
    let shutdown_receiver = shutdown_receiver.then(|x| async move {
        match x {
            Ok(sender) => sender,
            Err(oneshot::Canceled) => futures::future::pending().await,
        }
    });

    let signal_sender = {
        let mut send_loop = pin!(send_loop);
        let mut shutdown_receiver = pin!(shutdown_receiver);
        // Select biasing on the send loop, if that finishes we can just ignore
        // any outside shutdown requests.
        futures::select_biased! {
            () = send_loop => {
                // Send loop is over we can stop.
                return;
            }
            signal = shutdown_receiver => signal,
        }
    };

    // If we get here, it means application is requesting a send shutdown so we
    // must flush everything we can from the zircon socket and make it available
    // to core before responding and exiting from the send task.
    send_task_shutdown(socket, ops, cur_buffer);
    signal_sender.send(()).expect("shutdown receiver closed unexpectedly");
}

fn send_task_shutdown<O: SendTaskOps>(
    socket: Arc<zx::Socket>,
    mut ops: O,
    mut cur_buffer: Option<SendBufferWriter>,
) {
    // We have to look at the zircon socket to figure out how much data we need
    // to read and prealloc any necessary space.
    let zx::SocketInfo { mut rx_buf_available, .. } = socket.info().expect("zx socket get info");
    if rx_buf_available == 0 {
        // Early exit in case of no data available.
        return;
    }
    let (mut prod, new_cons) = match cur_buffer.take() {
        Some(SendBufferWriter { producer, capacity_change_signal: _ }) => (producer, None),
        None => {
            let ring_buffer = async_ringbuf::AsyncHeapRb::new(rx_buf_available);
            let (producer, cons) = ring_buffer.split();
            (producer, Some(cons))
        }
    };

    // Attempt to read as many bytes as we can from the zircon socket into our
    // available producer slices.
    let (a, b) = prod.vacant_slices_mut();
    let a_read = match socket.read_uninit(a).map_err(SocketErrorAction::from) {
        Ok(read) => read.len(),
        // We're already shutting down and we don't need to consider any waits
        // so consider any errors here as a no bytes available read.
        Err(SocketErrorAction::Wait | SocketErrorAction::Shutdown) => 0,
    };
    rx_buf_available -= a_read;
    // Only attempt to read into b if we still have expected available bytes.
    let b_read = if rx_buf_available != 0 {
        match socket.read_uninit(b).map_err(SocketErrorAction::from) {
            Ok(read) => read.len(),
            // Same as above.
            Err(SocketErrorAction::Wait | SocketErrorAction::Shutdown) => 0,
        }
    } else {
        0
    };
    rx_buf_available -= b_read;
    // SAFETY: slices a and b have been initialized by zircon socket
    // reading up to the returned slice length.
    unsafe { prod.advance_write_index(a_read + b_read) };
    // Ensure we can't produce anymore bytes from here on.
    std::mem::drop(prod);

    let extra = if rx_buf_available != 0 {
        let mut extra_uninit = vec![MaybeUninit::uninit(); rx_buf_available];
        match socket.read_uninit(&mut extra_uninit[..]).map_err(SocketErrorAction::from) {
            Ok(extra_init) => {
                let read = extra_init.len();
                // Nothing else should be reading from the socket and we've
                // allocated exactly how much we expect to see so we can assert
                // here that the extra vector's length is exactly what we got.
                assert_eq!(read, rx_buf_available);

                // SAFETY:
                // - MaybeUninit<u8> has the same layout as u8.
                // - Assertion above guarantees we've initialized the vector to
                //   its entire length.
                unsafe { std::mem::transmute::<Vec<MaybeUninit<u8>>, Vec<u8>>(extra_uninit) }
            }
            Err(SocketErrorAction::Wait | SocketErrorAction::Shutdown) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // We've accumulated all the data that the application has made available
    // now all that remains is pushing it into the core socket and let it drive
    // the connection to completion as needed.

    // We can ignore whether or not a send buffer was configured, given we could
    // be racing now with core state machine progression.
    let _: Option<()> = ops.with_send_buffer(move |b| {
        replace_with::replace_with(b, |b| {
            match b {
                CoreSendBuffer::Zero => {
                    // All this work for something else to have zeroed the
                    // buffer. We can only ignore it.
                    CoreSendBuffer::Zero
                }
                CoreSendBuffer::ShuttingDown { .. } => {
                    // This should be the only place we're putting the buffer in
                    // shutdown so we shouldn't find the socket with an already
                    // shutdown send buffer.
                    unreachable!("send buffer already shutting down")
                }
                CoreSendBuffer::Ready { buffer, pending_capacity } => {
                    let target_capacity = match pending_capacity {
                        SendBufferPendingCapacity::Idle(_) => buffer.capacity().get(),
                        SendBufferPendingCapacity::Requested(r) => r,
                    };
                    let buffer = match new_cons {
                        Some(new_cons) => {
                            // Update the consumer if we had to allocate a new
                            // one. Assert that the previous one did not have
                            // any data in it, otherwise this is dropping data.
                            assert!(buffer.is_empty());
                            new_cons
                        }
                        None => buffer,
                    };
                    CoreSendBuffer::ShuttingDown { buffer, extra, extra_offset: 0, target_capacity }
                }
            }
        })
    });
}

enum SocketErrorAction {
    Wait,
    Shutdown,
}

impl From<zx::Status> for SocketErrorAction {
    fn from(value: zx::Status) -> Self {
        match value {
            zx::Status::SHOULD_WAIT => Self::Wait,
            // If the socket is peer closed it means we're racing with socket
            // closure, so we can exit the task loop.
            zx::Status::PEER_CLOSED => Self::Shutdown,
            // BAD_STATE is reported on disposition change, which is caused by a
            // shutdown call. This means we can stop the task from running, core
            // should've discarded the buffers either way.
            zx::Status::BAD_STATE => Self::Shutdown,
            e => panic!("unexpected zircon socket error: {e:?}"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::cell::RefCell;
    use std::ops::Range;
    use std::rc::Rc;

    use proptest::strategy::{Just, Strategy};
    use proptest::test_runner::{Config, TestCaseError};
    use proptest::{prop_assert, prop_assert_eq, proptest};
    use proptest_support::failed_seeds;

    // Declare proptests. All functions are extracted to a `prop_`  variant to
    // keep rustfmt happier since it can't look inside this macro.
    proptest! {
        #![proptest_config(Config {
            failure_persistence: failed_seeds!(),
            ..Config::default()
        })]


        #[test]
        fn rcv_buffer_ready_in_order((warm, ops) in (
            0..(TEST_CAPACITY / 2),
            anybuffer::with_payload_ranges())
        ) {
            prop_rcv_buffer_ready_in_order(warm, ops)?
        }

        #[test]
        fn rcv_buffer_ready_out_of_order((warm, ops) in (
            0..TEST_CAPACITY,
            anybuffer::with_payload_ranges().prop_shuffle()
        )) {
            prop_rcv_buffer_ready_out_of_order(warm, ops)?
        }

        #[test]
        fn rcv_task_byte_shuttling(p in (1..=TEST_CAPACITY)){
            prop_rcv_task_byte_shuttling(p)?
        }

        #[test]
        fn send_buffer_ready((warm, seg, ack) in (
            0..TEST_CAPACITY,
            1..=TEST_CAPACITY,
            proptest::bool::ANY,
        )) {
            prop_send_buffer_ready_shutdown(warm, seg, None, ack)?
        }

        #[test]
        fn send_buffer_shutdown((warm, seg, extra, ack) in (
            0..TEST_CAPACITY,
            1..=TEST_CAPACITY,
            1..=TEST_CAPACITY,
            proptest::bool::ANY,
        )) {
            prop_send_buffer_ready_shutdown(warm, seg, Some(extra), ack)?
        }

        #[test]
        fn send_task_byte_shuttling((warm, seg) in (
            0..TEST_CAPACITY,
            1..=TEST_CAPACITY,
        )) {
            prop_send_task_byte_shuttling(warm, seg)?
        }

        #[test]
        fn send_task_shutdown((before, pending) in (
            proptest::option::of(((0..TEST_CAPACITY), (0..=TEST_CAPACITY))),
            0..=2*TEST_CAPACITY,
        )) {
            prop_send_task_shutdown(before, pending)?
        }
    }

    const TEST_CAPACITY: usize = 16;
    const TEST_PAYLOAD: [u8; TEST_CAPACITY] =
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

    fn prop_rcv_buffer_ready_in_order(
        warm: usize,
        ops: impl IntoIterator<Item = Range<usize>>,
    ) -> Result<(), TestCaseError> {
        // Use half capacity to test partial writes.
        const CAPACITY: usize = TEST_CAPACITY / 2;
        let (mut buffer, mut reader) = new_ready_receive_buffer(CAPACITY);
        warm_receive_buffer(&mut buffer, &mut reader, warm);
        prop_assert_eq!(buffer.limits(), BufferLimits { len: 0, capacity: CAPACITY });

        let len = ops.into_iter().try_fold(0, |offset, test_range| {
            let wr = buffer.write_at(0, &&TEST_PAYLOAD[test_range.clone()]);
            prop_assert_eq!(wr, test_range.end.min(CAPACITY) - offset);

            prop_assert_eq!(reader.occupied_len(), offset);
            prop_assert_eq!(buffer.limits(), BufferLimits { len: offset, capacity: CAPACITY });

            buffer.make_readable(wr, false);
            let end = wr + offset;
            prop_assert_eq!(reader.occupied_len(), end);
            prop_assert_eq!(clone_ringbuf(&reader), &TEST_PAYLOAD[0..end]);
            prop_assert_eq!(buffer.limits(), BufferLimits { len: end, capacity: CAPACITY });
            Ok(end)
        })?;
        prop_assert_eq!(async_ringbuf::traits::Consumer::skip(&mut reader, len), len);
        prop_assert_eq!(buffer.limits(), BufferLimits { len: 0, capacity: CAPACITY });

        Ok(())
    }

    fn prop_rcv_buffer_ready_out_of_order(
        warm: usize,
        ops: impl IntoIterator<Item = Range<usize>>,
    ) -> Result<(), TestCaseError> {
        let (mut buffer, mut reader) = new_ready_receive_buffer(TEST_CAPACITY);
        warm_receive_buffer(&mut buffer, &mut reader, warm);
        prop_assert_eq!(buffer.limits(), BufferLimits { len: 0, capacity: TEST_CAPACITY });

        let len = ops.into_iter().try_fold(0, |written, test_range| {
            let wr = buffer.write_at(test_range.start, &&TEST_PAYLOAD[test_range.clone()]);
            prop_assert_eq!(wr, test_range.end - test_range.start);
            Ok(written + wr)
        })?;

        // We have received the ranges possibly out of order, mark readable only
        // once and check that we end up in the right state.
        prop_assert_eq!(buffer.limits(), BufferLimits { len: 0, capacity: TEST_CAPACITY });
        buffer.make_readable(len, false);
        prop_assert_eq!(buffer.limits(), BufferLimits { len, capacity: TEST_CAPACITY });

        prop_assert_eq!(clone_ringbuf(&reader), &TEST_PAYLOAD[0..len]);
        prop_assert_eq!(async_ringbuf::traits::Consumer::skip(&mut reader, len), len);
        prop_assert_eq!(buffer.limits(), BufferLimits { len: 0, capacity: TEST_CAPACITY });

        Ok(())
    }

    fn prop_rcv_task_byte_shuttling(partial_read: usize) -> Result<(), TestCaseError> {
        // Set up a buffer and a zircon socket that is full enough to cause
        // partial writes by the read task. The happy byte-shuttling case is
        // covered by integration tests.
        let mut executor = fasync::TestExecutor::new();
        let (mut buffer, chan) = new_ready_receive_buffer_and_channel(TEST_CAPACITY);
        let (socket, task_socket) = zx::Socket::create_stream();

        let zx::SocketInfo { rx_buf_max, .. } = socket.info().unwrap();
        let socket_preamble = rx_buf_max - partial_read;
        let total_msg = std::iter::repeat(0xAA)
            .take(socket_preamble)
            .chain(TEST_PAYLOAD)
            .chain(TEST_PAYLOAD)
            .collect::<Vec<_>>();
        let mut recvbuf = vec![0u8; total_msg.len()];
        prop_assert_eq!(task_socket.write(&total_msg[..socket_preamble]), Ok(socket_preamble));

        let rcv_task = receive_task(Arc::new(task_socket), (), chan);
        let mut rcv_task = pin!(rcv_task);

        prop_assert_eq!(executor.run_until_stalled(&mut rcv_task), Poll::Pending);

        let mut send_slice = &total_msg[socket_preamble..];
        let mut recv_slice = &mut recvbuf[..];

        while !send_slice.is_empty() {
            let wr = buffer.write_at(0, &send_slice);
            prop_assert!(wr != 0, "test can't make progress");

            buffer.make_readable(wr, false);
            prop_assert_eq!(executor.run_until_stalled(&mut rcv_task), Poll::Pending);
            prop_assert_eq!(socket.read(&mut recv_slice[..partial_read]), Ok(partial_read));
            send_slice = &send_slice[wr..];
            recv_slice = &mut recv_slice[partial_read..];
        }
        // We managed to send TEST_PAYLOAD twice over the socket, read
        // everything else now and check that the data is correct.
        let in_buffer = buffer.limits().len;
        let in_socket = recv_slice.len() - in_buffer;
        prop_assert_eq!(socket.read(recv_slice), Ok(in_socket));
        prop_assert_eq!(executor.run_until_stalled(&mut rcv_task), Poll::Pending);
        if in_buffer != 0 {
            prop_assert_eq!(socket.read(&mut recv_slice[in_socket..]), Ok(in_buffer));
        }

        // Most of the buffer is basically trash, just compare the tail of the
        // buffers to avoid comparing the entire buffer which is ~256K long to
        // match the zircon socket.
        let buffer_tail = total_msg.len() - TEST_PAYLOAD.len() * 2 - partial_read;
        prop_assert_eq!(&total_msg[buffer_tail..], &recvbuf[buffer_tail..]);

        // Dropping buffer causes receive task to end.
        drop(buffer);
        prop_assert_eq!(executor.run_until_stalled(&mut rcv_task), Poll::Ready(()));

        Ok(())
    }

    fn prop_send_buffer_ready_shutdown(
        warm: usize,
        segment: usize,
        extra: Option<usize>,
        ack: bool,
    ) -> Result<(), TestCaseError> {
        let ring_buffer = async_ringbuf::AsyncHeapRb::new(TEST_CAPACITY);
        let (mut producer, mut cons) = ring_buffer.split();
        // Prime the buffer with `warm` bytes` so we exercise the ring buffer
        // split slices.
        prop_assert_eq!(producer.push_iter(std::iter::repeat(0xAA).take(warm)), warm);
        prop_assert_eq!(async_ringbuf::traits::Consumer::skip(&mut cons, warm), warm);

        prop_assert_eq!(producer.push_slice(&TEST_PAYLOAD), TEST_PAYLOAD.len());

        let (mut buffer, producer) = match extra {
            Some(extra) => {
                drop(producer);
                (
                    CoreSendBuffer::ShuttingDown {
                        buffer: cons,
                        extra: (&TEST_PAYLOAD[..extra]).to_vec(),
                        extra_offset: 0,
                        target_capacity: TEST_CAPACITY,
                    },
                    None,
                )
            }
            None => (
                CoreSendBuffer::Ready {
                    buffer: cons,
                    // NB: arbitrary, just easier to construct than the idle
                    // variant.
                    pending_capacity: SendBufferPendingCapacity::Requested(TEST_CAPACITY),
                },
                Some(producer),
            ),
        };

        let expect = TEST_PAYLOAD
            .into_iter()
            .chain((&TEST_PAYLOAD[..extra.unwrap_or(0)]).into_iter().copied())
            .collect::<Vec<_>>();
        let mut read = vec![0xBB; expect.len()];

        prop_assert_eq!(buffer.limits().len, expect.len());

        let mut read_offset = 0;
        let mut buffer_offset = 0;
        while read_offset != read.len() {
            let read_end = (read_offset + segment).min(read.len());
            buffer.peek_with(buffer_offset, |pl| {
                pl.partial_copy(0, &mut read[read_offset..read_end]);
            });
            prop_assert_eq!(&read[read_offset..read_end], &expect[read_offset..read_end]);

            let did_read = read_end - read_offset;
            read_offset = read_end;

            if ack {
                buffer.mark_read(did_read);
                prop_assert_eq!(buffer.limits().len, read.len() - read_end);
                // NB: Producer must be dropped for the shutting down buffer
                // case, since we don't allow any more bytes to be produced
                // there.
                if let Some(producer) = producer.as_ref() {
                    prop_assert_eq!(producer.vacant_len(), read_offset);
                }
            } else {
                buffer_offset += did_read;
            }
        }
        prop_assert_eq!(read, expect);
        Ok(())
    }

    fn prop_send_task_byte_shuttling(warm: usize, seg: usize) -> Result<(), TestCaseError> {
        let mut executor = fasync::TestExecutor::new();

        let (mut buffer, mut writer) = CoreSendBuffer::new_ready(TEST_CAPACITY);

        prop_assert_eq!(writer.producer.push_iter(std::iter::repeat(0xAA).take(warm)), warm);
        buffer.mark_read(warm);

        let buffer = Rc::new(RefCell::new(buffer));
        let (socket, task_socket) = zx::Socket::create_stream();
        let (sender, start) = oneshot::channel();
        let (_shutdown_sender, shutdown_receiver) = oneshot::channel();
        sender.send(writer).map_err(|_: SendBufferWriter| ()).unwrap();

        let send_task =
            send_task(Arc::new(task_socket), Rc::clone(&buffer), shutdown_receiver, start);
        let mut send_task = pin!(send_task);

        prop_assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        let expect = [&TEST_PAYLOAD[..], &TEST_PAYLOAD[..]].concat();
        prop_assert_eq!(socket.write(&expect), Ok(expect.len()));

        // Loop until we've accumulated `expect` in our receive
        // buffer.
        let mut received = vec![0u8; expect.len()];
        let mut received_offset = 0;
        while received_offset != received.len() {
            prop_assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
            let mut buffer = buffer.borrow_mut();
            let expect_len = TEST_CAPACITY.min(received.len() - received_offset);
            prop_assert_eq!(buffer.limits().len, expect_len);

            let received_end = received.len().min(received_offset + seg);
            buffer
                .peek_with(0, |f| f.partial_copy(0, &mut received[received_offset..received_end]));
            let mark_read = received_end - received_offset;
            buffer.mark_read(mark_read);
            prop_assert_eq!(buffer.limits().len, expect_len - mark_read);
            received_offset = received_end;
        }

        prop_assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        prop_assert_eq!(buffer.borrow().limits().len, 0);
        prop_assert_eq!(received, expect);

        // Drop the consumer, the send task should finish.
        *buffer.borrow_mut() = CoreSendBuffer::Zero;
        prop_assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Ready(()));
        Ok(())
    }

    fn prop_send_task_shutdown(
        before: Option<(usize, usize)>,
        pending: usize,
    ) -> Result<(), TestCaseError> {
        let mut expect = vec![];

        let (mut buffer, mut writer) = CoreSendBuffer::new_ready(TEST_CAPACITY);
        let (buffer, writer) = match before {
            Some((warm, in_buffer)) => {
                prop_assert_eq!(
                    writer.producer.push_iter(std::iter::repeat(0xAA).take(warm)),
                    warm
                );
                buffer.mark_read(warm);

                let in_buffer = &TEST_PAYLOAD[..in_buffer];
                expect.extend_from_slice(in_buffer);
                prop_assert_eq!(writer.producer.push_slice(in_buffer), in_buffer.len());
                (buffer, Some(writer))
            }
            None => (buffer, None),
        };

        // Use a different slice for what goes into extra so alignment to
        // capacity doesn't hide problems.
        let pending =
            TEST_PAYLOAD.into_iter().cycle().map(|x| x | 0x80).take(pending).collect::<Vec<_>>();
        expect.extend_from_slice(&pending[..]);

        let buffer = Rc::new(RefCell::new(buffer));
        let (socket, task_socket) = zx::Socket::create_stream();
        prop_assert_eq!(socket.write(&pending[..]), Ok(pending.len()));

        super::send_task_shutdown(Arc::new(task_socket), Rc::clone(&buffer), writer);
        let mut buffer = Rc::try_unwrap(buffer).unwrap().into_inner();

        if !pending.is_empty() {
            let (buffer, extra) = assert_matches!(
                &buffer,
                CoreSendBuffer::ShuttingDown {
                    buffer,
                    extra,
                    extra_offset: _,
                    target_capacity: _
                } => (buffer, extra)
            );
            let expect_buffer_len = expect.len().min(buffer.capacity().get());
            prop_assert_eq!(buffer.occupied_len(), expect_buffer_len);
            prop_assert_eq!(extra.len(), expect.len() - expect_buffer_len);
        }

        let mut got = vec![0xAAu8; expect.len()];
        buffer.peek_with(0, |p| p.partial_copy(0, &mut got[..]));
        prop_assert_eq!(got, expect);
        Ok(())
    }

    #[test]
    fn rcv_buffer_update_capacity() {
        let (mut buffer, mut chan) = new_ready_receive_buffer_and_channel(TEST_CAPACITY);
        let reader = chan.next().now_or_never().flatten().unwrap();
        assert_eq!(buffer.target_capacity(), TEST_CAPACITY);

        const CAP1: usize = 2;
        const CAP2: usize = 4;
        const CAP3: usize = 8;

        // Attempt to update capacity on an empty buffer, should succeed
        // immediately.
        buffer.request_capacity(CAP1);
        assert_eq!(buffer.target_capacity(), CAP1);
        assert!(reader.is_closed());
        let reader = chan.next().now_or_never().flatten().unwrap();
        assert_eq!(reader.capacity().get(), CAP1);

        const PAYLOAD: &'static [u8] = &[1, 2];
        assert_eq!(buffer.write_at(0, &PAYLOAD), PAYLOAD.len());
        buffer.request_capacity(CAP2);
        assert_eq!(buffer.target_capacity(), CAP2);
        assert!(!reader.is_closed());

        assert_eq!(buffer.write_at(0, &()), 0);
        buffer.request_capacity(CAP3);
        assert_eq!(buffer.target_capacity(), CAP3);
        assert!(!reader.is_closed());

        buffer.make_readable(1, /* has_outstanding */ true);
        assert!(!reader.is_closed());
        buffer.make_readable(1, /* has_outstanding */ false);

        assert!(reader.is_closed());
        assert_eq!(clone_ringbuf(&reader), PAYLOAD);
        let reader = chan.next().now_or_never().flatten().unwrap();
        assert_eq!(reader.capacity().get(), CAP3);
        assert!(!reader.is_closed());

        // After dropping the buffer the channel should be closed no more
        // surprise readers.
        drop(buffer);
        assert!(reader.is_closed());
        assert!(chan.next().now_or_never().unwrap().is_none());
    }

    #[test]
    fn send_task_change_capacity() {
        let mut executor = fasync::TestExecutor::new();

        const CAP1: usize = TEST_CAPACITY;
        const CAP2: usize = CAP1 + 1;
        const CAP3: usize = CAP2 + 1;

        let (buffer, writer) = CoreSendBuffer::new_ready(CAP1);
        let buffer = Rc::new(RefCell::new(buffer));
        let (socket, task_socket) = zx::Socket::create_stream();
        let (sender, start) = oneshot::channel();
        let (_shutdown_sender, shutdown_receiver) = oneshot::channel();
        sender.send(writer).map_err(|_: SendBufferWriter| ()).unwrap();

        let send_task =
            send_task(Arc::new(task_socket), Rc::clone(&buffer), shutdown_receiver, start);
        let mut send_task = pin!(send_task);

        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);

        assert_eq!(buffer.borrow().limits(), BufferLimits { len: 0, capacity: CAP1 });
        assert_eq!(buffer.borrow().target_capacity(), CAP1);
        buffer.borrow_mut().request_capacity(CAP2);
        assert_eq!(buffer.borrow().limits(), BufferLimits { len: 0, capacity: CAP1 });
        assert_eq!(buffer.borrow().target_capacity(), CAP2);

        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        assert_eq!(buffer.borrow().limits(), BufferLimits { len: 0, capacity: CAP2 });
        assert_eq!(buffer.borrow().target_capacity(), CAP2);

        assert_eq!(socket.write(&TEST_PAYLOAD), Ok(TEST_PAYLOAD.len()));
        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        assert_eq!(
            buffer.borrow().limits(),
            BufferLimits { len: TEST_PAYLOAD.len(), capacity: CAP2 }
        );

        buffer.borrow_mut().request_capacity(CAP3);
        assert_eq!(
            buffer.borrow().limits(),
            BufferLimits { len: TEST_PAYLOAD.len(), capacity: CAP2 }
        );
        assert_eq!(buffer.borrow().target_capacity(), CAP3);

        // There's still pending data in the buffer, capacity must not have
        // changed yet.
        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        assert_eq!(
            buffer.borrow().limits(),
            BufferLimits { len: TEST_PAYLOAD.len(), capacity: CAP2 }
        );

        buffer.borrow_mut().mark_read(TEST_PAYLOAD.len());
        assert_eq!(buffer.borrow().limits(), BufferLimits { len: 0, capacity: CAP2 });

        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Pending);
        assert_eq!(buffer.borrow().limits(), BufferLimits { len: 0, capacity: CAP3 });
        assert_eq!(buffer.borrow().target_capacity(), CAP3);

        // Drop the consumer, the send task should finish.
        *buffer.borrow_mut() = CoreSendBuffer::Zero;
        assert_eq!(executor.run_until_stalled(&mut send_task), Poll::Ready(()));
    }

    impl ReceiveTaskOps for () {
        fn shutdown_recv(&mut self) -> Result<bool, NoConnection> {
            Ok(true)
        }

        fn on_receive_buffer_read(&mut self) {}
    }

    impl SendTaskOps for Rc<RefCell<CoreSendBuffer>> {
        fn do_send(&mut self) {}
        fn with_send_buffer<R, F: FnOnce(&mut CoreSendBuffer) -> R>(&mut self, f: F) -> Option<R> {
            Some(f(&mut self.borrow_mut()))
        }
    }

    fn clone_ringbuf<B: async_ringbuf::traits::Consumer<Item = u8>>(b: &B) -> Vec<u8> {
        let (a, b) = b.as_slices();
        [a, b].concat()
    }

    fn new_ready_receive_buffer_and_channel(
        capacity: usize,
    ) -> (CoreReceiveBuffer, mpsc::UnboundedReceiver<ReceiveBufferReader>) {
        let (snd, rcv) = mpsc::unbounded();
        let b = CoreReceiveBuffer::new_ready(snd, capacity).unwrap();
        (b, rcv)
    }

    fn new_ready_receive_buffer(capacity: usize) -> (CoreReceiveBuffer, ReceiveBufferReader) {
        let (b, mut rcv) = new_ready_receive_buffer_and_channel(capacity);
        let rd = rcv.next().now_or_never().flatten().unwrap();
        (b, rd)
    }

    fn warm_receive_buffer(
        buffer: &mut CoreReceiveBuffer,
        reader: &mut ReceiveBufferReader,
        warm: usize,
    ) {
        let buffer = assert_matches!(buffer, CoreReceiveBuffer::Ready { buffer, .. } => buffer);
        assert_eq!(buffer.push_iter(std::iter::repeat(0xAA).take(warm)), warm);
        assert_eq!(async_ringbuf::traits::Consumer::skip(reader, warm), warm);
    }

    mod anybuffer {
        use super::*;

        /// Produces 3 random ranges covering contiguous subslices of
        /// `TEST_PAYLOAD` (in order).
        pub(super) fn with_payload_ranges() -> impl Strategy<Value = [Range<usize>; 3]> {
            (0..TEST_CAPACITY)
                .prop_flat_map(|start| {
                    (Just(0..start), (start..TEST_CAPACITY).prop_map(move |end| start..end))
                })
                .prop_flat_map(|(first, second)| {
                    let start = second.end;
                    let third = (start..TEST_CAPACITY).prop_map(move |end| start..end);
                    (Just(first), Just(second), third)
                })
                .prop_map(|(a, b, c)| [a, b, c])
        }
    }
}
