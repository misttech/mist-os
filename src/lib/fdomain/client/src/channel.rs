// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::handle::handle_type;
use crate::responder::Responder;
use crate::{ordinals, Error, Event, EventPair, Handle, OnFDomainSignals, Socket};
use fidl_fuchsia_fdomain as proto;
use futures::channel::mpsc::UnboundedReceiver;
use futures::future::Either;
use futures::stream::{FusedStream, Stream};
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{ready, Context, Poll};

/// A channel in a remote FDomain.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(pub(crate) Handle);

handle_type!(Channel CHANNEL peered);

/// A message which has been read from a channel.
#[derive(Debug)]
pub struct ChannelMessage {
    pub bytes: Vec<u8>,
    pub handles: Vec<HandleInfo>,
}

impl ChannelMessage {
    /// Convert a proto ChannelMessage to a local ChannelMessage.
    fn from_proto(client: &Arc<crate::Client>, message: proto::ChannelMessage) -> ChannelMessage {
        let proto::ChannelMessage { data, handles } = message;
        ChannelMessage {
            bytes: data,
            handles: handles
                .into_iter()
                .map(|info| {
                    let handle = Handle { id: info.handle.id, client: Arc::downgrade(client) };
                    HandleInfo {
                        rights: info.rights,
                        handle: AnyHandle::from_handle(handle, info.type_),
                    }
                })
                .collect(),
        }
    }
}

/// A handle which has been read from a channel.
#[derive(Debug)]
pub struct HandleInfo {
    pub handle: AnyHandle,
    pub rights: fidl::Rights,
}

/// Sum type of all the handle types which can be read from a channel. Allows
/// the user to learn the type of a handle after it has been read.
#[derive(Debug)]
pub enum AnyHandle {
    Channel(Channel),
    Socket(Socket),
    Event(Event),
    EventPair(EventPair),
    Unknown(Handle, fidl::ObjectType),
}

impl AnyHandle {
    /// Construct an `AnyHandle` from a `Handle` and an object type.
    pub fn from_handle(handle: Handle, ty: fidl::ObjectType) -> AnyHandle {
        match ty {
            fidl::ObjectType::CHANNEL => AnyHandle::Channel(Channel(handle)),
            fidl::ObjectType::SOCKET => AnyHandle::Socket(Socket(handle)),
            fidl::ObjectType::EVENT => AnyHandle::Event(Event(handle)),
            fidl::ObjectType::EVENTPAIR => AnyHandle::EventPair(EventPair(handle)),
            _ => AnyHandle::Unknown(handle, ty),
        }
    }

    /// Get an `AnyHandle` wrapping an invalid handle.
    pub fn invalid() -> AnyHandle {
        AnyHandle::Unknown(Handle::invalid(), fidl::ObjectType::NONE)
    }

    /// Get the object type for a handle.
    pub fn object_type(&self) -> fidl::ObjectType {
        match self {
            AnyHandle::Channel(_) => fidl::ObjectType::CHANNEL,
            AnyHandle::Socket(_) => fidl::ObjectType::SOCKET,
            AnyHandle::Event(_) => fidl::ObjectType::EVENT,
            AnyHandle::EventPair(_) => fidl::ObjectType::EVENTPAIR,
            AnyHandle::Unknown(_, t) => *t,
        }
    }
}

impl From<AnyHandle> for Handle {
    fn from(item: AnyHandle) -> Handle {
        match item {
            AnyHandle::Channel(h) => h.into(),
            AnyHandle::Socket(h) => h.into(),
            AnyHandle::Event(h) => h.into(),
            AnyHandle::EventPair(h) => h.into(),
            AnyHandle::Unknown(h, _) => h,
        }
    }
}

/// Operation to perform on a handle when writing it to a channel.
pub enum HandleOp<'h> {
    Move(Handle, fidl::Rights),
    Duplicate(&'h Handle, fidl::Rights),
}

impl Channel {
    /// Reads a message from the channel.
    pub fn recv_msg(&self) -> impl Future<Output = Result<ChannelMessage, Error>> {
        let client = self.0.client();
        let handle = self.0.proto();

        let result = client.map(move |client| {
            client
                .transaction(
                    ordinals::READ_CHANNEL,
                    proto::ChannelReadChannelRequest { handle },
                    Responder::ReadChannel,
                )
                .map(move |f| f.map(|message| ChannelMessage::from_proto(&client, message)))
        });

        async move { result?.await }
    }

    /// Writes a message into the channel.
    pub fn write(
        &self,
        bytes: &[u8],
        handles: Vec<Handle>,
    ) -> impl Future<Output = Result<(), Error>> + '_ {
        self.write_inner(
            bytes,
            proto::Handles::Handles(handles.into_iter().map(|x| x.take_proto()).collect()),
        )
    }

    /// A future that returns when the channel is closed.
    pub fn on_closed(&self) -> OnFDomainSignals {
        OnFDomainSignals::new(&self.0, fidl::Signals::OBJECT_PEER_CLOSED)
    }

    /// Whether this handle is closed.
    pub fn is_closed(&self) -> bool {
        self.0.client.upgrade().is_none()
    }

    /// Writes a message into the channel.
    pub fn write_etc<'b>(
        &self,
        bytes: &[u8],
        handles: Vec<HandleOp<'b>>,
    ) -> impl Future<Output = Result<(), Error>> + 'b {
        let handles = handles
            .into_iter()
            .map(|handle| match handle {
                HandleOp::Move(x, rights) => {
                    if Weak::ptr_eq(&x.client, &self.0.client) {
                        Ok(proto::HandleDisposition {
                            handle: proto::HandleOp::Move_(x.take_proto()),
                            rights,
                        })
                    } else {
                        Err(Error::ConnectionMismatch)
                    }
                }
                HandleOp::Duplicate(x, rights) => {
                    if Weak::ptr_eq(&x.client, &self.0.client) {
                        Ok(proto::HandleDisposition {
                            handle: proto::HandleOp::Duplicate(x.proto()),
                            rights,
                        })
                    } else {
                        Err(Error::ConnectionMismatch)
                    }
                }
            })
            .collect::<Result<Vec<_>, Error>>();

        match handles {
            Ok(handles) => {
                Either::Left(self.write_inner(bytes, proto::Handles::Dispositions(handles)))
            }
            Err(e) => Either::Right(async move { Err(e) }),
        }
    }

    /// Writes a message into the channel.
    fn write_inner(
        &self,
        bytes: &[u8],
        handles: proto::Handles,
    ) -> impl Future<Output = Result<(), Error>> {
        let data = bytes.to_vec();
        let client = self.0.client();
        let handle = self.0.proto();

        let result = client.map(move |client| {
            client.transaction(
                ordinals::WRITE_CHANNEL,
                proto::ChannelWriteChannelRequest { handle, data, handles },
                move |x| Responder::WriteChannel(x, handle),
            )
        });

        async move { result?.await }
    }

    /// Split this channel into a streaming reader and a writer. This is more
    /// efficient on the read side if you intend to consume all of the messages
    /// from the channel. However it will prevent you from transferring the
    /// handle in the future. It also means messages will build up in the
    /// buffer, so it may lead to memory issues if you don't intend to use the
    /// messages from the channel as fast as they come.
    pub fn stream(self) -> Result<(ChannelMessageStream, ChannelWriter), Error> {
        let (sender, messages) = futures::channel::mpsc::unbounded();
        self.0.client()?.start_channel_streaming(self.0.proto(), sender)?;

        let a = Arc::new(self);
        let b = Arc::clone(&a);

        Ok((ChannelMessageStream { channel: a, messages }, ChannelWriter(b)))
    }
}

/// A write-only handle to a socket.
#[derive(Debug)]
pub struct ChannelWriter(Arc<Channel>);

impl ChannelWriter {
    /// Writes a message into the channel.
    pub fn write(
        &self,
        bytes: &[u8],
        handles: Vec<Handle>,
    ) -> impl Future<Output = Result<(), Error>> + '_ {
        self.0.write(bytes, handles)
    }

    /// Writes a message into the channel.
    pub fn write_etc<'b>(
        &self,
        bytes: &[u8],
        handles: Vec<HandleOp<'b>>,
    ) -> impl Future<Output = Result<(), Error>> + 'b {
        self.0.write_etc(bytes, handles)
    }

    /// Get a reference to the inner channel.
    pub fn as_channel(&self) -> &Channel {
        &*self.0
    }
}

/// A stream of data issuing from a socket.
#[derive(Debug)]
pub struct ChannelMessageStream {
    channel: Arc<Channel>,
    messages: UnboundedReceiver<Result<proto::ChannelMessage, Error>>,
}

impl ChannelMessageStream {
    /// Turn a `ChannelMessageStream` and its accompanying `ChannelWriter` back
    /// into a `Channel`.
    ///
    /// # Panics
    /// If this stream and the writer passed didn't come from the same call to
    /// `Channel::stream`.
    pub fn rejoin(
        mut self,
        writer: ChannelWriter,
    ) -> (Channel, UnboundedReceiver<Result<proto::ChannelMessage, Error>>) {
        assert!(Arc::ptr_eq(&self.channel, &writer.0), "Tried to join stream with wrong writer!");
        if let Ok(client) = self.channel.0.client() {
            client.stop_channel_streaming(self.channel.0.proto())
        }
        std::mem::drop(writer);
        let channel = std::mem::replace(&mut self.channel, Arc::new(Channel(Handle::invalid())));
        let (_, r) = futures::channel::mpsc::unbounded();
        let messages = std::mem::replace(&mut self.messages, r);
        (Arc::try_unwrap(channel).expect("Stream pointer no longer unique!"), messages)
    }

    /// Whether this stream is closed.
    pub fn is_closed(&self) -> bool {
        self.messages.is_terminated()
    }

    /// Get a reference to the inner channel.
    pub fn as_channel(&self) -> &Channel {
        &*self.channel
    }
}

impl Stream for ChannelMessageStream {
    type Item = Result<ChannelMessage, Error>;
    fn poll_next(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.messages).poll_next(ctx)) {
            Some(Ok(c)) => {
                if let Ok(client) = self.channel.0.client() {
                    Poll::Ready(Some(Ok(ChannelMessage::from_proto(&client, c))))
                } else {
                    Poll::Ready(None)
                }
            }
            Some(Err(e)) => Poll::Ready(Some(Err(e))),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for ChannelMessageStream {
    fn drop(&mut self) {
        if let Ok(client) = self.channel.0.client() {
            client.stop_channel_streaming(self.channel.0.proto());
        }
    }
}
