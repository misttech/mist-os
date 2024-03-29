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

/// Stores the name of the protocol for a channel handle. If the protocol isn't
/// known yet, stores the necessary shared state to eventually receive the
/// protocol via type coercion.
enum ProtocolState {
    Known(String),
    Coercible(Arc<Mutex<Option<String>>>),
}

impl ProtocolState {
    /// Set the protocol if it is not yet set, coercing the protocol of the
    /// opposite endpoint in the process. If the protocol is already set, this
    /// will return successfully if it is set to the protocol we expected, or
    /// fail otherwise.
    fn set_protocol(&mut self, protocol: String) -> Result<()> {
        match self {
            ProtocolState::Known(s) => {
                if *s == protocol {
                    Ok(())
                } else {
                    Err(Error::from(anyhow!("Protocol state already set")))
                }
            }
            ProtocolState::Coercible(s) => {
                let mut s = s.lock().unwrap();
                if let Some(s) = &*s {
                    if *s == protocol {
                        Ok(())
                    } else {
                        Err(Error::from(anyhow!("Protocol state already set")))
                    }
                } else {
                    *s = Some(protocol.clone());
                    std::mem::drop(s);
                    *self = ProtocolState::Known(protocol);
                    Ok(())
                }
            }
        }
    }
}

/// Stores an actual handle along with state information about the type of handle it is.
enum HandleObject {
    Handle(fidl::Handle, fidl::ObjectType),
    ClientEnd(fasync::Channel, ProtocolState),
    ServerEnd(fasync::Channel, ProtocolState),
    Defunct,
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
            handle: Arc::new(Mutex::new(HandleObject::ClientEnd(
                channel,
                ProtocolState::Known(identifier),
            ))),
        }
    }

    /// Get the type of this handle. Returns `None` if the handle has been consumed.
    pub fn object_type(&self) -> Option<fidl::ObjectType> {
        match &*self.handle.lock().unwrap() {
            HandleObject::Handle(_, ty) => Some(*ty),
            HandleObject::ClientEnd(_, _) | HandleObject::ServerEnd(_, _) => {
                Some(fidl::ObjectType::CHANNEL)
            }
            HandleObject::Defunct => None,
        }
    }

    /// Create a new [`InUseHandle`] for a server end channel.
    pub fn server_end(channel: fidl::Channel, identifier: String) -> Self {
        let channel = fasync::Channel::from_channel(channel);
        InUseHandle {
            handle: Arc::new(Mutex::new(HandleObject::ServerEnd(
                channel,
                ProtocolState::Known(identifier),
            ))),
        }
    }

    /// Create a new [`InUseHandle`] for an arbitrary handle.
    pub fn handle(handle: fidl::Handle, ty: fidl::ObjectType) -> Self {
        InUseHandle { handle: Arc::new(Mutex::new(HandleObject::Handle(handle, ty))) }
    }

    /// Create new client and server endd channel endpoints.
    pub fn new_endpoints() -> (Self, Self) {
        let protocol_name = Arc::new(Mutex::new(None));
        let (a, b) = fidl::Channel::create();
        let a = fasync::Channel::from_channel(a);
        let b = fasync::Channel::from_channel(b);
        (
            InUseHandle {
                handle: Arc::new(Mutex::new(HandleObject::ServerEnd(
                    a,
                    ProtocolState::Coercible(Arc::clone(&protocol_name)),
                ))),
            },
            InUseHandle {
                handle: Arc::new(Mutex::new(HandleObject::ClientEnd(
                    b,
                    ProtocolState::Coercible(protocol_name),
                ))),
            },
        )
    }

    /// If this handle is a client end channel with the given protocol, steal it
    /// and return a raw FIDL value wrapping it.
    pub fn take_client(&self, expect_proto: &str) -> Result<FidlValue> {
        let mut this = self.handle.lock().unwrap();
        match &mut *this {
            HandleObject::Handle(_, t) if *t == fidl::ObjectType::CHANNEL => {
                let HandleObject::Handle(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::ClientEnd(h.into(), expect_proto.to_owned()))
            }
            HandleObject::ClientEnd(_, s) => {
                s.set_protocol(expect_proto.to_owned())?;
                let HandleObject::ClientEnd(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::ClientEnd(h.into_zx_channel(), expect_proto.to_owned()))
            }
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle is closed"))),
            _ => Err(Error::from(anyhow!("Handle is not a client"))),
        }
    }

    /// If this handle is a server end channel with the given protocol, steal it
    /// and return a raw FIDL value wrapping it.
    pub fn take_server(&self, expect_proto: &str) -> Result<FidlValue> {
        let mut this = self.handle.lock().unwrap();
        match &mut *this {
            HandleObject::Handle(_, t) if *t == fidl::ObjectType::CHANNEL => {
                let HandleObject::Handle(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::ServerEnd(h.into(), expect_proto.to_owned()))
            }
            HandleObject::ServerEnd(_, s) => {
                s.set_protocol(expect_proto.to_owned())?;
                let HandleObject::ServerEnd(h, _) =
                    std::mem::replace(&mut *this, HandleObject::Defunct)
                else {
                    unreachable!();
                };
                Ok(FidlValue::ServerEnd(h.into_zx_channel(), expect_proto.to_owned()))
            }
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle is closed"))),
            _ => Err(Error::from(anyhow!("Handle is not a client"))),
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
        let this = self.handle.lock().unwrap();
        let hdl = match &*this {
            HandleObject::ClientEnd(ch, _) | HandleObject::ServerEnd(ch, _) => ch,
            HandleObject::Handle(_, _) => {
                return Poll::Ready(Err(Error::from(anyhow!("Raw channel reads unimplemented"))))
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
        let this = self.handle.lock().unwrap();
        let hdl = match &*this {
            HandleObject::ClientEnd(ch, _) | HandleObject::ServerEnd(ch, _) => ch,
            HandleObject::Handle(_, _) => {
                return Err(Error::from(anyhow!("Raw channel reads unimplemented")))
            }
            HandleObject::Defunct => return Err(Error::from(anyhow!("Channel was closed"))),
        };

        hdl.write_etc(bytes, handles).map_err(Into::into)
    }

    /// Get an ID for this handle (the raw handle number). Fails if the handle
    /// has been stolen.
    pub fn id(&self) -> Result<u32> {
        match &*self.handle.lock().unwrap() {
            HandleObject::ClientEnd(ch, _) | HandleObject::ServerEnd(ch, _) => Ok(ch.raw_handle()),
            HandleObject::Handle(h, _) => Ok(h.raw_handle()),
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle was closed"))),
        }
    }

    /// If this handle is a client, get the name of the protocol it is a client
    /// for if known.
    pub fn get_client_protocol(&self) -> Result<String> {
        match &*self.handle.lock().unwrap() {
            HandleObject::ClientEnd(_, proto) => {
                let proto = match proto {
                    ProtocolState::Known(s) => s.clone(),
                    ProtocolState::Coercible(s) => {
                        s.lock().unwrap().clone().ok_or_else(|| {
                            Error::from(anyhow!("Protocol for client end unknown"))
                        })?
                    }
                };
                Ok(proto)
            }
            HandleObject::Handle(_, _) | HandleObject::ServerEnd(_, _) => {
                Err(Error::from(anyhow!("Handle is not a client")))
            }
            HandleObject::Defunct => Err(Error::from(anyhow!("Handle was closed"))),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn coerce() {
        let (a, b) = InUseHandle::new_endpoints();
        a.take_server("test_proto").unwrap();
        assert_eq!("test_proto", &b.get_client_protocol().unwrap());
    }

    #[fuchsia::test]
    async fn read_and_write() {
        let (a, b) = InUseHandle::new_endpoints();
        let test_str =
            b"Alas that we are themepark animatronics, and our existence is inherently whimsical";
        a.write_channel_etc(test_str, &mut []).unwrap();
        let mut test_buf = fidl::MessageBufEtc::new();
        b.read_channel_etc(&mut test_buf).await.unwrap();
        assert_eq!(test_str, test_buf.bytes());
        assert!(test_buf.n_handle_infos() == 0);
    }
}
