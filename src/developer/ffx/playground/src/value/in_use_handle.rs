// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fidl::AsHandleRef;
use fidl_codec::Value as FidlValue;
use fuchsia_async as fasync;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use crate::error::{Error, Result};

pub enum Endpoint {
    Client(fasync::Channel, String),
    Server(fasync::Channel, String),
    Socket(fidl::Socket),
}

impl Endpoint {
    fn object_type(&self) -> fidl::ObjectType {
        match self {
            Endpoint::Client(_, _) | Endpoint::Server(_, _) => fidl::ObjectType::CHANNEL,
            Endpoint::Socket(_) => fidl::ObjectType::SOCKET,
        }
    }

    fn endpoint_type(&self) -> Option<(EndpointType, &str)> {
        match self {
            Endpoint::Client(_, p) => Some((EndpointType::Client, p)),
            Endpoint::Server(_, p) => Some((EndpointType::Server, p)),
            Endpoint::Socket(_) => None,
        }
    }
}

#[derive(PartialEq, Eq, Copy, Clone)]
enum EndpointType {
    Client,
    Server,
}

impl std::fmt::Display for EndpointType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointType::Client => write!(f, "client"),
            EndpointType::Server => write!(f, "server"),
        }
    }
}

impl EndpointType {
    fn opposite(&self) -> Self {
        match self {
            EndpointType::Client => EndpointType::Server,
            EndpointType::Server => EndpointType::Client,
        }
    }

    fn construct(&self, channel: fasync::Channel, protocol: String) -> Endpoint {
        match self {
            EndpointType::Client => Endpoint::Client(channel, protocol),
            EndpointType::Server => Endpoint::Server(channel, protocol),
        }
    }
}

/// Stores an actual handle along with state information about the type of handle it is.
enum HandleObject {
    Handle(fidl::Handle, fidl::ObjectType),
    Endpoint(Endpoint),
    Undetermined(Arc<Mutex<Option<Endpoint>>>),
    Defunct,
}

impl HandleObject {
    fn is_client_end(&mut self) -> bool {
        self.determine();
        matches!(self, HandleObject::Endpoint(Endpoint::Client(_, _)))
    }

    fn determine(&mut self) {
        if let HandleObject::Undetermined(this) = self {
            let got = this.lock().unwrap().take();
            if let Some(got) = got {
                *self = HandleObject::Endpoint(got);
            }
        }
    }
}

/// Represents a handle that is currently in use by the interpreter. Handles are
/// single-owner things normally, but the interpreter allows multiple access to
/// them. That access is coordinated here by surrounding the handle in a lock
/// and allowing patterns for changing the handle's type in response to type
/// coercion, or stealing the handle entirely if it is to be sent over the wire.
#[derive(Clone)]
pub struct InUseHandle {
    handle: Arc<Mutex<HandleObject>>,
}

impl InUseHandle {
    /// Create a new [`InUseHandle`] for a client end channel.
    pub fn client_end(channel: fidl::Channel, identifier: String) -> Self {
        let channel = fasync::Channel::from_channel(channel);
        InUseHandle {
            handle: Arc::new(Mutex::new(HandleObject::Endpoint(Endpoint::Client(
                channel, identifier,
            )))),
        }
    }

    /// Get the type of this handle. Returns `None` if the handle has been
    /// consumed, or if it is unconstructed due to insufficient type
    /// information.
    pub fn object_type(&self) -> Option<fidl::ObjectType> {
        match &*self.handle.lock().unwrap() {
            HandleObject::Handle(_, ty) => Some(*ty),
            HandleObject::Endpoint(e) => Some(e.object_type()),
            HandleObject::Undetermined(s) => s.lock().unwrap().as_ref().map(|s| s.object_type()),
            HandleObject::Defunct => None,
        }
    }

    /// Whether this is a client end handle.
    pub fn is_client_end(&self) -> bool {
        self.handle.lock().unwrap().is_client_end()
    }

    /// Create a new [`InUseHandle`] for a server end channel.
    pub fn server_end(channel: fidl::Channel, identifier: String) -> Self {
        let channel = fasync::Channel::from_channel(channel);
        InUseHandle {
            handle: Arc::new(Mutex::new(HandleObject::Endpoint(Endpoint::Server(
                channel, identifier,
            )))),
        }
    }

    /// Create a new [`InUseHandle`] for an arbitrary handle.
    pub fn handle(handle: fidl::Handle, ty: fidl::ObjectType) -> Self {
        InUseHandle { handle: Arc::new(Mutex::new(HandleObject::Handle(handle, ty))) }
    }

    /// Create new paired handle set. The handles could become channels or
    /// sockets depending on how they are used.
    pub fn new_endpoints() -> (Self, Self) {
        let endpoint = Arc::new(Mutex::new(None));
        (
            InUseHandle {
                handle: Arc::new(Mutex::new(HandleObject::Undetermined(Arc::clone(&endpoint)))),
            },
            InUseHandle { handle: Arc::new(Mutex::new(HandleObject::Undetermined(endpoint))) },
        )
    }

    /// If this handle is a client end channel with the given protocol, steal it
    /// and return a raw FIDL value wrapping it.
    pub fn take_client(&self, expect_proto: Option<&str>) -> Result<fidl::Channel> {
        self.take_endpoint(EndpointType::Client, expect_proto)
    }

    /// If this handle is a server end channel with the given protocol, steal it
    /// and return a raw FIDL value wrapping it.
    pub fn take_server(&self, expect_proto: Option<&str>) -> Result<fidl::Channel> {
        self.take_endpoint(EndpointType::Server, expect_proto)
    }

    /// If this handle is a channel with the given protocol and endpoint type, steal it
    /// and return a raw FIDL value wrapping it.
    fn take_endpoint(&self, ty: EndpointType, expect_proto: Option<&str>) -> Result<fidl::Channel> {
        let mut this = self.handle.lock().unwrap();
        this.determine();

        match &mut *this {
            HandleObject::Handle(_, t) if *t == fidl::ObjectType::CHANNEL => {
                let HandleObject::Handle(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(h.into())
            }
            HandleObject::Endpoint(e) => {
                let Some((got_ty, proto)) = e.endpoint_type() else {
                    return Err(Error::from(anyhow!("Handle is not a channel")));
                };
                let expect_proto_mismatched = expect_proto.filter(|x| *x != proto);
                if got_ty != ty && expect_proto_mismatched.is_some() {
                    let expect_proto = expect_proto_mismatched.unwrap();
                    return Err(Error::from(anyhow!(
                        "Handle is a {got_ty} for {proto} (need {ty} for {expect_proto})"
                    )));
                } else if got_ty != ty {
                    return Err(Error::from(anyhow!("Handle is a {got_ty} (need {ty})")));
                } else if let Some(expect_proto) = expect_proto_mismatched {
                    return Err(Error::from(anyhow!(
                        "Handle is a {got_ty} for {proto} (need {expect_proto})"
                    )));
                }
                let HandleObject::Endpoint(Endpoint::Server(h, _) | Endpoint::Client(h, _)) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(h.into())
            }
            HandleObject::Undetermined(e) => {
                let Some(expect_proto) = expect_proto else {
                    return Err(Error::from(anyhow!("Could not determine protocol for handle")));
                };
                let mut e = e.lock().unwrap();
                assert!(e.is_none());
                let (a, b) = fidl::Channel::create();
                let a = fasync::Channel::from_channel(a);
                *e = Some(ty.opposite().construct(a, expect_proto.to_owned()));
                drop(e);
                Ok(b)
            }
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle is closed"))),
            HandleObject::Handle(_, _) => Err(Error::from(anyhow!("Handle is not a {ty}"))),
        }
    }

    /// If this handle is a socket, steal it and return a raw FIDL value
    /// wrapping it.
    pub fn take_socket(&self) -> Result<FidlValue> {
        let mut this = self.handle.lock().unwrap();
        this.determine();
        match &mut *this {
            HandleObject::Handle(_, t) if *t == fidl::ObjectType::SOCKET => {
                let HandleObject::Handle(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::Handle(h.into(), fidl::ObjectType::SOCKET))
            }
            HandleObject::Endpoint(Endpoint::Socket(_)) => {
                let HandleObject::Endpoint(Endpoint::Socket(h)) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::Handle(h.into(), fidl::ObjectType::SOCKET))
            }
            HandleObject::Undetermined(e) => {
                let mut e = e.lock().unwrap();
                assert!(e.is_none());
                let (a, b) = fidl::Socket::create_stream();
                *e = Some(Endpoint::Socket(a));
                drop(e);
                Ok(FidlValue::Handle(b.into(), fidl::ObjectType::SOCKET))
            }
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle is closed"))),
            _ => Err(Error::from(anyhow!("Handle is not a socket"))),
        }
    }

    /// If this is a channel, perform a `read_etc` operation on it
    /// asynchronously. Returns an error if it is not a channel.
    pub fn poll_read_channel_etc(
        &self,
        ctx: &mut Context<'_>,
        bytes: &mut Vec<u8>,
        handles: &mut Vec<fidl::HandleInfo>,
    ) -> Poll<Result<()>> {
        let mut this = self.handle.lock().unwrap();
        this.determine();
        let hdl = match &*this {
            HandleObject::Endpoint(Endpoint::Client(ch, _) | Endpoint::Server(ch, _)) => ch,
            HandleObject::Handle(_, _) => {
                return Poll::Ready(Err(Error::from(anyhow!("Raw channel reads unimplemented"))))
            }
            HandleObject::Endpoint(_) => {
                return Poll::Ready(Err(Error::from(anyhow!("Handle is not a channel"))))
            }
            HandleObject::Undetermined(_) => {
                return Poll::Ready(Err(Error::from(anyhow!(
                    "Not enough type information to use handle as channel"
                ))))
            }
            HandleObject::Defunct => {
                return Poll::Ready(Err(Error::from(anyhow!("Channel was closed"))))
            }
        };

        hdl.read_etc(ctx, bytes, handles).map_err(Into::into)
    }

    /// If this is a channel, perform a `read_etc` operation on it within a
    /// future.
    pub async fn read_channel_etc(&self, buf: &mut fidl::MessageBufEtc) -> Result<()> {
        let (bytes, handles) = buf.split_mut();
        futures::future::poll_fn(|ctx| self.poll_read_channel_etc(ctx, bytes, handles)).await
    }

    /// If this is a channel, perform a `write_etc` operation on it. Returns an
    /// error if it is not a channel.
    pub fn write_channel_etc(
        &self,
        bytes: &[u8],
        handles: &mut [fidl::HandleDisposition<'_>],
    ) -> Result<()> {
        let mut this = self.handle.lock().unwrap();
        this.determine();
        let hdl = match &*this {
            HandleObject::Endpoint(Endpoint::Client(ch, _) | Endpoint::Server(ch, _)) => ch,
            HandleObject::Handle(_, _) => {
                return Err(Error::from(anyhow!("Raw channel writes unimplemented")))
            }
            HandleObject::Endpoint(_) => {
                return Err(Error::from(anyhow!("Handle is not a channel")))
            }
            HandleObject::Undetermined(_) => {
                return Err(Error::from(anyhow!(
                    "Not enough type information to use handle as channel"
                )))
            }
            HandleObject::Defunct => return Err(Error::from(anyhow!("Channel was closed"))),
        };

        hdl.write_etc(bytes, handles).map_err(Into::into)
    }

    /// Get an ID for this handle (the raw handle number). Fails if the handle
    /// has been stolen.
    pub fn id(&self) -> Result<u32> {
        let mut this = self.handle.lock().unwrap();
        this.determine();
        match &*this {
            HandleObject::Endpoint(Endpoint::Client(ch, _) | Endpoint::Server(ch, _)) => {
                Ok(ch.raw_handle())
            }
            HandleObject::Endpoint(Endpoint::Socket(s)) => Ok(s.raw_handle()),
            HandleObject::Undetermined(_) => {
                Err(Error::from(anyhow!("Insufficient type information to determine handle state")))
            }
            HandleObject::Handle(h, _) => Ok(h.raw_handle()),
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle was closed"))),
        }
    }

    /// If this handle is a client, get the name of the protocol it is a client
    /// for if known.
    pub fn get_client_protocol(&self) -> Result<String> {
        let mut this = self.handle.lock().unwrap();
        this.determine();
        match &*this {
            HandleObject::Endpoint(Endpoint::Client(_, proto)) => Ok(proto.clone()),
            HandleObject::Handle(_, _)
            | HandleObject::Endpoint(_)
            | HandleObject::Undetermined(_) => Err(Error::from(anyhow!("Handle is not a client"))),
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle was closed"))),
        }
    }
}

#[cfg(test)]
mod test {
    use futures::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[fuchsia::test]
    async fn coerce() {
        let (a, b) = InUseHandle::new_endpoints();
        let a = a.take_server(Some("test_proto")).unwrap();
        assert_eq!("test_proto", &b.get_client_protocol().unwrap());
        let test_str =
            b"Alas that we are themepark animatronics, and our existence is inherently whimsical";
        let a = fasync::Channel::from_channel(a);
        a.write_etc(test_str, &mut []).unwrap();
        let mut test_buf = fidl::MessageBufEtc::new();
        b.read_channel_etc(&mut test_buf).await.unwrap();
        assert_eq!(test_str, test_buf.bytes());
        assert!(test_buf.n_handle_infos() == 0);
    }

    #[fuchsia::test]
    async fn coerce_socket() {
        let (a, b) = InUseHandle::new_endpoints();
        let FidlValue::Handle(a, a_ty) = a.take_socket().unwrap() else {
            panic!();
        };
        assert_eq!(fidl::ObjectType::SOCKET, a_ty);
        const TEST_STR: &[u8] = b"Why were we programmed to get bored anyway?";
        let mut a = fasync::Socket::from_socket(a.into());
        fasync::Task::spawn(async move {
            a.write_all(TEST_STR).await.unwrap();
        })
        .detach();

        let FidlValue::Handle(b, b_ty) = b.take_socket().unwrap() else {
            panic!();
        };
        assert_eq!(fidl::ObjectType::SOCKET, b_ty);
        let mut b = fasync::Socket::from_socket(b.into());
        let mut buf = Vec::with_capacity(TEST_STR.len());
        let got = b.read_to_end(&mut buf).await.unwrap();
        assert_eq!(TEST_STR.len(), got);
        assert_eq!(TEST_STR, &buf);
    }
}
