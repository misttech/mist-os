// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A transport implementation which uses Zircon channels.

use core::future::Future;
use core::mem::replace;
use core::pin::Pin;
use core::slice::from_raw_parts_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Context, Poll};
use std::sync::Arc;

use fuchsia_async::{RWHandle, ReadableHandle as _};
use futures::task::AtomicWaker;
use zx::sys::{
    zx_channel_read, zx_channel_write, ZX_ERR_BUFFER_TOO_SMALL, ZX_ERR_PEER_CLOSED,
    ZX_ERR_SHOULD_WAIT, ZX_OK,
};
use zx::{AsHandleRef as _, Channel, Handle, Status};

use crate::decoder::InternalHandleDecoder;
use crate::fuchsia::{HandleDecoder, HandleEncoder};
use crate::protocol::Transport;
use crate::{Chunk, DecodeError, Decoder, Encoder, CHUNK_SIZE};

struct Shared {
    is_closed: AtomicBool,
    sender_count: AtomicUsize,
    closed_waker: AtomicWaker,
    channel: RWHandle<Channel>,
    // TODO: recycle send/recv buffers to reduce allocations
}

impl Shared {
    fn new(channel: Channel) -> Self {
        Self {
            is_closed: AtomicBool::new(false),
            sender_count: AtomicUsize::new(1),
            closed_waker: AtomicWaker::new(),
            channel: RWHandle::new(channel),
        }
    }

    fn close(&self) {
        self.is_closed.store(true, Ordering::Relaxed);
        self.closed_waker.wake();
    }
}

/// A channel sender.
pub struct Sender {
    shared: Arc<Shared>,
}

impl Drop for Sender {
    fn drop(&mut self) {
        let senders = self.shared.sender_count.fetch_sub(1, Ordering::Relaxed);
        if senders == 1 {
            self.shared.close();
        }
    }
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Ordering::Relaxed);
        Self { shared: self.shared.clone() }
    }
}

/// A channel buffer.
pub struct Buffer {
    handles: Vec<Handle>,
    chunks: Vec<Chunk>,
}

impl Buffer {
    fn new() -> Self {
        Self { handles: Vec::new(), chunks: Vec::new() }
    }
}

impl Encoder for Buffer {
    #[inline]
    fn bytes_written(&self) -> usize {
        Encoder::bytes_written(&self.chunks)
    }

    #[inline]
    fn reserve(&mut self, len: usize) {
        Encoder::reserve(&mut self.chunks, len)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        Encoder::write(&mut self.chunks, bytes)
    }

    #[inline]
    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        Encoder::rewrite(&mut self.chunks, pos, bytes)
    }

    #[inline]
    fn __internal_handle_count(&self) -> usize {
        self.handles.len()
    }
}

impl HandleEncoder for Buffer {
    fn push_handle(&mut self, handle: Handle) -> Result<(), crate::EncodeError> {
        self.handles.push(handle);
        Ok(())
    }

    fn handles_pushed(&self) -> usize {
        self.handles.len()
    }
}

/// A channel send future.
#[must_use = "futures do nothing unless polled"]
pub struct SendFuture<'s> {
    shared: &'s Shared,
    buffer: Buffer,
}

impl Future for SendFuture<'_> {
    type Output = Result<(), Status>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);

        let result = unsafe {
            zx_channel_write(
                this.shared.channel.get_ref().raw_handle(),
                0,
                this.buffer.chunks.as_ptr().cast::<u8>(),
                (this.buffer.chunks.len() * CHUNK_SIZE) as u32,
                this.buffer.handles.as_ptr().cast(),
                this.buffer.handles.len() as u32,
            )
        };

        if result == ZX_OK {
            // Handles were written to the channel, so we must not drop them.
            unsafe {
                this.buffer.handles.set_len(0);
            }
            Poll::Ready(Ok(()))
        } else {
            Poll::Ready(Err(Status::from_raw(result)))
        }
    }
}

/// A channel receiver.
pub struct Receiver {
    shared: Arc<Shared>,
}

/// A channel receive future.
#[must_use = "futures do nothing unless polled"]
pub struct RecvFuture<'r> {
    shared: &'r Shared,
    buffer: Option<Buffer>,
}

impl Future for RecvFuture<'_> {
    type Output = Result<Option<RecvBuffer>, Status>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        let buffer = this.buffer.as_mut().unwrap();

        let mut actual_bytes = 0;
        let mut actual_handles = 0;

        loop {
            let result = unsafe {
                zx_channel_read(
                    this.shared.channel.get_ref().raw_handle(),
                    0,
                    buffer.chunks.as_mut_ptr().cast(),
                    buffer.handles.as_mut_ptr().cast(),
                    (buffer.chunks.capacity() * CHUNK_SIZE) as u32,
                    buffer.handles.capacity() as u32,
                    &mut actual_bytes,
                    &mut actual_handles,
                )
            };

            match result {
                ZX_OK => {
                    unsafe {
                        buffer.chunks.set_len(actual_bytes as usize / CHUNK_SIZE);
                        buffer.handles.set_len(actual_handles as usize);
                    }
                    return Poll::Ready(Ok(Some(RecvBuffer {
                        buffer: this.buffer.take().unwrap(),
                        chunks_taken: 0,
                        handles_taken: 0,
                    })));
                }
                ZX_ERR_PEER_CLOSED => return Poll::Ready(Ok(None)),
                ZX_ERR_BUFFER_TOO_SMALL => {
                    let min_chunks = (actual_bytes as usize).div_ceil(CHUNK_SIZE);
                    buffer.chunks.reserve(min_chunks - buffer.chunks.capacity());
                    buffer.handles.reserve(actual_handles as usize - buffer.handles.capacity());
                }
                ZX_ERR_SHOULD_WAIT => {
                    if matches!(this.shared.channel.need_readable(cx)?, Poll::Pending) {
                        this.shared.closed_waker.register(cx.waker());
                        if this.shared.is_closed.load(Ordering::Relaxed) {
                            return Poll::Ready(Ok(None));
                        }
                        return Poll::Pending;
                    }
                }
                raw => return Poll::Ready(Err(Status::from_raw(raw))),
            }
        }
    }
}

/// A channel receive buffer.
pub struct RecvBuffer {
    buffer: Buffer,
    chunks_taken: usize,
    handles_taken: usize,
}

impl<'buf> Decoder<'buf> for &'buf mut RecvBuffer {
    fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], DecodeError> {
        if count > self.buffer.chunks.len() - self.chunks_taken {
            return Err(DecodeError::InsufficientData);
        }

        let chunks = unsafe {
            from_raw_parts_mut(self.buffer.chunks.as_mut_ptr().add(self.chunks_taken), count)
        };
        self.chunks_taken += count;

        Ok(chunks)
    }

    fn finish(&mut self) -> Result<(), DecodeError> {
        if self.chunks_taken != self.buffer.chunks.len() {
            return Err(DecodeError::ExtraBytes {
                num_extra: (self.buffer.chunks.len() - self.chunks_taken) * CHUNK_SIZE,
            });
        }

        if self.handles_taken != self.buffer.handles.len() {
            return Err(DecodeError::ExtraHandles {
                num_extra: self.buffer.handles.len() - self.handles_taken,
            });
        }

        Ok(())
    }
}

impl InternalHandleDecoder for &mut RecvBuffer {
    fn __internal_take_handles(&mut self, count: usize) -> Result<(), DecodeError> {
        if count > self.buffer.handles.len() - self.handles_taken {
            return Err(DecodeError::InsufficientHandles);
        }

        for i in self.handles_taken..self.handles_taken + count {
            let handle = replace(&mut self.buffer.handles[i], Handle::invalid());
            drop(handle);
        }
        self.handles_taken += count;

        Ok(())
    }

    fn __internal_handles_remaining(&self) -> usize {
        self.buffer.handles.len() - self.handles_taken
    }
}

impl HandleDecoder for &mut RecvBuffer {
    fn take_handle(&mut self) -> Result<Handle, DecodeError> {
        if self.handles_taken >= self.buffer.handles.len() {
            return Err(DecodeError::InsufficientHandles);
        }

        let handle = replace(&mut self.buffer.handles[self.handles_taken], Handle::invalid());
        self.handles_taken += 1;

        Ok(handle)
    }

    fn handles_remaining(&mut self) -> usize {
        self.buffer.handles.len() - self.handles_taken
    }
}

impl Transport for Channel {
    type Error = Status;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let shared = Arc::new(Shared::new(self));
        (Sender { shared: shared.clone() }, Receiver { shared })
    }

    type Sender = Sender;
    type SendBuffer = Buffer;
    type Encoder<'b> = &'b mut Buffer;
    type SendFuture<'s> = SendFuture<'s>;

    fn acquire(_: &Self::Sender) -> Self::SendBuffer {
        Buffer::new()
    }

    fn encoder(buffer: &mut Self::SendBuffer) -> Self::Encoder<'_> {
        buffer
    }

    fn send(sender: &Self::Sender, buffer: Self::SendBuffer) -> Self::SendFuture<'_> {
        SendFuture { shared: &sender.shared, buffer }
    }

    fn close(sender: &Self::Sender) {
        sender.shared.close();
    }

    type Receiver = Receiver;
    type RecvFuture<'r> = RecvFuture<'r>;
    type RecvBuffer = RecvBuffer;
    type Decoder<'b> = &'b mut RecvBuffer;

    fn recv(receiver: &mut Self::Receiver) -> Self::RecvFuture<'_> {
        RecvFuture { shared: &receiver.shared, buffer: Some(Buffer::new()) }
    }

    fn decoder(buffer: &mut Self::RecvBuffer) -> Self::Decoder<'_> {
        buffer
    }
}

#[cfg(test)]
mod tests {
    use fuchsia_async as fasync;
    use zx::Channel;

    use crate::testing::transport::*;

    #[fasync::run_singlethreaded(test)]
    async fn close_on_drop() {
        let (client_end, server_end) = Channel::create();
        test_close_on_drop(client_end, server_end).await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn one_way() {
        let (client_end, server_end) = Channel::create();
        test_one_way(client_end, server_end).await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn two_way() {
        let (client_end, server_end) = Channel::create();
        test_two_way(client_end, server_end).await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn multiple_two_way() {
        let (client_end, server_end) = Channel::create();
        test_multiple_two_way(client_end, server_end).await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn event() {
        let (client_end, server_end) = Channel::create();
        test_event(client_end, server_end).await;
    }
}
