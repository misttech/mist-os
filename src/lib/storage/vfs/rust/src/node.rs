// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a (limited) node connection.

use crate::{
    common::inherit_rights_for_clone, directory::entry::DirectoryEntry,
    execution_scope::ExecutionScope, object_request::Representation, path::Path, ObjectRequestRef,
    ProtocolsExt, ToObjectRequest,
};

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{
        self as zx,
        sys::{ZX_ERR_BAD_HANDLE, ZX_ERR_NOT_SUPPORTED, ZX_OK},
    },
    futures::stream::StreamExt,
    libc::{S_IRUSR, S_IWUSR},
    std::{future::Future, sync::Arc},
};

/// POSIX emulation layer access attributes for all services created with service().
pub const POSIX_READ_WRITE_PROTECTION_ATTRIBUTES: u32 = S_IRUSR | S_IWUSR;

pub struct NodeOptions {
    pub rights: fio::Operations,
}

/// All nodes must implement this trait.
#[async_trait]
pub trait Node: DirectoryEntry {
    /// Returns node attributes (io2).
    async fn get_attributes(
        &self,
        _requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    /// Get this node's attributes.
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status>;

    /// Called when the node is closed.
    fn close(self: Arc<Self>) {}
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
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef,
    ) -> Result<impl Future<Output = ()>, zx::Status> {
        let node = OpenNode::new(node);
        let options = protocols.to_node_options(&node.entry_info())?;
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
            fio::NodeRequest::Clone { flags, object, control_handle: _ } => {
                self.handle_clone(flags, object);
            }
            fio::NodeRequest::Reopen { rights_request: _, object_request, control_handle: _ } => {
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            fio::NodeRequest::Close { responder } => {
                responder.send(Ok(()))?;
                return Ok(ConnectionState::Closed);
            }
            fio::NodeRequest::GetConnectionInfo { responder } => {
                responder.send(fio::ConnectionInfo {
                    rights: Some(fio::Operations::GET_ATTRIBUTES),
                    ..Default::default()
                })?;
            }
            fio::NodeRequest::Sync { responder } => {
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::GetAttr { responder } => match {
                if !self.options.rights.contains(fio::Operations::GET_ATTRIBUTES) {
                    Err(zx::Status::BAD_HANDLE)
                } else {
                    self.node.get_attrs().await
                }
            } {
                Ok(attr) => responder.send(ZX_OK, &attr)?,
                Err(status) => {
                    responder.send(
                        status.into_raw(),
                        &fio::NodeAttributes {
                            mode: 0,
                            id: fio::INO_UNKNOWN,
                            content_size: 0,
                            storage_size: 0,
                            link_count: 0,
                            creation_time: 0,
                            modification_time: 0,
                        },
                    )?;
                }
            },
            fio::NodeRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(ZX_ERR_BAD_HANDLE)?;
            }
            fio::NodeRequest::GetAttributes { query: _, responder } => {
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::UpdateAttributes { payload: _, responder } => {
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::ListExtendedAttributes { iterator, .. } => {
                iterator.close_with_epitaph(zx::Status::NOT_SUPPORTED)?;
            }
            fio::NodeRequest::GetExtendedAttribute { responder, .. } => {
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::SetExtendedAttribute { responder, .. } => {
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::RemoveExtendedAttribute { responder, .. } => {
                responder.send(Err(ZX_ERR_NOT_SUPPORTED))?;
            }
            fio::NodeRequest::GetFlags { responder } => {
                responder.send(ZX_OK, fio::OpenFlags::NODE_REFERENCE)?;
            }
            fio::NodeRequest::SetFlags { flags: _, responder } => {
                responder.send(ZX_ERR_BAD_HANDLE)?;
            }
            fio::NodeRequest::Query { responder } => {
                responder.send(fio::NODE_PROTOCOL_NAME.as_bytes())?;
            }
            fio::NodeRequest::QueryFilesystem { responder } => {
                responder.send(ZX_ERR_NOT_SUPPORTED, None)?;
            }
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        flags.to_object_request(server_end).handle(|object_request| {
            let flags = inherit_rights_for_clone(fio::OpenFlags::NODE_REFERENCE, flags)?;
            self.node.clone().open(
                self.scope.clone(),
                flags,
                Path::dot(),
                object_request.take().into_server_end(),
            );
            Ok(())
        });
    }
}

#[async_trait]
impl<N: Node> Representation for Connection<N> {
    type Protocol = fio::NodeMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, zx::Status> {
        Ok(fio::Representation::Connector(fio::ConnectorInfo {
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.node.get_attributes(requested_attributes).await?)
            },
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, zx::Status> {
        Ok(fio::NodeInfoDeprecated::Service(fio::Service))
    }
}

/// This struct is a RAII wrapper around a node that will call close() on it when dropped.
pub struct OpenNode<T: Node + ?Sized> {
    node: Arc<T>,
}

impl<T: Node + ?Sized> OpenNode<T> {
    pub fn new(node: Arc<T>) -> Self {
        Self { node }
    }
}

impl<T: Node + ?Sized> Drop for OpenNode<T> {
    fn drop(&mut self) {
        self.node.clone().close();
    }
}

impl<T: Node + ?Sized> std::ops::Deref for OpenNode<T> {
    type Target = Arc<T>;

    fn deref(&self) -> &Self::Target {
        &self.node
    }
}
