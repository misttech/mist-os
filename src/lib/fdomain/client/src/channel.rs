// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::handle::handle_type;
use crate::responder::Responder;
use crate::{ordinals, Error, Event, Eventpair, Handle, Socket};
use fidl_fuchsia_fdomain as proto;
use fidl_fuchsia_fdomain_ext::AsFDomainRights;
use futures::channel::mpsc::UnboundedReceiver;
use futures::future::Either;
use futures::stream::Stream;
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{ready, Context, Poll};

/// A channel in a remote FDomain.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(pub(crate) Handle);

handle_type!(Channel peered);

/// A message which has been read from a channel.
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
                        rights: convert_rights_truncate(info.rights),
                        handle: match info.type_ {
                            proto::ObjType::Channel => AnyHandle::Channel(Channel(handle)),
                            proto::ObjType::Socket => AnyHandle::Socket(Socket(handle)),
                            proto::ObjType::Event => AnyHandle::Event(Event(handle)),
                            proto::ObjType::Eventpair => AnyHandle::EventPair(Eventpair(handle)),
                            _ => AnyHandle::Unknown(handle, convert_object_type(info.type_)),
                        },
                    }
                })
                .collect(),
        }
    }
}

/// A handle which has been read from a channel.
pub struct HandleInfo {
    pub handle: AnyHandle,
    pub rights: fidl::Rights,
}

/// Sum type of all the handle types which can be read from a channel. Allows
/// the user to learn the type of a handle after it has been read.
pub enum AnyHandle {
    Channel(Channel),
    Socket(Socket),
    Event(Event),
    EventPair(Eventpair),
    Unknown(Handle, fidl::ObjectType),
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
        async move {
            let client = client?;
            client
                .transaction(
                    ordinals::READ_CHANNEL,
                    proto::ChannelReadChannelRequest { handle },
                    Responder::ReadChannel,
                )
                .map(|f| f.map(|message| ChannelMessage::from_proto(&client, message)))
                .await
        }
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
                            rights: rights
                                .as_fdomain_rights()
                                .ok_or(Error::ProtocolRightsIncompatible)?,
                        })
                    } else {
                        Err(Error::ConnectionMismatch)
                    }
                }
                HandleOp::Duplicate(x, rights) => {
                    if Weak::ptr_eq(&x.client, &self.0.client) {
                        Ok(proto::HandleDisposition {
                            handle: proto::HandleOp::Duplicate(x.proto()),
                            rights: rights
                                .as_fdomain_rights()
                                .ok_or(Error::ProtocolRightsIncompatible)?,
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

        async move {
            client?
                .transaction(
                    ordinals::WRITE_CHANNEL,
                    proto::ChannelWriteChannelRequest { handle, data, handles },
                    move |x| Responder::WriteChannel(x, handle),
                )
                .await
        }
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
}

/// A stream of data issuing from a socket.
pub struct ChannelMessageStream {
    channel: Arc<Channel>,
    messages: UnboundedReceiver<Result<proto::ChannelMessage, proto::Error>>,
}

impl Stream for ChannelMessageStream {
    type Item = ChannelMessage;
    fn poll_next(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(Pin::new(&mut self.messages).poll_next(ctx)) {
            Some(Ok(c)) => {
                if let Ok(client) = self.channel.0.client() {
                    Poll::Ready(Some(ChannelMessage::from_proto(&client, c)))
                } else {
                    Poll::Ready(None)
                }
            }
            // TODO: Log?
            Some(Err(_)) | Option::None => Poll::Ready(None),
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

/// Convert a `proto::ObjType` to a `fidl::ObjectType`
fn convert_object_type(ty: proto::ObjType) -> fidl::ObjectType {
    match ty {
        proto::ObjType::None => fidl::ObjectType::NONE,
        proto::ObjType::Process => fidl::ObjectType::PROCESS,
        proto::ObjType::Thread => fidl::ObjectType::THREAD,
        proto::ObjType::Vmo => fidl::ObjectType::VMO,
        proto::ObjType::Channel => fidl::ObjectType::CHANNEL,
        proto::ObjType::Event => fidl::ObjectType::EVENT,
        proto::ObjType::Port => fidl::ObjectType::PORT,
        proto::ObjType::Interrupt => fidl::ObjectType::INTERRUPT,
        proto::ObjType::PciDevice => fidl::ObjectType::PCI_DEVICE,
        proto::ObjType::Debuglog => fidl::ObjectType::DEBUGLOG,
        proto::ObjType::Socket => fidl::ObjectType::SOCKET,
        proto::ObjType::Resource => fidl::ObjectType::RESOURCE,
        proto::ObjType::Eventpair => fidl::ObjectType::EVENTPAIR,
        proto::ObjType::Job => fidl::ObjectType::JOB,
        proto::ObjType::Vmar => fidl::ObjectType::VMAR,
        proto::ObjType::Fifo => fidl::ObjectType::FIFO,
        proto::ObjType::Guest => fidl::ObjectType::GUEST,
        proto::ObjType::Vcpu => fidl::ObjectType::VCPU,
        proto::ObjType::Timer => fidl::ObjectType::TIMER,
        proto::ObjType::Iommu => fidl::ObjectType::IOMMU,
        proto::ObjType::Bti => fidl::ObjectType::BTI,
        proto::ObjType::Profile => fidl::ObjectType::PROFILE,
        proto::ObjType::Pmt => fidl::ObjectType::PMT,
        proto::ObjType::SuspendToken => fidl::ObjectType::SUSPEND_TOKEN,
        proto::ObjType::Pager => fidl::ObjectType::PAGER,
        proto::ObjType::Exception => fidl::ObjectType::EXCEPTION,
        proto::ObjType::Clock => fidl::ObjectType::CLOCK,
        proto::ObjType::Stream => fidl::ObjectType::STREAM,
        proto::ObjType::Msi => fidl::ObjectType::MSI,
        proto::ObjType::Iob => fidl::ObjectType::IOB,
        proto::ObjType::__SourceBreaking { .. } => fidl::ObjectType::NONE,
    }
}

/// Convert a `proto::Rights` to a `fidl::Rights`. If any rights aren't
/// understood they are discarded.
fn convert_rights_truncate(mut rights: proto::Rights) -> fidl::Rights {
    let mut ret = fidl::Rights::empty();

    for (fidl_right, proto_right) in [
        (fidl::Rights::DUPLICATE, proto::Rights::DUPLICATE),
        (fidl::Rights::TRANSFER, proto::Rights::TRANSFER),
        (fidl::Rights::READ, proto::Rights::READ),
        (fidl::Rights::WRITE, proto::Rights::WRITE),
        (fidl::Rights::EXECUTE, proto::Rights::EXECUTE),
        (fidl::Rights::MAP, proto::Rights::MAP),
        (fidl::Rights::GET_PROPERTY, proto::Rights::GET_PROPERTY),
        (fidl::Rights::SET_PROPERTY, proto::Rights::SET_PROPERTY),
        (fidl::Rights::ENUMERATE, proto::Rights::ENUMERATE),
        (fidl::Rights::DESTROY, proto::Rights::DESTROY),
        (fidl::Rights::SET_POLICY, proto::Rights::SET_POLICY),
        (fidl::Rights::GET_POLICY, proto::Rights::GET_POLICY),
        (fidl::Rights::SIGNAL, proto::Rights::SIGNAL),
        (fidl::Rights::SIGNAL_PEER, proto::Rights::SIGNAL_PEER),
        (fidl::Rights::WAIT, proto::Rights::WAIT),
        (fidl::Rights::INSPECT, proto::Rights::INSPECT),
        (fidl::Rights::MANAGE_JOB, proto::Rights::MANAGE_JOB),
        (fidl::Rights::MANAGE_PROCESS, proto::Rights::MANAGE_PROCESS),
        (fidl::Rights::MANAGE_THREAD, proto::Rights::MANAGE_THREAD),
        (fidl::Rights::APPLY_PROFILE, proto::Rights::APPLY_PROFILE),
        (fidl::Rights::MANAGE_SOCKET, proto::Rights::MANAGE_SOCKET),
        (fidl::Rights::OP_CHILDREN, proto::Rights::OP_CHILDREN),
        (fidl::Rights::RESIZE, proto::Rights::RESIZE),
        (fidl::Rights::ATTACH_VMO, proto::Rights::ATTACH_VMO),
        (fidl::Rights::MANAGE_VMO, proto::Rights::MANAGE_VMO),
        (fidl::Rights::SAME_RIGHTS, proto::Rights::SAME_RIGHTS),
    ] {
        if rights.contains(proto_right) {
            rights.remove(proto_right);
            ret |= fidl_right;
        }
    }

    ret
}
