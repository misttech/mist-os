// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::execution_scope::ExecutionScope;
use crate::node::{self, Node};
use crate::ProtocolsExt;
use fidl::endpoints::{ControlHandle, ProtocolMarker, RequestStream, ServerEnd};
use fidl::epitaph::ChannelEpitaphExt;
use fidl::{AsHandleRef, HandleBased};
use futures::future::BoxFuture;
use std::future::Future;
use std::sync::Arc;
use zx_status::Status;
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

/// Wraps the channel provided in the open methods and provide convenience methods for sending
/// appropriate responses.  It also records actions that should be taken upon successful connection
/// such as truncating file objects.
#[derive(Debug)]
pub struct ObjectRequest {
    // The channel.
    object_request: fidl::Channel,

    // What should be sent first.
    what_to_send: ObjectRequestSend,

    // Attributes required in the open method.
    attributes: fio::NodeAttributesQuery,

    // Creation attributes.
    create_attributes: Option<Box<fio::MutableNodeAttributes>>,

    /// Truncate the object before use.
    pub truncate: bool,
}

impl ObjectRequest {
    pub(crate) fn new(
        object_request: fidl::Channel,
        what_to_send: ObjectRequestSend,
        attributes: fio::NodeAttributesQuery,
        create_attributes: Option<&fio::MutableNodeAttributes>,
        truncate: bool,
    ) -> Self {
        assert!(!object_request.is_invalid_handle());
        let create_attributes = create_attributes.map(|a| Box::new(a.clone()));
        Self { object_request, what_to_send, attributes, create_attributes, truncate }
    }

    pub fn new3(flags: fio::Flags, options: &fio::Options, object_request: fidl::Channel) -> Self {
        ObjectRequest::new(
            object_request,
            if flags.get_representation() {
                ObjectRequestSend::OnRepresentation
            } else {
                ObjectRequestSend::Nothing
            },
            options.attributes.unwrap_or(fio::NodeAttributesQuery::empty()),
            options.create_attributes.as_ref(),
            flags.is_truncate(),
        )
    }

    pub(crate) fn what_to_send(&self) -> ObjectRequestSend {
        self.what_to_send
    }

    pub fn attributes(&self) -> fio::NodeAttributesQuery {
        self.attributes
    }

    pub fn create_attributes(&self) -> Option<&fio::MutableNodeAttributes> {
        self.create_attributes.as_deref()
    }

    pub fn options(&self) -> fio::Options {
        fio::Options {
            attributes: (!self.attributes.is_empty()).then_some(self.attributes),
            create_attributes: self
                .create_attributes
                .as_ref()
                .map(|a| fio::MutableNodeAttributes::clone(&a)),
            ..Default::default()
        }
    }

    /// Returns the request stream after sending requested information.
    pub async fn into_request_stream<T: Representation>(
        self,
        connection: &T,
    ) -> Result<<T::Protocol as ProtocolMarker>::RequestStream, Status> {
        let stream = fio::NodeRequestStream::from_channel(fasync::Channel::from_channel(
            self.object_request,
        ));
        match self.what_to_send {
            ObjectRequestSend::OnOpen => {
                let control_handle = stream.control_handle();
                let node_info = connection.node_info().await.map_err(|s| {
                    control_handle.shutdown_with_epitaph(s);
                    s
                })?;
                send_on_open(&stream.control_handle(), node_info)?;
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
                    .map_err(|_| Status::PEER_CLOSED)?;
            }
            ObjectRequestSend::Nothing => {}
        }
        Ok(stream.cast_stream())
    }

    /// Converts to ServerEnd<T>.
    pub fn into_server_end<T>(self) -> ServerEnd<T> {
        ServerEnd::new(self.object_request)
    }

    /// Extracts the channel (without sending on_open).
    pub fn into_channel(self) -> fidl::Channel {
        self.object_request
    }

    /// Extracts the channel after sending on_open.
    pub fn into_channel_after_sending_on_open(
        self,
        node_info: fio::NodeInfoDeprecated,
    ) -> Result<fidl::Channel, Status> {
        let stream = fio::NodeRequestStream::from_channel(fasync::Channel::from_channel(
            self.object_request,
        ));
        send_on_open(&stream.control_handle(), node_info)?;
        let (inner, _is_terminated) = stream.into_inner();
        // It's safe to unwrap here because inner is clearly the only Arc reference left.
        Ok(Arc::try_unwrap(inner).unwrap().into_channel().into())
    }

    /// Terminates the object request with the given status.
    pub fn shutdown(self, status: Status) {
        if self.object_request.is_invalid_handle() {
            return;
        }
        if let ObjectRequestSend::OnOpen = self.what_to_send {
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
    pub fn handle<T>(
        mut self,
        f: impl FnOnce(ObjectRequestRef<'_>) -> Result<T, Status>,
    ) -> Option<T> {
        match f(&mut self) {
            Ok(o) => Some(o),
            Err(s) => {
                self.shutdown(s);
                None
            }
        }
    }

    /// Spawn a task for the object request.  The callback returns a future that can return a
    /// Status which will be handled appropriately.  If the future succeeds it should return
    /// another future that is responsible for the long term servicing of the object request.  This
    /// is done to avoid paying the stack cost of the object request for the lifetime of the
    /// connection.
    ///
    /// For example:
    ///
    ///   object_request.spawn(
    ///       scope,
    ///       move |object_request| Box::pin(async move {
    ///           // Perform checks on the new connection
    ///           if !valid(...) {
    ///               return Err(Status::INVALID_ARGS);
    ///           }
    ///           // Upon success, return a future that handles the connection.
    ///           let requests = object_request.take().into_request_stream();
    ///           Ok(async {
    ///                  while let request = requests.next().await {
    ///                      ...
    ///                  }
    ///              })
    ///       }));
    ///
    pub fn spawn<F, Fut>(self, scope: &ExecutionScope, f: F)
    where
        for<'a> F:
            FnOnce(ObjectRequestRef<'a>) -> BoxFuture<'a, Result<Fut, Status>> + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        scope.spawn(async {
            // This avoids paying the stack cost for ObjectRequest for the lifetime of the task.
            let fut = {
                let mut this = self;
                match f(&mut this).await {
                    Err(s) => {
                        this.shutdown(s);
                        return;
                    }
                    Ok(fut) => fut,
                }
            };
            fut.await
        });
    }

    /// Waits until the request has a request waiting in its channel.  Returns immediately if this
    /// request requires sending an initial event such as OnOpen or OnRepresentation.  Returns
    /// `true` if the channel is readable (rather than just closed).
    pub async fn wait_till_ready(&self) -> bool {
        if !matches!(self.what_to_send, ObjectRequestSend::Nothing) {
            return true;
        }
        let signals = fasync::OnSignalsRef::new(
            self.object_request.as_handle_ref(),
            fidl::Signals::OBJECT_READABLE | fidl::Signals::CHANNEL_PEER_CLOSED,
        )
        .await
        .unwrap();
        signals.contains(fidl::Signals::OBJECT_READABLE)
    }

    /// Take the ObjectRequest.  The caller is responsible for sending errors.
    pub fn take(&mut self) -> ObjectRequest {
        assert!(!self.object_request.is_invalid_handle());
        Self {
            object_request: std::mem::replace(
                &mut self.object_request,
                fidl::Handle::invalid().into(),
            ),
            what_to_send: self.what_to_send,
            attributes: self.attributes,
            create_attributes: self.create_attributes.take(),
            truncate: self.truncate,
        }
    }

    /// Returns a future that will run the connection. `f` is a callback that returns a future
    /// that will run the connection but it will not be called if the connection is supposed
    /// to be a node connection.
    pub fn create_connection<N: Node, F: Future<Output = ()> + Send + 'static, P: ProtocolsExt>(
        &mut self,
        scope: ExecutionScope,
        node: Arc<N>,
        protocols: P,
        f: impl FnOnce(ExecutionScope, Arc<N>, P, &mut Self) -> Result<F, Status>,
    ) -> Result<BoxFuture<'static, ()>, Status> {
        assert!(!self.object_request.is_invalid_handle());
        if protocols.is_node() {
            Ok(Box::pin(node::Connection::create(scope.clone(), node, protocols, self)?))
        } else {
            Ok(Box::pin(f(scope, node, protocols, self)?))
        }
    }

    /// Spawns a new connection for this request. `f` is similar to `create_connection` above.
    pub fn spawn_connection<N: Node, F: Future<Output = ()> + Send + 'static, P: ProtocolsExt>(
        &mut self,
        scope: ExecutionScope,
        node: Arc<N>,
        protocols: P,
        f: impl FnOnce(ExecutionScope, Arc<N>, P, &mut Self) -> Result<F, Status>,
    ) -> Result<(), Status> {
        assert!(!self.object_request.is_invalid_handle());
        if protocols.is_node() {
            scope.spawn(node::Connection::create(scope.clone(), node, protocols, self)?);
        } else {
            scope.spawn(f(scope.clone(), node, protocols, self)?);
        }
        Ok(())
    }
}

pub type ObjectRequestRef<'a> = &'a mut ObjectRequest;

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ObjectRequestSend {
    OnOpen,
    OnRepresentation,
    Nothing,
}

/// Trait to get either fio::Representation or fio::NodeInfoDeprecated.  Connection types
/// should implement this.
pub trait Representation {
    /// The protocol used for the connection.
    type Protocol: ProtocolMarker;

    /// Returns io2's Representation for the object.
    fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> impl Future<Output = Result<fio::Representation, Status>> + Send;

    /// Returns io1's NodeInfoDeprecated.
    fn node_info(&self) -> impl Future<Output = Result<fio::NodeInfoDeprecated, Status>> + Send;
}

/// Trait for converting fio::Flags and fio::OpenFlags into ObjectRequest.
pub trait ToObjectRequest: ProtocolsExt {
    fn to_object_request(&self, object_request: impl Into<fidl::Handle>) -> ObjectRequest;
}

impl ToObjectRequest for fio::OpenFlags {
    fn to_object_request(&self, object_request: impl Into<fidl::Handle>) -> ObjectRequest {
        ObjectRequest::new(
            object_request.into().into(),
            if self.contains(fio::OpenFlags::DESCRIBE) {
                ObjectRequestSend::OnOpen
            } else {
                ObjectRequestSend::Nothing
            },
            fio::NodeAttributesQuery::empty(),
            None,
            self.is_truncate(),
        )
    }
}

impl ToObjectRequest for fio::Flags {
    fn to_object_request(&self, object_request: impl Into<fidl::Handle>) -> ObjectRequest {
        ObjectRequest::new3(*self, &Default::default(), object_request.into().into())
    }
}

fn send_on_open(
    control_handle: &fio::NodeControlHandle,
    node_info: fio::NodeInfoDeprecated,
) -> Result<(), Status> {
    control_handle
        .send_on_open_(Status::OK.into_raw(), Some(node_info))
        .map_err(|_| Status::PEER_CLOSED)
}
