// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::task::AtomicWaker;
use std::num::NonZero;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;

use fidl_next::Chunk;
use zx::Status;

use fdf_channel::arena::{Arena, ArenaBox};
use fdf_channel::channel::Channel;
use fdf_channel::futures::ReadMessageState;
use fdf_channel::message::Message;
use fdf_core::dispatcher::{CurrentDispatcher, OnDispatcher};
use fdf_core::handle::{MixedHandle, MixedHandleType};

/// A fidl-compatible driver channel that also holds a reference to the
/// dispatcher. Defaults to using [`CurrentDispatcher`].
pub struct DriverChannel<D = CurrentDispatcher> {
    dispatcher: D,
    channel: Channel<[Chunk]>,
}

impl<D> DriverChannel<D> {
    /// Create a new driver fidl channel that will perform its operations on the given
    /// dispatcher handle.
    pub fn new_with_dispatcher(dispatcher: D, channel: Channel<[Chunk]>) -> Self {
        Self { dispatcher, channel }
    }
}

impl DriverChannel<CurrentDispatcher> {
    /// Create a new driver fidl channel that will perform its operations on the
    /// [`CurrentDispatcher`].
    pub fn new(channel: Channel<[Chunk]>) -> Self {
        Self::new_with_dispatcher(CurrentDispatcher, channel)
    }
}

/// A channel buffer.
pub struct SendBuffer {
    handles: Vec<Option<MixedHandle>>,
    data: Vec<Chunk>,
}

impl SendBuffer {
    fn new() -> Self {
        Self { handles: Vec::new(), data: Vec::new() }
    }
}

impl fidl_next::Encoder for SendBuffer {
    #[inline]
    fn bytes_written(&self) -> usize {
        fidl_next::Encoder::bytes_written(&self.data)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        fidl_next::Encoder::write(&mut self.data, bytes)
    }

    #[inline]
    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        fidl_next::Encoder::rewrite(&mut self.data, pos, bytes)
    }

    fn write_zeroes(&mut self, len: usize) {
        fidl_next::Encoder::write_zeroes(&mut self.data, len);
    }
}

impl fidl_next::encoder::InternalHandleEncoder for SendBuffer {
    #[inline]
    fn __internal_handle_count(&self) -> usize {
        self.handles.len()
    }
}

impl fidl_next::fuchsia::HandleEncoder for SendBuffer {
    fn push_handle(&mut self, handle: zx::Handle) -> Result<(), fidl_next::EncodeError> {
        if let Some(handle) = MixedHandle::from_zircon_handle(handle) {
            if handle.is_driver() {
                return Err(fidl_next::EncodeError::ExpectedZirconHandle);
            }
            self.handles.push(Some(handle));
        } else {
            self.handles.push(None);
        }
        Ok(())
    }

    fn push_raw_driver_handle(&mut self, handle: u32) -> Result<(), fidl_next::EncodeError> {
        if let Some(handle) = NonZero::new(handle) {
            // SAFETY: the fidl framework is responsible for providing us with a valid, otherwise
            // unowned handle.
            let handle = unsafe { MixedHandle::from_raw(handle) };
            if !handle.is_driver() {
                return Err(fidl_next::EncodeError::ExpectedDriverHandle);
            }
            self.handles.push(Some(handle));
        } else {
            self.handles.push(None);
        }
        Ok(())
    }

    fn handles_pushed(&self) -> usize {
        self.handles.len()
    }
}

pub struct RecvBuffer {
    buffer: Message<[Chunk]>,
    data_offset: usize,
    handle_offset: usize,
}

impl RecvBuffer {
    fn next_handle(&self) -> Result<&MixedHandle, fidl_next::DecodeError> {
        let Some(handles) = self.buffer.handles() else {
            return Err(fidl_next::DecodeError::InsufficientHandles);
        };
        if handles.len() < self.handle_offset + 1 {
            return Err(fidl_next::DecodeError::InsufficientHandles);
        }
        handles[self.handle_offset].as_ref().ok_or(fidl_next::DecodeError::RequiredHandleAbsent)
    }
}

// SAFETY: The decoder implementation stores the data buffer in a [`Message`] tied to an [`Arena`],
// and the memory in an [`Arena`] is guaranteed not to move while the arena is valid.
// Also, since we own the [`Message`] and nothing else can, it is ok to treat its contents
// as mutable through an `&mut self` reference to the struct.
unsafe impl fidl_next::Decoder for RecvBuffer {
    // SAFETY: if the caller requests a number of [`Chunk`]s that we can't supply, we return
    // `InsufficientData`.
    fn take_chunks_raw(&mut self, count: usize) -> Result<NonNull<Chunk>, fidl_next::DecodeError> {
        let Some(data) = self.buffer.data_mut() else {
            return Err(fidl_next::DecodeError::InsufficientData);
        };
        if data.len() < self.data_offset + count {
            return Err(fidl_next::DecodeError::InsufficientData);
        }
        let pos = self.data_offset;
        self.data_offset += count;
        Ok(unsafe { NonNull::new_unchecked((&mut data[pos..(pos + count)]).as_mut_ptr()) })
    }

    fn commit(&mut self) {
        if let Some(handles) = self.buffer.handles_mut() {
            for i in 0..self.handle_offset {
                core::mem::forget(handles[i].take());
            }
        }
    }

    fn finish(&self) -> Result<(), fidl_next::DecodeError> {
        let data_len = self.buffer.data().unwrap_or(&[]).len();
        if self.data_offset != data_len {
            return Err(fidl_next::DecodeError::ExtraBytes {
                num_extra: data_len - self.data_offset,
            });
        }
        let handle_len = self.buffer.handles().unwrap_or(&[]).len();
        if self.handle_offset != handle_len {
            return Err(fidl_next::DecodeError::ExtraHandles {
                num_extra: handle_len - self.handle_offset,
            });
        }
        Ok(())
    }
}

impl fidl_next::decoder::InternalHandleDecoder for RecvBuffer {
    fn __internal_take_handles(&mut self, count: usize) -> Result<(), fidl_next::DecodeError> {
        let Some(handles) = self.buffer.handles_mut() else {
            return Err(fidl_next::DecodeError::InsufficientHandles);
        };
        if handles.len() < self.handle_offset + count {
            return Err(fidl_next::DecodeError::InsufficientHandles);
        }
        let pos = self.handle_offset;
        self.handle_offset = pos + count;
        Ok(())
    }

    fn __internal_handles_remaining(&self) -> usize {
        self.buffer.handles().unwrap_or(&[]).len() - self.handle_offset
    }
}

impl fidl_next::fuchsia::HandleDecoder for RecvBuffer {
    fn take_raw_handle(&mut self) -> Result<zx::sys::zx_handle_t, fidl_next::DecodeError> {
        let result = {
            let handle = self.next_handle()?.resolve_ref();
            let MixedHandleType::Zircon(handle) = handle else {
                return Err(fidl_next::DecodeError::ExpectedZirconHandle);
            };
            handle.raw_handle()
        };
        let pos = self.handle_offset;
        self.handle_offset = pos + 1;
        Ok(result)
    }

    fn take_raw_driver_handle(&mut self) -> Result<u32, fidl_next::DecodeError> {
        let result = {
            let handle = self.next_handle()?.resolve_ref();
            let MixedHandleType::Driver(handle) = handle else {
                return Err(fidl_next::DecodeError::ExpectedDriverHandle);
            };
            unsafe { handle.get_raw().get() }
        };
        let pos = self.handle_offset;
        self.handle_offset = pos + 1;
        Ok(result)
    }

    fn handles_remaining(&mut self) -> usize {
        self.buffer.handles().unwrap_or(&[]).len() - self.handle_offset
    }
}

/// The inner state of a receive future used by [`fidl_next::protocol::Transport`].
pub struct DriverRecvState(ReadMessageState);

struct Shared<D> {
    is_closed: AtomicBool,
    sender_count: AtomicUsize,
    closed_waker: AtomicWaker,
    channel: DriverChannel<D>,
}

impl<D> Shared<D> {
    fn new(channel: DriverChannel<D>) -> Self {
        Self {
            is_closed: AtomicBool::new(false),
            sender_count: AtomicUsize::new(1),
            closed_waker: AtomicWaker::new(),
            channel,
        }
    }

    fn close(&self) {
        self.is_closed.store(true, Ordering::Relaxed);
        self.closed_waker.wake();
    }
}
/// The sender side of a [`DriverChannel`].
pub struct DriverSender<D> {
    shared: Arc<Shared<D>>,
}

impl<D> Drop for DriverSender<D> {
    fn drop(&mut self) {
        let senders = self.shared.sender_count.fetch_sub(1, Ordering::Relaxed);
        if senders == 1 {
            self.shared.close();
        }
    }
}

impl<D> Clone for DriverSender<D> {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Ordering::Relaxed);
        Self { shared: self.shared.clone() }
    }
}

/// The receiver side of a [`DriverChannel`].
pub struct DriverReceiver<D> {
    shared: Arc<Shared<D>>,
}

impl<D: OnDispatcher> fidl_next::protocol::Transport for DriverChannel<D> {
    type Error = Status;

    fn split(self) -> (Self::Sender, Self::Receiver) {
        let shared = Arc::new(Shared::new(self));
        let sender = DriverSender { shared: shared.clone() };
        let receiver = DriverReceiver { shared };
        (sender, receiver)
    }

    type Sender = DriverSender<D>;

    type SendBuffer = SendBuffer;

    type SendFutureState = SendBuffer;

    fn acquire(_sender: &Self::Sender) -> Self::SendBuffer {
        SendBuffer::new()
    }

    fn close(sender: &Self::Sender) {
        sender.shared.close();
    }

    type Receiver = DriverReceiver<D>;

    type RecvFutureState = DriverRecvState;

    type RecvBuffer = RecvBuffer;

    fn begin_send(_sender: &Self::Sender, buffer: Self::SendBuffer) -> Self::SendFutureState {
        buffer
    }

    fn poll_send(
        mut buffer: std::pin::Pin<&mut Self::SendFutureState>,
        _cx: &mut std::task::Context<'_>,
        sender: &Self::Sender,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let arena = Arena::new();
        let message = Message::new_with(arena, |arena| {
            let data = arena.insert_slice(&buffer.data);
            let handles = buffer.handles.split_off(0);
            let handles = arena.insert_from_iter(handles.into_iter());
            (Some(data), Some(handles))
        });
        Poll::Ready(sender.shared.channel.channel.write(message))
    }

    fn begin_recv(receiver: &mut Self::Receiver) -> Self::RecvFutureState {
        // SAFETY: The `receiver` owns the channel we're using here and will be the same
        // receiver given to `poll_recv`, so must outlive the state object we're constructing.
        let state =
            unsafe { ReadMessageState::new(receiver.shared.channel.channel.driver_handle()) };
        DriverRecvState(state)
    }

    fn poll_recv(
        mut future: std::pin::Pin<&mut Self::RecvFutureState>,
        cx: &mut std::task::Context<'_>,
        receiver: &mut Self::Receiver,
    ) -> std::task::Poll<Result<Option<Self::RecvBuffer>, Self::Error>> {
        use std::task::Poll::*;
        match future.as_mut().0.poll_with_dispatcher(cx, receiver.shared.channel.dispatcher.clone())
        {
            Ready(Ok(Some(buffer))) => {
                let buffer = buffer.map_data(|_, data| {
                    let bytes = data.len();
                    assert_eq!(
                        0,
                        bytes % size_of::<Chunk>(),
                        "Received driver channel buffer was not a multiple of {} bytes",
                        size_of::<Chunk>()
                    );
                    // SAFETY: we verified that the size of the message we received was the correct
                    // multiple of chunks and we know that the data pointer is otherwise valid and
                    // from the correct arena by construction.
                    let new_box = unsafe {
                        let ptr = ArenaBox::into_ptr(data).cast();
                        ArenaBox::new(NonNull::slice_from_raw_parts(
                            ptr,
                            bytes / size_of::<Chunk>(),
                        ))
                    };
                    new_box
                });

                Ready(Ok(Some(RecvBuffer { buffer, data_offset: 0, handle_offset: 0 })))
            }
            Ready(Ok(None)) => Ready(Ok(None)),
            Ready(Err(err)) => Ready(Err(err)),
            Pending => {
                receiver.shared.closed_waker.register(cx.waker());
                if receiver.shared.is_closed.load(Ordering::Relaxed) {
                    return Poll::Ready(Ok(None));
                }
                Pending
            }
        }
    }
}

#[cfg(test)]
mod test {
    use fidl_next::{Client, ClientEnd, Responder, Server, ServerEnd, ServerSender};
    use fidl_next_fuchsia_examples_gizmo::device::{GetEvent, GetHardwareId};
    use fidl_next_fuchsia_examples_gizmo::{
        Device, DeviceClientHandler, DeviceClientSender, DeviceGetEventResponse,
        DeviceGetHardwareIdResponse, DeviceServerHandler,
    };
    use fuchsia_async::OnSignals;
    use zx::{AsHandleRef, Event, HandleBased, Signals};

    use super::*;
    use fdf_core::dispatcher::{CurrentDispatcher, OnDispatcher};
    use fdf_env::test::spawn_in_driver;

    struct DeviceServer;
    impl DeviceServerHandler<DriverChannel> for DeviceServer {
        fn get_hardware_id(
            &mut self,
            sender: &ServerSender<DriverChannel, Device>,
            responder: Responder<GetHardwareId>,
        ) {
            let sender = sender.clone();
            CurrentDispatcher
                .spawn_task(async move {
                    responder
                        .respond(
                            &sender,
                            &mut Result::<_, i32>::Ok(DeviceGetHardwareIdResponse {
                                response: 4004,
                            }),
                        )
                        .unwrap()
                        .await
                        .unwrap();
                })
                .unwrap();
        }

        fn get_event(
            &mut self,
            sender: &ServerSender<DriverChannel, Device>,
            responder: Responder<GetEvent>,
        ) {
            let sender = sender.clone();
            let event = Event::create();
            event.signal_handle(Signals::empty(), Signals::USER_0).unwrap();
            let mut response = DeviceGetEventResponse { event: event.into_handle() };
            CurrentDispatcher
                .spawn_task(async move {
                    responder.respond(&sender, &mut response).unwrap().await.unwrap();
                })
                .unwrap();
        }
    }

    struct DeviceClient;
    impl DeviceClientHandler<DriverChannel> for DeviceClient {}

    #[test]
    fn driver_fidl_server() {
        spawn_in_driver("driver fidl server", async {
            let (server_chan, client_chan) = Channel::<[Chunk]>::create();
            let client_end = ClientEnd::from_untyped(DriverChannel::new(client_chan));
            let server_end: ServerEnd<_, Device> =
                ServerEnd::from_untyped(DriverChannel::new(server_chan));
            let mut client = Client::new(client_end);
            let mut server = Server::new(server_end);
            let client_sender = client.sender().clone();

            CurrentDispatcher
                .spawn_task(async move {
                    server.run(DeviceServer).await.unwrap();
                    println!("server task finished");
                })
                .unwrap();
            CurrentDispatcher
                .spawn_task(async move {
                    client.run(DeviceClient).await.unwrap();
                    println!("client task finished");
                })
                .unwrap();

            {
                let res = client_sender.get_hardware_id().unwrap().await.unwrap();
                let hardware_id = res.unwrap();
                assert_eq!(hardware_id.response, 4004);
            }

            {
                let res = client_sender.get_event().unwrap().await.unwrap();
                let event = Event::from_handle(res.event.take());

                // wait for the event on a fuchsia_async executor
                let mut executor = fuchsia_async::LocalExecutor::new();
                let signalled =
                    executor.run_singlethreaded(OnSignals::new(event, Signals::USER_0)).unwrap();
                assert_eq!(Signals::USER_0, signalled);
            }
        });
    }
}
