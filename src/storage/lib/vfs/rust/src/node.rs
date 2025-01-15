// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a (limited) node connection.

use crate::common::{inherit_rights_for_clone, IntoAny};
use crate::directory::entry::GetEntryInfo;
use crate::directory::entry_container::MutableDirectory;
use crate::execution_scope::ExecutionScope;
use crate::name::Name;
use crate::object_request::Representation;
use crate::protocols::ToNodeOptions;
use crate::{node, ObjectRequestRef, ToObjectRequest};
use anyhow::Error;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use futures::stream::StreamExt;
use libc::{S_IRUSR, S_IWUSR};
use std::future::{ready, Future};
use std::sync::Arc;
use zx_status::Status;

/// POSIX emulation layer access attributes for all services created with service().
#[cfg(not(target_os = "macos"))]
pub const POSIX_READ_WRITE_PROTECTION_ATTRIBUTES: u32 = S_IRUSR | S_IWUSR;
#[cfg(target_os = "macos")]
pub const POSIX_READ_WRITE_PROTECTION_ATTRIBUTES: u16 = S_IRUSR | S_IWUSR;

#[derive(Clone, Copy)]
pub struct NodeOptions {
    pub rights: fio::Operations,
}

/// All nodes must implement this trait.
pub trait Node: GetEntryInfo + IntoAny + Send + Sync + 'static {
    /// Returns node attributes (io2).
    fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> impl Future<Output = Result<fio::NodeAttributes2, Status>> + Send
    where
        Self: Sized;

    /// Called when the node is about to be opened as the node protocol.  Implementers can use this
    /// to perform any initialization or reference counting.  Errors here will result in the open
    /// failing.  By default, this forwards to the infallible will_clone.
    fn will_open_as_node(&self) -> Result<(), Status> {
        self.will_clone();
        Ok(())
    }

    /// Called when the node is about to be cloned (and also by the default implementation of
    /// will_open_as_node).  Implementations that perform their own open count can use this.  Each
    /// call to `will_clone` will be accompanied by an eventual call to `close`.
    fn will_clone(&self) {}

    /// Called when the node is closed.
    fn close(self: Arc<Self>) {}

    fn link_into(
        self: Arc<Self>,
        _destination_dir: Arc<dyn MutableDirectory>,
        _name: Name,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Returns information about the filesystem.
    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Opens the node using the node protocol.
    fn open_as_node(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: NodeOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status>
    where
        Self: Sized,
    {
        self.will_open_as_node()?;
        scope.spawn(node::Connection::create(scope.clone(), self, options, object_request)?);
        Ok(())
    }
}

/// Represents a FIDL (limited) node connection.
pub struct Connection<N: Node> {
    // Execution scope this connection and any async operations and connections it creates will
    // use.
    scope: ExecutionScope,

    // The underlying node.
    node: OpenNode<N>,

    // Node options.
    options: NodeOptions,
}

/// Return type for [`handle_request()`] functions.
enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message, it was dropped by the peer, or an error had
    /// occurred.  As we do not perform any actions, except for closing our end we do not
    /// distinguish those cases, unlike file and directory connections.
    Closed,
}

impl<N: Node> Connection<N> {
    pub fn create(
        scope: ExecutionScope,
        node: Arc<N>,
        options: impl ToNodeOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        let node = OpenNode::new(node);
        let options = options.to_node_options(node.entry_info().type_())?;
        let object_request = object_request.take();
        Ok(async move {
            let connection = Connection { scope: scope.clone(), node, options };
            if let Ok(requests) = object_request.into_request_stream(&connection).await {
                connection.handle_requests(requests).await
            }
        })
    }

    async fn handle_requests(mut self, mut requests: fio::NodeRequestStream) {
        while let Some(request_or_err) = requests.next().await {
            match request_or_err {
                Err(_) => {
                    // FIDL level error, such as invalid message format and alike.  Close the
                    // connection on any unexpected error.
                    // TODO: Send an epitaph.
                    break;
                }
                Ok(request) => {
                    match self.handle_request(request).await {
                        Ok(ConnectionState::Alive) => (),
                        Ok(ConnectionState::Closed) | Err(_) => {
                            // Err(_) means a protocol level error.  Close the connection on any
                            // unexpected error.  TODO: Send an epitaph.
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Handle a [`NodeRequest`].
    async fn handle_request(&mut self, req: fio::NodeRequest) -> Result<ConnectionState, Error> {
        match req {
            #[cfg(fuchsia_api_level_at_least = "26")]
            fio::NodeRequest::DeprecatedClone { flags, object, control_handle: _ } => {
                self.handle_clone_deprecated(flags, object);
            }
            #[cfg(not(fuchsia_api_level_at_least = "26"))]
            fio::NodeRequest::Clone { flags, object, control_handle: _ } => {
                self.handle_clone_deprecated(flags, object);
            }
            #[cfg(fuchsia_api_level_at_least = "26")]
            fio::NodeRequest::Clone { request, control_handle: _ } => {
                // Suppress any errors in the event a bad `request` channel was provided.
                self.handle_clone(ServerEnd::new(request.into_channel()));
            }
            #[cfg(not(fuchsia_api_level_at_least = "26"))]
            fio::NodeRequest::Clone2 { request, control_handle: _ } => {
                // Suppress any errors in the event a bad `request` channel was provided.
                self.handle_clone(ServerEnd::new(request.into_channel()));
            }
            fio::NodeRequest::Close { responder } => {
                responder.send(Ok(()))?;
                return Ok(ConnectionState::Closed);
            }
            fio::NodeRequest::GetConnectionInfo { responder } => {
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::NodeRequest::Sync { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::GetAttr { responder } => {
                let (status, attrs) =
                    crate::common::io2_to_io1_attrs(self.node.as_ref(), self.options.rights).await;
                responder.send(status.into_raw(), &attrs)?;
            }
            fio::NodeRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(Status::BAD_HANDLE.into_raw())?;
            }
            fio::NodeRequest::GetAttributes { query, responder } => {
                let result = self.node.get_attributes(query).await;
                responder.send(
                    result
                        .as_ref()
                        .map(|attrs| (&attrs.mutable_attributes, &attrs.immutable_attributes))
                        .map_err(|status| status.into_raw()),
                )?;
            }
            fio::NodeRequest::UpdateAttributes { payload: _, responder } => {
                responder.send(Err(Status::BAD_HANDLE.into_raw()))?;
            }
            fio::NodeRequest::ListExtendedAttributes { iterator, .. } => {
                iterator.close_with_epitaph(Status::NOT_SUPPORTED)?;
            }
            fio::NodeRequest::GetExtendedAttribute { responder, .. } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::SetExtendedAttribute { responder, .. } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::RemoveExtendedAttribute { responder, .. } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::GetFlags { responder } => {
                responder.send(Status::OK.into_raw(), fio::OpenFlags::NODE_REFERENCE)?;
            }
            fio::NodeRequest::SetFlags { flags: _, responder } => {
                responder.send(Status::BAD_HANDLE.into_raw())?;
            }
            fio::NodeRequest::Query { responder } => {
                responder.send(fio::NODE_PROTOCOL_NAME.as_bytes())?;
            }
            fio::NodeRequest::QueryFilesystem { responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw(), None)?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::NodeRequest::GetFlags2 { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::NodeRequest::SetFlags2 { flags: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::_UnknownMethod { .. } => (),
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone_deprecated(
        &mut self,
        flags: fio::OpenFlags,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let options = inherit_rights_for_clone(fio::OpenFlags::NODE_REFERENCE, flags)?
                .to_node_options(self.node.entry_info().type_())?;

            self.node.will_clone();

            let connection =
                Self { scope: self.scope.clone(), node: OpenNode::new(self.node.clone()), options };

            object_request.take().spawn(&self.scope, |object_request| {
                Box::pin(async {
                    let requests = object_request.take().into_request_stream(&connection).await?;
                    Ok(connection.handle_requests(requests))
                })
            });

            Ok(())
        });
    }

    fn handle_clone(&mut self, server_end: ServerEnd<fio::NodeMarker>) {
        self.node.will_clone();
        let connection = Self {
            scope: self.scope.clone(),
            node: OpenNode::new(self.node.clone()),
            options: self.options,
        };
        self.scope.spawn(async move {
            let requests = server_end.into_stream();
            connection.handle_requests(requests).await;
        });
    }
}

impl<N: Node> Representation for Connection<N> {
    type Protocol = fio::NodeMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, Status> {
        Ok(fio::Representation::Connector(fio::ConnectorInfo {
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.node.get_attributes(requested_attributes).await?)
            },
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        Ok(fio::NodeInfoDeprecated::Service(fio::Service))
    }
}

/// This struct is a RAII wrapper around a node that will call close() on it when dropped.
pub struct OpenNode<T: Node> {
    node: Arc<T>,
}

impl<T: Node> OpenNode<T> {
    pub fn new(node: Arc<T>) -> Self {
        Self { node }
    }
}

impl<T: Node> Drop for OpenNode<T> {
    fn drop(&mut self) {
        self.node.clone().close();
    }
}

impl<T: Node> std::ops::Deref for OpenNode<T> {
    type Target = Arc<T>;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}
