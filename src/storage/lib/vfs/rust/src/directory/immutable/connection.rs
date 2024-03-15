// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connection to a directory that can not be modified by the client, no matter what permissions
//! the client has on the FIDL connection.

use crate::{
    directory::{
        connection::{BaseConnection, ConnectionState, DerivedConnection},
        entry_container,
    },
    execution_scope::ExecutionScope,
    node::OpenNode,
    ObjectRequestRef, ProtocolsExt,
};

use {
    fidl_fuchsia_io as fio,
    fio::DirectoryRequest,
    fuchsia_zircon_status::Status,
    futures::TryStreamExt as _,
    std::{future::Future, sync::Arc},
};

pub struct ImmutableConnection {
    base: BaseConnection<Self>,
}

impl ImmutableConnection {
    async fn handle_requests<RS>(mut self, mut requests: RS)
    where
        RS: futures::stream::TryStream<Ok = DirectoryRequest, Error = fidl::Error> + Unpin,
    {
        while let Ok(Some(request)) = requests.try_next().await {
            let _guard = self.base.scope.active_guard();
            if !matches!(self.base.handle_request(request).await, Ok(ConnectionState::Alive)) {
                break;
            }
        }
    }

    pub fn create(
        scope: ExecutionScope,
        directory: Arc<impl entry_container::Directory>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        Self::create_transform_stream(
            scope,
            directory,
            protocols,
            object_request,
            std::convert::identity,
        )
    }

    /// TODO(https://fxbug.dev/326626515): this is an experimental method to run a FIDL
    /// directory connection until stalled, with the purpose to cleanly stop a component.
    /// We'll expect to revisit how this works to generalize to all connections later.
    /// Try not to use this function for other purposes.
    pub fn create_transform_stream<Transform, RS>(
        scope: ExecutionScope,
        directory: Arc<impl entry_container::Directory>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
        transform: Transform,
    ) -> Result<impl Future<Output = ()>, Status>
    where
        Transform: FnOnce(fio::DirectoryRequestStream) -> RS,
        RS: futures::stream::TryStream<Ok = DirectoryRequest, Error = fidl::Error> + Unpin,
    {
        // Ensure we close the directory if we fail to create the connection.
        let directory = OpenNode::new(directory as Arc<dyn entry_container::Directory>);

        let connection = ImmutableConnection {
            base: BaseConnection::<Self>::new(scope, directory, protocols.to_directory_options()?),
        };

        // If we fail to send the task to the executor, it is probably shut down or is in the
        // process of shutting down (this is the only error state currently).  So there is nothing
        // for us to do - the connection will be closed automatically when the connection object is
        // dropped.
        let object_request = object_request.take();
        Ok(async move {
            if let Ok(requests) = object_request.into_request_stream(&connection.base).await {
                connection.handle_requests(transform(requests)).await;
            }
        })
    }
}

impl DerivedConnection for ImmutableConnection {
    type Directory = dyn entry_container::Directory;
    const MUTABLE: bool = false;
}
