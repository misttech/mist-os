// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{execution_scope::ExecutionScope, ProtocolsExt},
    async_trait::async_trait,
    fidl::{
        endpoints::{ControlHandle, ProtocolMarker, RequestStream, ServerEnd},
        epitaph::ChannelEpitaphExt,
    },
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::future::BoxFuture,
    std::{
        future::Future,
        ops::{Deref, DerefMut},
    },
};

/// Wraps the channel provided in the open methods and provide convenience methods for sending
/// appropriate responses.  It also records actions that should be taken upon successful connection
/// such as truncating file objects.
pub struct ObjectRequest {
    // The channel.
    object_request: zx::Channel,

    // What should be sent first.
    send: ObjectRequestSend,

    // Attributes requested in the open method.
    attributes: fio::NodeAttributesQuery,

    /// Truncate the object before use.
    pub truncate: bool,
}

impl ObjectRequest {
    pub(crate) fn new(
        object_request: zx::Channel,
        send: ObjectRequestSend,
        attributes: fio::NodeAttributesQuery,
        truncate: bool,
    ) -> Self {
        Self { object_request, send, attributes, truncate }
    }

    /// Returns the request stream after sending requested information.
    pub async fn into_request_stream<T: Representation>(
        self,
        connection: &T,
    ) -> Result<<T::Protocol as ProtocolMarker>::RequestStream, zx::Status> {
        let stream = <fio::NodeMarker as ProtocolMarker>::RequestStream::from_channel(
            fasync::Channel::from_channel(self.object_request)?,
        );
        match self.send {
            ObjectRequestSend::OnOpen => {
                let control_handle = stream.control_handle();
                let node_info = connection.node_info().await.map_err(|s| {
                    control_handle.shutdown_with_epitaph(s);
                    s
                })?;
                control_handle
                    .send_on_open_(zx::Status::OK.into_raw(), Some(node_info))
                    .map_err(|_| zx::Status::PEER_CLOSED)?;
            }
            ObjectRequestSend::OnRepresentation => {
                let control_handle = stream.control_handle();
                let representation =
                    connection.get_representation(self.attributes).await.map_err(|s| {
                        control_handle.shutdown_with_epitaph(s);
                        s
                    })?;
                control_handle
                    .send_on_representation(representation)
                    .map_err(|_| zx::Status::PEER_CLOSED)?;
            }
            ObjectRequestSend::Nothing => {}
        }
        Ok(stream.cast_stream())
    }

    /// Converts to ServerEnd<T>.
    pub fn into_server_end<T>(self) -> ServerEnd<T> {
        ServerEnd::new(self.object_request)
    }

    /// Extracts the channel.
    pub fn into_channel(self) -> zx::Channel {
        self.object_request
    }

    /// Terminates the object request with the given status.
    pub fn shutdown(self, status: zx::Status) {
        if let ObjectRequestSend::OnOpen = self.send {
            if let Ok((_, control_handle)) = ServerEnd::<fio::NodeMarker>::new(self.object_request)
                .into_stream_and_control_handle()
            {
                let _ = control_handle.send_on_open_(status.into_raw(), None);
                control_handle.shutdown_with_epitaph(status);
            }
        } else {
            let _ = self.object_request.close_with_epitaph(status);
        }
    }

    /// Calls `f` and sends an error on the object request channel upon failure.
    pub fn handle<T>(self, f: impl FnOnce(ObjectRequestRef) -> Result<T, zx::Status>) -> Option<T> {
        let mut request = Some(self);
        match f(ObjectRequestRef(&mut request)) {
            Err(s) => {
                if let Some(r) = request {
                    r.shutdown(s);
                }
                None
            }
            Ok(o) => Some(o),
        }
    }

    /// Spawn a task for the object request.  The callback returns a future that can return a
    /// zx::Status which will be handled appropriately.  If the future succeeds it should return
    /// another future that is responsible for the long term servicing of the object request.  This
    /// is done to avoid paying the stack cost of the object request for the lifetime of the
    /// connection.
    ///
    /// For example:
    ///
    ///   object_request.spawn(
    ///       move |object_request| async move {
    ///           // Perform checks on the new connection
    ///           if !valid(...) {
    ///               return Err(zx::Status::INVALID_ARGS);
    ///           }
    ///           // Upon success, return a future that handles the connection.
    ///           let requests = object_request.take().into_request_stream();
    ///           Ok(async {
    ///                  while let request = requests.next().await {
    ///                      ...
    ///                  }
    ///              })
    ///       });
    ///
    pub fn spawn<F, Fut>(self, scope: &ExecutionScope, f: F)
    where
        for<'a> F:
            FnOnce(ObjectRequestRef<'a>) -> BoxFuture<'a, Result<Fut, zx::Status>> + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        scope.spawn(async {
            // This avoids paying the stack cost for ObjectRequest for the lifetime of the task.
            let fut = {
                let mut object_request = Some(self);
                match f(ObjectRequestRef(&mut object_request)).await {
                    Err(s) => {
                        if let Some(object_request) = object_request {
                            object_request.shutdown(s);
                        }
                        return;
                    }
                    Ok(fut) => fut,
                }
            };
            fut.await
        });
    }
}

/// Holds a reference to an ObjectRequest.
// Whilst it contains an option, it is guaranteed to always hold a request so it is safe to unwrap
// the Option.  It is designed this way for the benefit of `handle` and `spawn` above.
pub struct ObjectRequestRef<'a>(&'a mut Option<ObjectRequest>);

impl ObjectRequestRef<'_> {
    /// Take the ObjectRequest.  The caller is responsible for sending errors.
    pub fn take(self) -> ObjectRequest {
        self.0.take().unwrap()
    }
}

impl Deref for ObjectRequestRef<'_> {
    type Target = ObjectRequest;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl DerefMut for ObjectRequestRef<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}

pub(crate) enum ObjectRequestSend {
    OnOpen,
    OnRepresentation,
    Nothing,
}

#[async_trait]
/// Trait to get either fio::Representation or fio::NodeInfoDeprecated.  Connection types
/// should implement this.
pub trait Representation {
    /// The protocol used for the connection.
    type Protocol: ProtocolMarker;

    /// Returns io2's Representation for the object.
    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, zx::Status>;

    /// Returns io1's NodeInfoDeprecated.
    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, zx::Status>;
}

/// Trait for converting fio::ConnectionProtocols and fio::OpenFlags into ObjectRequest.
pub trait ToObjectRequest: ProtocolsExt {
    fn to_object_request(&self, object_request: impl Into<zx::Handle>) -> ObjectRequest;
}

impl ToObjectRequest for fio::ConnectionProtocols {
    fn to_object_request(&self, object_request: impl Into<zx::Handle>) -> ObjectRequest {
        ObjectRequest::new(
            object_request.into().into(),
            if self.get_representation() {
                ObjectRequestSend::OnRepresentation
            } else {
                ObjectRequestSend::Nothing
            },
            self.attributes(),
            self.is_truncate(),
        )
    }
}

impl ToObjectRequest for fio::OpenFlags {
    fn to_object_request(&self, object_request: impl Into<zx::Handle>) -> ObjectRequest {
        ObjectRequest::new(
            object_request.into().into(),
            if self.contains(fio::OpenFlags::DESCRIBE) {
                ObjectRequestSend::OnOpen
            } else {
                ObjectRequestSend::Nothing
            },
            self.attributes(),
            self.is_truncate(),
        )
    }
}
