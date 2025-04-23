// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A transport implementation which uses Zircon channels.

use core::mem::replace;
use core::pin::Pin;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Context, Poll};
use std::sync::Arc;

use fidl_next_codec::decoder::InternalHandleDecoder;
use fidl_next_codec::encoder::InternalHandleEncoder;
use fidl_next_codec::fuchsia::{HandleDecoder, HandleEncoder};
use fidl_next_codec::{Chunk, DecodeError, Decoder, EncodeError, Encoder, CHUNK_SIZE};
use fuchsia_async::{RWHandle, ReadableHandle as _};
use futures::task::AtomicWaker;
use zx::sys::{
    zx_channel_read, zx_channel_write, zx_handle_t, ZX_ERR_BUFFER_TOO_SMALL, ZX_ERR_PEER_CLOSED,
    ZX_ERR_SHOULD_WAIT, ZX_OK,
};
use zx::{AsHandleRef as _, Channel, Handle, HandleBased, Status};

use crate::{NonBlockingTransport, Transport};

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
#[derive(Default)]
pub struct Buffer {
    handles: Vec<Handle>,
    chunks: Vec<Chunk>,
}

impl Buffer {
    /// New buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve the handles.
    pub fn handles(&self) -> &[Handle] {
        &self.handles
    }

    /// Retrieve the bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.chunks.iter().flat_map(|chunk| chunk.to_le_bytes()).collect()
    }
}

impl InternalHandleEncoder for Buffer {
    #[inline]
    fn __internal_handle_count(&self) -> usize {
        self.handles.len()
    }
}

impl Encoder for Buffer {
    #[inline]
    fn bytes_written(&self) -> usize {
        Encoder::bytes_written(&self.chunks)
    }

    #[inline]
    fn write_zeroes(&mut self, len: usize) {
        Encoder::write_zeroes(&mut self.chunks, len)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        Encoder::write(&mut self.chunks, bytes)
    }

    #[inline]
    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        Encoder::rewrite(&mut self.chunks, pos, bytes)
    }
}

impl HandleEncoder for Buffer {
    fn push_handle(&mut self, handle: Handle) -> Result<(), EncodeError> {
        self.handles.push(handle);
        Ok(())
    }

    fn handles_pushed(&self) -> usize {
        self.handles.len()
    }
}

/// The state for a channel send future.
pub struct SendFutureState {
    buffer: Buffer,
}

/// A channel receiver.
pub struct Receiver {
    shared: Arc<Shared>,
}

/// The state for a channel receive future.
pub struct RecvFutureState {
    buffer: Option<Buffer>,
}

/// A channel receive buffer.
pub struct RecvBuffer {
    buffer: Buffer,
    chunks_taken: usize,
    handles_taken: usize,
}

unsafe impl Decoder for RecvBuffer {
    fn take_chunks_raw(&mut self, count: usize) -> Result<NonNull<Chunk>, DecodeError> {
        if count > self.buffer.chunks.len() - self.chunks_taken {
            return Err(DecodeError::InsufficientData);
        }

        let chunks = unsafe { self.buffer.chunks.as_mut_ptr().add(self.chunks_taken) };
        self.chunks_taken += count;

        unsafe { Ok(NonNull::new_unchecked(chunks)) }
    }

    fn commit(&mut self) {
        for handle in &mut self.buffer.handles[0..self.handles_taken] {
            // This handle was taken. To commit the current changes, we need to forget it.
            let _ = replace(handle, Handle::invalid()).into_raw();
        }
    }

    fn finish(&self) -> Result<(), DecodeError> {
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

impl InternalHandleDecoder for RecvBuffer {
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

impl HandleDecoder for RecvBuffer {
    fn take_raw_handle(&mut self) -> Result<zx_handle_t, DecodeError> {
        if self.handles_taken >= self.buffer.handles.len() {
            return Err(DecodeError::InsufficientHandles);
        }

        let handle = self.buffer.handles[self.handles_taken].raw_handle();
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
    type SendFutureState = SendFutureState;

    fn acquire(_: &Self::Sender) -> Self::SendBuffer {
        Buffer::new()
    }

    fn begin_send(_: &Self::Sender, buffer: Self::SendBuffer) -> Self::SendFutureState {
        SendFutureState { buffer }
    }

    fn poll_send(
        future_state: Pin<&mut Self::SendFutureState>,
        _: &mut Context<'_>,
        sender: &Self::Sender,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Self::send_immediately(future_state.get_mut(), sender))
    }

    fn close(sender: &Self::Sender) {
        sender.shared.close();
    }

    type Receiver = Receiver;
    type RecvFutureState = RecvFutureState;
    type RecvBuffer = RecvBuffer;

    fn begin_recv(_: &mut Self::Receiver) -> Self::RecvFutureState {
        RecvFutureState { buffer: Some(Buffer::new()) }
    }

    fn poll_recv(
        mut future_state: Pin<&mut Self::RecvFutureState>,
        cx: &mut Context<'_>,
        receiver: &mut Self::Receiver,
    ) -> Poll<Result<Option<Self::RecvBuffer>, Self::Error>> {
        let buffer = future_state.buffer.as_mut().unwrap();

        let mut actual_bytes = 0;
        let mut actual_handles = 0;

        loop {
            let result = unsafe {
                zx_channel_read(
                    receiver.shared.channel.get_ref().raw_handle(),
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
                        buffer: future_state.buffer.take().unwrap(),
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
                    if matches!(receiver.shared.channel.need_readable(cx)?, Poll::Pending) {
                        receiver.shared.closed_waker.register(cx.waker());
                        if receiver.shared.is_closed.load(Ordering::Relaxed) {
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

impl NonBlockingTransport for Channel {
    fn send_immediately(
        future_state: &mut Self::SendFutureState,
        sender: &Self::Sender,
    ) -> Result<(), Self::Error> {
        let result = unsafe {
            zx_channel_write(
                sender.shared.channel.get_ref().raw_handle(),
                0,
                future_state.buffer.chunks.as_ptr().cast::<u8>(),
                (future_state.buffer.chunks.len() * CHUNK_SIZE) as u32,
                future_state.buffer.handles.as_ptr().cast(),
                future_state.buffer.handles.len() as u32,
            )
        };

        if result == ZX_OK {
            // Handles were written to the channel, so we must not drop them.
            unsafe {
                future_state.buffer.handles.set_len(0);
            }
            Ok(())
        } else {
            Err(Status::from_raw(result))
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::MaybeUninit;

    use fidl_next_codec::fuchsia::{HandleDecoder, HandleEncoder, WireHandle};
    use fidl_next_codec::{
        munge, Decode, DecodeError, DecoderExt as _, Encodable, Encode, EncodeError,
        EncoderExt as _, Slot, WireString, ZeroPadding,
    };
    use fuchsia_async as fasync;
    use zx::{AsHandleRef, Channel, Handle, HandleBased as _, Instant, Signals, WaitResult};

    use crate::fuchsia::channel::{Buffer, RecvBuffer};
    use crate::testing::{
        test_close_on_drop, test_event, test_multiple_two_way, test_one_way, test_two_way,
    };
    use crate::{Client, Responder, Server, ServerHandler, ServerSender, Transport};

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

    struct HandleAndBoolean {
        handle: Handle,
        boolean: bool,
    }

    #[derive(Debug)]
    #[repr(C)]
    struct WireHandleAndBoolean {
        handle: WireHandle,
        boolean: bool,
    }

    unsafe impl ZeroPadding for WireHandleAndBoolean {
        fn zero_padding(out: &mut MaybeUninit<Self>) {
            unsafe {
                out.as_mut_ptr().write_bytes(0, 1);
            }
        }
    }

    impl Encodable for HandleAndBoolean {
        type Encoded = WireHandleAndBoolean;
    }

    unsafe impl<E: HandleEncoder + ?Sized> Encode<E> for HandleAndBoolean {
        fn encode(
            &mut self,
            encoder: &mut E,
            out: &mut MaybeUninit<Self::Encoded>,
        ) -> Result<(), EncodeError> {
            munge!(let Self::Encoded { handle, boolean } = out);
            Encode::encode(&mut self.handle, encoder, handle)?;
            Encode::encode(&mut self.boolean, encoder, boolean)?;
            Ok(())
        }
    }

    unsafe impl<D: HandleDecoder + ?Sized> Decode<D> for WireHandleAndBoolean {
        fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
            munge!(let Self { handle, boolean } = slot);
            Decode::decode(handle, decoder)?;
            Decode::decode(boolean, decoder)?;
            Ok(())
        }
    }

    #[test]
    fn partial_decode_drops_handles() {
        let (encode_end, check_end) = Channel::create();

        let mut buffer = Buffer::new();
        buffer
            .encode_next(&mut HandleAndBoolean { handle: encode_end.into_handle(), boolean: false })
            .expect("encoding should succeed");
        // Modify the buffer so that the boolean value is invalid
        *buffer.chunks[0] |= 0x00000002_00000000;

        let mut recv_buffer = RecvBuffer { buffer, chunks_taken: 0, handles_taken: 0 };
        (&mut recv_buffer)
            .decode_prefix::<WireHandleAndBoolean>()
            .expect_err("decoding an invalid boolean should fail");

        // Decoding failed, so the handle should still be in the buffer.
        assert_eq!(
            check_end.wait_handle(Signals::CHANNEL_PEER_CLOSED, Instant::INFINITE_PAST),
            WaitResult::TimedOut(Signals::CHANNEL_WRITABLE),
        );

        drop(recv_buffer);

        // The handle should have been dropped with the buffer.
        assert_eq!(
            check_end.wait_handle(Signals::CHANNEL_PEER_CLOSED, Instant::INFINITE_PAST),
            WaitResult::Ok(Signals::CHANNEL_PEER_CLOSED),
        );
    }

    #[test]
    fn complete_decode_moves_handles() {
        let (encode_end, check_end) = Channel::create();

        let mut buffer = Buffer::new();
        buffer
            .encode_next(&mut HandleAndBoolean { handle: encode_end.into_handle(), boolean: false })
            .expect("encoding should succeed");

        let recv_buffer = RecvBuffer { buffer, chunks_taken: 0, handles_taken: 0 };
        let decoded =
            recv_buffer.decode::<WireHandleAndBoolean>().expect("decoding should succeed");

        // The handle should remain un-signaled after successful decoding.
        assert_eq!(
            check_end.wait_handle(Signals::CHANNEL_PEER_CLOSED, Instant::INFINITE_PAST),
            WaitResult::TimedOut(Signals::CHANNEL_WRITABLE),
        );

        drop(decoded.handle.take());

        // Now the handle should be signaled.
        assert_eq!(
            check_end.wait_handle(Signals::CHANNEL_PEER_CLOSED, Instant::INFINITE_PAST),
            WaitResult::Ok(Signals::CHANNEL_PEER_CLOSED),
        );

        drop(decoded);
    }

    #[fasync::run_singlethreaded(test)]
    async fn one_way_nonblocking() {
        let (client_end, server_end) = Channel::create();
        struct TestServer;

        impl<T: Transport> ServerHandler<T> for TestServer {
            fn on_one_way(&mut self, _: &ServerSender<T>, ordinal: u64, buffer: T::RecvBuffer) {
                assert_eq!(ordinal, 42);
                let message = buffer.decode::<WireString>().expect("failed to decode request");
                assert_eq!(&**message, "Hello world");
            }

            fn on_two_way(&mut self, _: &ServerSender<T>, _: u64, _: T::RecvBuffer, _: Responder) {
                panic!("unexpected two-way message");
            }
        }

        let mut client = Client::new(client_end);
        let client_sender = client.sender().clone();
        let client_task = fasync::Task::spawn(async move { client.run_sender().await });
        let mut server = Server::new(server_end);
        let server_task = fasync::Task::spawn(async move { server.run(TestServer).await });

        client_sender
            .send_one_way(42, &mut "Hello world".to_string())
            .expect("client failed to encode request")
            .send_immediately()
            .expect("client failed to send request");
        client_sender.close();
        drop(client_sender);

        client_task.await.expect("client encountered an error");
        server_task.await.expect("server encountered an error");
    }
}
