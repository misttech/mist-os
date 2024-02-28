// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of a (limited) node connection.

use crate::{
    common::{inherit_rights_for_clone, IntoAny},
    directory::entry_container::MutableDirectory,
    execution_scope::ExecutionScope,
    name::Name,
    node,
    object_request::Representation,
    protocols::ToNodeOptions,
    ObjectRequestRef, ToObjectRequest,
};

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    futures::stream::StreamExt,
    libc::{S_IRUSR, S_IWUSR},
    std::{future::Future, sync::Arc},
};

/// POSIX emulation layer access attributes for all services created with service().
#[cfg(not(target_os = "macos"))]
pub const POSIX_READ_WRITE_PROTECTION_ATTRIBUTES: u32 = S_IRUSR | S_IWUSR;
#[cfg(target_os = "macos")]
pub const POSIX_READ_WRITE_PROTECTION_ATTRIBUTES: u16 = S_IRUSR | S_IWUSR;

#[derive(Clone, Copy)]
pub struct NodeOptions {
    pub rights: fio::Operations,
}

pub trait IsDirectory {
    fn is_directory(&self) -> bool {
        true
    }
}

/// All nodes must implement this trait.
#[async_trait]
pub trait Node: IsDirectory + IntoAny + Send + Sync + 'static {
    /// Returns node attributes (io2).
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status>;

    /// Get this node's attributes.
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status>;

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

    async fn link_into(
        self: Arc<Self>,
        _destination_dir: Arc<dyn MutableDirectory>,
        _name: Name,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
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
        object_request: ObjectRequestRef,
    ) -> Result<(), Status> {
        self.will_open_as_node()?;
        scope.spawn(node::Connection::create(scope.clone(), self, options, object_request)?);
        Ok(())
    }
}

/// Represents a FIDL (limited) node connection.
pub struct Connection<N: Node + ?Sized> {
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

impl<N: Node + ?Sized> Connection<N> {
    pub fn create(
        scope: ExecutionScope,
        node: Arc<N>,
        options: impl ToNodeOptions,
        object_request: ObjectRequestRef,
    ) -> Result<impl Future<Output = ()>, Status> {
        let node = OpenNode::new(node);
        let options = options.to_node_options(node.is_directory())?;
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
                // TODO(https://fxbug.dev/42157659): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(Status::NOT_SUPPORTED);
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
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::NodeRequest::GetAttr { responder } => match {
                if !self.options.rights.contains(fio::Operations::GET_ATTRIBUTES) {
                    Err(Status::BAD_HANDLE)
                } else {
                    self.node.get_attrs().await
                }
            } {
                Ok(attr) => responder.send(Status::OK.into_raw(), &attr)?,
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
                responder.send(Status::BAD_HANDLE.into_raw())?;
            }
            fio::NodeRequest::GetAttributes { query, responder } => {
                let result = self.node.get_attributes(query).await;
                responder.send(
                    result
                        .as_ref()
                        .map(|a| {
                            let fio::NodeAttributes2 {
                                mutable_attributes: m,
                                immutable_attributes: i,
                            } = a;
                            (m, i)
                        })
                        .map_err(|status| Status::into_raw(*status)),
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
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        flags.to_object_request(server_end).handle(|object_request| {
            let options = inherit_rights_for_clone(fio::OpenFlags::NODE_REFERENCE, flags)?
                .to_node_options(self.node.is_directory())?;

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
}

impl<N: Node + ?Sized> Representation for Connection<N> {
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
