// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::responder::Responder;
use crate::{ordinals, Client, Error};
use fidl_fuchsia_fdomain as proto;
use fidl_fuchsia_fdomain_ext::{
    proto_signals_to_fidl_take, AsFDomainObjectType, AsFDomainRights, AsFDomainSignals,
};
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

/// A handle of unspecified type within a remote FDomain.
#[derive(Debug)]
pub struct Handle {
    pub(crate) id: u32,
    pub(crate) client: Weak<Client>,
}

impl Handle {
    /// Get the FDomain client this handle belongs to.
    pub(crate) fn client(&self) -> Result<Arc<Client>, Error> {
        self.client.upgrade().ok_or(Error::ConnectionLost)
    }
}

impl std::cmp::PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Weak::ptr_eq(&self.client, &other.client)
    }
}

impl std::cmp::Eq for Handle {}

impl std::cmp::PartialOrd for Handle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for Handle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl std::hash::Hash for Handle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// A reference to a [`Handle`]. Can be derived from a [`Handle`] or any other
/// type implementing [`AsHandleRef`].
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HandleRef<'a>(&'a Handle);

impl std::ops::Deref for HandleRef<'_> {
    type Target = Handle;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl HandleRef<'_> {
    /// Replace this handle with a new handle to the same object, with different
    /// rights.
    pub fn duplicate(
        &self,
        rights: impl AsFDomainRights,
    ) -> impl Future<Output = Result<Handle, Error>> {
        let client = self.0.client();
        let handle = self.0.proto();
        let rights = rights.as_fdomain_rights();

        async move {
            let rights = rights.ok_or(Error::ProtocolRightsIncompatible)?;
            let client = client?;
            let new_handle = client.new_hid();
            let id = new_handle.id;
            client
                .transaction(
                    ordinals::DUPLICATE,
                    proto::FDomainDuplicateRequest { handle, new_handle, rights },
                    Responder::Duplicate,
                )
                .await?;
            Ok(Handle { id, client: Arc::downgrade(&client) })
        }
    }

    /// Assert and deassert signals on this handle.
    pub fn fdomain_signal(
        &self,
        ty: impl AsFDomainObjectType + Send + Sync + 'static,
        set: impl AsFDomainSignals + Send + Sync + 'static,
        clear: impl AsFDomainSignals + Send + Sync + 'static,
    ) -> impl Future<Output = Result<(), Error>> {
        let client = self.client();
        let handle = self.proto();
        async move {
            let ty = ty.as_fdomain_object_type().ok_or(Error::ProtocolObjectTypeIncompatible)?;
            let set = set.as_fdomain_signals(ty).ok_or(Error::ProtocolSignalsIncompatible)?;
            let clear = clear.as_fdomain_signals(ty).ok_or(Error::ProtocolSignalsIncompatible)?;
            client?
                .transaction(
                    ordinals::SIGNAL,
                    proto::FDomainSignalRequest { handle, set, clear },
                    Responder::Signal,
                )
                .await
        }
    }
}

/// Trait for turning handle-based types into [`HandleRef`], and for handle
/// operations that can be performed on [`HandleRef`].
pub trait AsHandleRef {
    fn as_handle_ref(&self) -> HandleRef<'_>;
    fn object_type() -> impl AsFDomainObjectType + Send + Sync + 'static;

    fn fdomain_signal_handle(
        &self,
        set: impl AsFDomainSignals + Send + Sync + 'static,
        clear: impl AsFDomainSignals + Send + Sync + 'static,
    ) -> impl Future<Output = Result<(), Error>> {
        self.as_handle_ref().fdomain_signal(Self::object_type(), set, clear)
    }
}

impl AsHandleRef for Handle {
    /// Get a [`HandleRef`] referring to the handle contained in `Self`
    fn as_handle_ref(&self) -> HandleRef<'_> {
        HandleRef(self)
    }

    /// Get the object type of this handle.
    fn object_type() -> impl AsFDomainObjectType {
        proto::ObjType::None
    }
}

/// Trait for handle-based types that have a peer.
pub trait Peered: HandleBased {
    /// Assert and deassert signals on this handle's peer.
    fn fdomain_signal_peer(
        &self,
        ty: impl AsFDomainObjectType + Send + Sync + 'static,
        set: impl AsFDomainSignals + Send + Sync + 'static,
        clear: impl AsFDomainSignals + Send + Sync + 'static,
    ) -> impl Future<Output = Result<(), Error>> {
        let client = self.as_handle_ref().client();
        let handle = self.as_handle_ref().proto();
        async move {
            let ty = ty.as_fdomain_object_type().ok_or(Error::ProtocolObjectTypeIncompatible)?;
            let set = set.as_fdomain_signals(ty).ok_or(Error::ProtocolSignalsIncompatible)?;
            let clear = clear.as_fdomain_signals(ty).ok_or(Error::ProtocolSignalsIncompatible)?;
            client?
                .transaction(
                    ordinals::SIGNAL_PEER,
                    proto::FDomainSignalPeerRequest { handle, set, clear },
                    Responder::SignalPeer,
                )
                .await
        }
    }
}

pub trait HandleBased: AsHandleRef + From<Handle> + Into<Handle> {
    /// Closes this handle. Surfaces errors that dropping the handle will not.
    fn close(self) -> impl Future<Output = Result<(), Error>> {
        let h = <Self as Into<Handle>>::into(self);
        Handle::close(h)
    }

    /// Duplicate this handle.
    fn duplicate_handle(&self, rights: fidl::Rights) -> impl Future<Output = Result<Self, Error>> {
        let fut = self.as_handle_ref().duplicate(rights);
        async move { fut.await.map(|handle| Self::from(handle)) }
    }

    /// Replace this handle with an equivalent one with different rights.
    fn replace_handle(self, rights: fidl::Rights) -> impl Future<Output = Result<Self, Error>> {
        let h = <Self as Into<Handle>>::into(self);
        async move { h.replace(rights).await.map(|handle| Self::from(handle)) }
    }

    /// Convert this handle-based value into a pure [`Handle`].
    fn into_handle(self) -> Handle {
        self.into()
    }

    /// Construct a new handle-based value from a [`Handle`].
    fn from_handle(handle: Handle) -> Self {
        Self::from(handle)
    }

    /// Turn this handle-based value into one of a different type.
    fn into_handle_based<H: HandleBased>(self) -> H {
        H::from_handle(self.into_handle())
    }

    /// Turn another handle-based type into this one.
    fn from_handle_based<H: HandleBased>(h: H) -> Self {
        Self::from_handle(h.into_handle())
    }
}

impl HandleBased for Handle {}

/// Future which waits for a particular set of signals to be asserted for a
/// given handle.
pub struct OnFDomainSignals {
    fut: futures::future::BoxFuture<'static, Result<fidl::Signals, Error>>,
}

impl OnFDomainSignals {
    /// Construct a new [`OnFDomainSignals`]. The next time one of the given
    /// signals is asserted the future will return. The return value is all
    /// asserted signals intersected with the input signals.
    pub fn new(handle: &Handle, signals: proto::Signals) -> Self {
        let client = handle.client();
        let handle = handle.proto();
        OnFDomainSignals {
            fut: async move {
                client?
                    .transaction(
                        ordinals::WAIT_FOR_SIGNALS,
                        proto::FDomainWaitForSignalsRequest { handle, signals },
                        Responder::WaitForSignals,
                    )
                    .map(|f| f.map(|mut x| proto_signals_to_fidl_take(&mut x.signals)))
                    .await
            }
            .boxed(),
        }
    }
}

impl Future for OnFDomainSignals {
    type Output = Result<fidl::Signals, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().poll(cx)
    }
}

impl Handle {
    /// Get a proto::Hid with the ID of this handle.
    pub(crate) fn proto(&self) -> proto::Hid {
        proto::Hid { id: self.id }
    }

    /// Get a proto::Hid with the ID of this handle, then destroy this object
    /// without sending a request to close the handlel.
    pub(crate) fn take_proto(self) -> proto::Hid {
        let ret = self.proto();
        std::mem::forget(self);
        ret
    }

    /// Close this handle. Surfaces errors that dropping the handle will not.
    pub fn close(self) -> impl Future<Output = Result<(), Error>> {
        let client = self.client();
        async move {
            client?
                .transaction(
                    ordinals::CLOSE,
                    proto::FDomainCloseRequest { handles: vec![self.take_proto()] },
                    Responder::Close,
                )
                .await
        }
    }

    /// Replace this handle with a new handle to the same object, with different
    /// rights.
    pub async fn replace(self, rights: impl AsFDomainRights) -> Result<Handle, Error> {
        let client = self.client()?;
        let new_handle = client.new_hid();
        let id = new_handle.id;
        let rights = rights.as_fdomain_rights().ok_or(Error::ProtocolRightsIncompatible)?;
        client
            .transaction(
                ordinals::REPLACE,
                proto::FDomainReplaceRequest { handle: self.take_proto(), new_handle, rights },
                Responder::Replace,
            )
            .await?;

        Ok(Handle { id, client: Arc::downgrade(&client) })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Ok(client) = self.client() {
            client.0.lock().unwrap().waiting_to_close.push(self.proto());
        }
    }
}

macro_rules! handle_type {
    ($name:ident) => {
        impl From<$name> for Handle {
            fn from(other: $name) -> Handle {
                other.0
            }
        }

        impl From<Handle> for $name {
            fn from(other: Handle) -> $name {
                $name(other)
            }
        }

        impl $crate::AsHandleRef for $name {
            fn as_handle_ref(&self) -> $crate::HandleRef<'_> {
                self.0.as_handle_ref()
            }

            fn object_type(
            ) -> impl ::fidl_fuchsia_fdomain_ext::AsFDomainObjectType + Send + Sync + 'static {
                ::fidl_fuchsia_fdomain::ObjType::$name
            }
        }

        impl $crate::HandleBased for $name {}
    };
    ($name:ident peered) => {
        handle_type!($name);

        impl $crate::Peered for $name {}
    };
}

pub(crate) use handle_type;
