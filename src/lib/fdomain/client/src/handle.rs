// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::responder::Responder;
use crate::{ordinals, Client, Error};
use fidl_fuchsia_fdomain as proto;
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

    /// Get an invalid handle.
    pub(crate) fn invalid() -> Self {
        Handle { id: 0, client: Weak::new() }
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
        rights: fidl::Rights,
    ) -> impl Future<Output = Result<Handle, Error>> + 'static {
        let client = self.0.client();
        let handle = self.0.proto();
        let result = client.map(|client| {
            let new_handle = client.new_hid();
            let id = new_handle.id;
            client
                .transaction(
                    ordinals::DUPLICATE,
                    proto::FDomainDuplicateRequest { handle, new_handle, rights },
                    Responder::Duplicate,
                )
                .map(move |res| res.map(|_| Handle { id, client: Arc::downgrade(&client) }))
        });

        async move { result?.await }
    }

    /// Assert and deassert signals on this handle.
    pub fn signal(
        &self,
        set: fidl::Signals,
        clear: fidl::Signals,
    ) -> impl Future<Output = Result<(), Error>> {
        let handle = self.proto();
        let client = self.client();

        let result: Result<_, Error> = client.map(|client| {
            client.transaction(
                ordinals::SIGNAL,
                proto::FDomainSignalRequest { handle, set: set.bits(), clear: clear.bits() },
                Responder::Signal,
            )
        });

        async move { result?.await }
    }
}

/// Trait for turning handle-based types into [`HandleRef`], and for handle
/// operations that can be performed on [`HandleRef`].
pub trait AsHandleRef {
    fn as_handle_ref(&self) -> HandleRef<'_>;
    fn object_type() -> fidl::ObjectType;

    fn signal_handle(
        &self,
        set: fidl::Signals,
        clear: fidl::Signals,
    ) -> impl Future<Output = Result<(), Error>> {
        self.as_handle_ref().signal(set, clear)
    }

    /// Get the client supporting this handle.
    fn client(&self) -> Result<Arc<Client>, Error> {
        self.as_handle_ref().0.client.upgrade().ok_or(Error::ConnectionLost)
    }
}

impl AsHandleRef for Handle {
    /// Get a [`HandleRef`] referring to the handle contained in `Self`
    fn as_handle_ref(&self) -> HandleRef<'_> {
        HandleRef(self)
    }

    /// Get the object type of this handle.
    fn object_type() -> fidl::ObjectType {
        fidl::ObjectType::NONE
    }
}

/// Trait for handle-based types that have a peer.
pub trait Peered: HandleBased {
    /// Assert and deassert signals on this handle's peer.
    fn signal_peer(
        &self,
        set: fidl::Signals,
        clear: fidl::Signals,
    ) -> impl Future<Output = Result<(), Error>> {
        let handle = self.as_handle_ref().proto();
        let client = self.as_handle_ref().client();

        let result: Result<_, Error> = client.map(|client| {
            client.transaction(
                ordinals::SIGNAL_PEER,
                proto::FDomainSignalPeerRequest { handle, set: set.bits(), clear: clear.bits() },
                Responder::SignalPeer,
            )
        });

        async move { result?.await }
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
    pub fn new(handle: &Handle, signals: fidl::Signals) -> Self {
        let client = handle.client();
        let handle = handle.proto();
        let result = client.map(|client| {
            client
                .transaction(
                    ordinals::WAIT_FOR_SIGNALS,
                    proto::FDomainWaitForSignalsRequest { handle, signals: signals.bits() },
                    Responder::WaitForSignals,
                )
                .map(|f| f.map(|x| x.signals))
        });
        OnFDomainSignals {
            fut: async move { result?.await.map(fidl::Signals::from_bits_retain) }.boxed(),
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
    pub(crate) fn take_proto(mut self) -> proto::Hid {
        let ret = self.proto();
        // Detach from the client so we don't close the handle when we drop self.
        self.client = Weak::new();
        ret
    }

    /// Close this handle. Surfaces errors that dropping the handle will not.
    pub fn close(self) -> impl Future<Output = Result<(), Error>> {
        let client = self.client();
        let result = client.map(|client| {
            client.transaction(
                ordinals::CLOSE,
                proto::FDomainCloseRequest { handles: vec![self.take_proto()] },
                Responder::Close,
            )
        });
        async move { result?.await }
    }

    /// Replace this handle with a new handle to the same object, with different
    /// rights.
    pub fn replace(self, rights: fidl::Rights) -> impl Future<Output = Result<Handle, Error>> {
        let params = (move || {
            let client = self.client()?;
            let handle = self.take_proto();
            let new_handle = client.new_hid();
            Ok((new_handle, handle, client))
        })();

        let result: Result<_, Error> = params.map(|(new_handle, handle, client)| {
            let id = new_handle.id;
            let ret = Handle { id, client: Arc::downgrade(&client) };
            (
                client.transaction(
                    ordinals::REPLACE,
                    proto::FDomainReplaceRequest { handle, new_handle, rights },
                    Responder::Replace,
                ),
                ret,
            )
        });

        async move {
            let (fut, ret) = result?;
            fut.await?;
            Ok(ret)
        }
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
    ($name:ident $objtype:ident) => {
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
            ) -> fidl::ObjectType {
                ::fidl::ObjectType::$objtype
            }
        }

        impl $crate::HandleBased for $name {}
    };
    ($name:ident $objtype:ident peered) => {
        handle_type!($name $objtype);

        impl $crate::Peered for $name {}
    };
}

pub(crate) use handle_type;
