// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of a service endpoint.

mod common;

#[cfg(test)]
mod tests;

use crate::{
    directory::entry::{DirectoryEntry, EntryInfo},
    execution_scope::ExecutionScope,
    node::{self, Node},
    object_request::ObjectRequestSend,
    path::Path,
    service::common::new_connection_validate_flags,
    ProtocolsExt, ToObjectRequest,
};

use {
    async_trait::async_trait,
    fidl::{
        self,
        endpoints::{RequestStream, ServerEnd},
    },
    fidl_fuchsia_io as fio,
    fuchsia_async::Channel,
    fuchsia_zircon as zx,
    futures::future::Future,
    libc::{S_IRUSR, S_IWUSR},
    std::sync::Arc,
};

/// Constructs a node in your file system that will host a service that implements a statically
/// specified FIDL protocol.  `ServerRequestStream` specifies the type of the server side of this
/// protocol.
///
/// `create_server` is a callback that is invoked when a new connection to the file system node is
/// established.  The connection is reinterpreted as a `ServerRequestStream` FIDL connection and
/// passed to `create_server`.  A task produces by the `create_server` callback is execution in the
/// same [`ExecutionScope`] as the one hosting current connection.
///
/// Prefer to use this method, if the type of your FIDL protocol is statically known and you want
/// to use the connection execution scope to serve the protocol requests.  See [`endpoint`] for a
/// lower level version that gives you more flexibility.
pub fn host<ServerRequestStream, CreateServer, Task>(create_server: CreateServer) -> Arc<Service>
where
    ServerRequestStream: RequestStream,
    CreateServer: Fn(ServerRequestStream) -> Task + Send + Sync + 'static,
    Task: Future<Output = ()> + Send + 'static,
{
    endpoint(move |scope, channel| {
        let requests = RequestStream::from_channel(channel);
        let task = create_server(requests);
        // There is no way to report executor failures, and if it is failing it must be shutting
        // down.
        let _ = scope.spawn(task);
    })
}

/// Constructs a node in your file system that will host a service.
///
/// This is a lower level version of [`host`], which you should prefer if it matches your use case.
/// Unlike [`host`], `endpoint` uses a callback that will just consume the server side of the
/// channel when it is connected to the service node.  It is up to the implementer of the `open`
/// callback to decide how to interpret the channel (allowing for non-static protocol selection)
/// and/or where the processing of the messages received over the channel will occur (but the
/// [`ExecutionScope`] connected to the connection is provided every time).
pub fn endpoint<Open>(open: Open) -> Arc<Service>
where
    Open: Fn(ExecutionScope, Channel) + Send + Sync + 'static,
{
    Arc::new(Service { open: Box::new(open) })
}

/// Represents a node in the file system that hosts a service.  Opening a connection to this node
/// will switch to FIDL protocol that is different from the file system protocols, described in
/// fuchsia.io.  See there for additional details.
///
/// Use [`host`] or [`endpoint`] to construct nodes of this type.
pub struct Service {
    open: Box<dyn Fn(ExecutionScope, Channel) + Send + Sync>,
}

#[async_trait]
impl DirectoryEntry for Service {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(zx::Status::NOT_DIR);
            }
            if flags.is_node() {
                scope.spawn(node::Connection::create(scope.clone(), self, flags, object_request)?);
            } else {
                new_connection_validate_flags(flags)?;
                if object_request.what_to_send() == ObjectRequestSend::OnOpen {
                    if let Ok(channel) = object_request
                        .take()
                        .into_channel_after_sending_on_open(fio::NodeInfoDeprecated::Service(
                            fio::Service,
                        ))
                        .and_then(Channel::from_channel)
                    {
                        (self.open)(scope, channel);
                    }
                } else {
                    if let Ok(channel) = Channel::from_channel(object_request.take().into_channel())
                    {
                        (self.open)(scope, channel);
                    }
                }
            }
            Ok(())
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Service)
    }
}

#[async_trait]
impl Node for Service {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_SERVICE | S_IRUSR | S_IWUSR,
            id: fio::INO_UNKNOWN,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::CONNECTOR,
                abilities: fio::Operations::GET_ATTRIBUTES | fio::Operations::CONNECT,
                content_size: 0,
                storage_size: 0,
                link_count: 1,
                id: fio::INO_UNKNOWN,
            }
        ))
    }
}
