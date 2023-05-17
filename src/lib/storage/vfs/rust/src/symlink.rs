// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Server support for symbolic links.

use {
    crate::{
        common::{
            inherit_rights_for_clone, rights_to_posix_mode_bits, send_on_open_with_error, IntoAny,
        },
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        object_request::Representation,
        path::Path,
        ObjectRequest, ProtocolsExt, ToObjectRequest,
    },
    async_trait::async_trait,
    fidl::endpoints::{ControlHandle as _, RequestStream, ServerEnd},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{channel::oneshot, select, StreamExt},
    std::sync::Arc,
};

#[async_trait]
pub trait Symlink: Send + Sync {
    async fn read_target(&self) -> Result<Vec<u8>, zx::Status>;

    /// Returns node attributes (io2).
    async fn get_attributes(
        &self,
        _requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

pub struct Connection {
    symlink: Arc<dyn Symlink>,
    scope: ExecutionScope,
}

pub struct SymlinkOptions;

// Returns null attributes that can be used in error cases.
fn null_node_attributes() -> fio::NodeAttributes {
    fio::NodeAttributes {
        mode: 0,
        id: fio::INO_UNKNOWN,
        content_size: 0,
        storage_size: 0,
        link_count: 0,
        creation_time: 0,
        modification_time: 0,
    }
}

enum HandleRequestError {
    ShutdownWithEpitaph(zx::Status),
    Other,
}

impl<E: std::error::Error> From<E> for HandleRequestError {
    fn from(_: E) -> HandleRequestError {
        HandleRequestError::Other
    }
}

impl Connection {
    /// Spawns a new task to run the connection.
    pub fn spawn(
        scope: ExecutionScope,
        symlink: Arc<dyn Symlink>,
        options: SymlinkOptions,
        object_request: ObjectRequest,
    ) {
        scope.clone().spawn_with_shutdown(move |shutdown| {
            Self::run(scope, symlink, options, object_request, shutdown)
        });
    }

    /// Processes requests for this connection until either `shutdown` is notfied, the connection is
    /// closed, or an error with the connection is encountered.
    pub async fn run(
        scope: ExecutionScope,
        symlink: Arc<dyn Symlink>,
        _options: SymlinkOptions,
        object_request: ObjectRequest,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let mut connection = Connection { symlink, scope };
        if let Ok(mut requests) = object_request.into_request_stream(&connection).await {
            while let Some(request) = select! {
                request = requests.next() => {
                    if let Some(Ok(request)) = request {
                        Some(request)
                    } else {
                        None
                    }
                },
                _ = shutdown => None,
            } {
                if let Err(e) = connection.handle_request(request).await {
                    if let HandleRequestError::ShutdownWithEpitaph(status) = e {
                        requests.control_handle().shutdown_with_epitaph(status);
                    }
                    break;
                }
            }
        }
    }

    // Returns true if the connection should terminate.
    async fn handle_request(&mut self, req: fio::SymlinkRequest) -> Result<(), HandleRequestError> {
        match req {
            fio::SymlinkRequest::Clone { flags, object, control_handle: _ } => {
                self.handle_clone(flags, object);
            }
            fio::SymlinkRequest::Reopen {
                rights_request: _,
                object_request,
                control_handle: _,
            } => {
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            fio::SymlinkRequest::Close { responder } => {
                responder.send(&mut Ok(()))?;
                return Err(HandleRequestError::Other);
            }
            fio::SymlinkRequest::GetConnectionInfo { responder } => {
                // TODO(https://fxbug.dev/77623): Restrict GET_ATTRIBUTES.
                let rights = fio::Operations::GET_ATTRIBUTES;
                responder
                    .send(fio::ConnectionInfo { rights: Some(rights), ..Default::default() })?;
            }
            fio::SymlinkRequest::Sync { responder } => {
                responder.send(&mut Ok(()))?;
            }
            fio::SymlinkRequest::GetAttr { responder } => match self.handle_get_attr().await {
                Ok(attrs) => responder.send(zx::Status::OK.into_raw(), &attrs)?,
                Err(status) => responder.send(status.into_raw(), &null_node_attributes())?,
            },
            fio::SymlinkRequest::SetAttr { responder, .. } => {
                responder.send(zx::Status::ACCESS_DENIED.into_raw())?;
            }
            fio::SymlinkRequest::GetAttributes { query: _, responder } => {
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::UpdateAttributes { payload: _, responder } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::ListExtendedAttributes { iterator, .. } => {
                iterator.close_with_epitaph(zx::Status::NOT_SUPPORTED)?;
            }
            fio::SymlinkRequest::GetExtendedAttribute { responder, .. } => {
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::SetExtendedAttribute { responder, .. } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::RemoveExtendedAttribute { responder, .. } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::Describe { responder } => match self.symlink.read_target().await {
                Ok(target) => responder
                    .send(&fio::SymlinkInfo { target: Some(target), ..Default::default() })?,
                Err(status) => return Err(HandleRequestError::ShutdownWithEpitaph(status)),
            },
            fio::SymlinkRequest::GetFlags { responder } => {
                responder.send(zx::Status::NOT_SUPPORTED.into_raw(), fio::OpenFlags::empty())?;
            }
            fio::SymlinkRequest::SetFlags { responder, .. } => {
                responder.send(zx::Status::ACCESS_DENIED.into_raw())?;
            }
            fio::SymlinkRequest::Query { responder } => {
                responder.send(fio::SYMLINK_PROTOCOL_NAME.as_bytes())?;
            }
            fio::SymlinkRequest::QueryFilesystem { responder } => {
                // TODO(fxbug.dev/123390): Support QueryFilesystem
                responder.send(zx::Status::NOT_SUPPORTED.into_raw(), None)?;
            }
        }
        Ok(())
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        let flags = match inherit_rights_for_clone(fio::OpenFlags::RIGHT_READABLE, flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(
                    flags.contains(fio::OpenFlags::DESCRIBE),
                    server_end,
                    status,
                );
                return;
            }
        };
        flags.to_object_request(server_end).handle(|object_request| {
            Self::spawn(
                self.scope.clone(),
                self.symlink.clone(),
                flags.to_symlink_options()?,
                object_request.take(),
            );
            Ok(())
        });
    }

    async fn handle_get_attr(&mut self) -> Result<fio::NodeAttributes, zx::Status> {
        // TODO(fxbug.dev/123390): Add support for more attributes such as timestamps
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_SYMLINK
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false),
            id: fio::INO_UNKNOWN,
            content_size: self.symlink.read_target().await?.len() as u64,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }
}

impl<T: IntoAny + Symlink + Send + Sync> DirectoryEntry for T {
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
            Connection::spawn(scope, self, flags.to_symlink_options()?, object_request.take());
            Ok(())
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Symlink)
    }
}

#[async_trait]
impl Representation for Connection {
    type Protocol = fio::SymlinkMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, zx::Status> {
        Ok(fio::Representation::Symlink(fio::SymlinkInfo {
            attributes: Some(self.symlink.get_attributes(requested_attributes).await?),
            target: Some(self.symlink.read_target().await?),
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, zx::Status> {
        Ok(fio::NodeInfoDeprecated::Symlink(fio::SymlinkObject {
            target: self.symlink.read_target().await?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Connection, Symlink},
        crate::{
            common::rights_to_posix_mode_bits, execution_scope::ExecutionScope,
            symlink::SymlinkOptions, ProtocolsExt, ToObjectRequest,
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        fidl::endpoints::create_proxy,
        fidl_fuchsia_io as fio, fuchsia_zircon as zx,
        futures::StreamExt,
        std::sync::Arc,
    };

    struct TestSymlink;

    #[async_trait]
    impl Symlink for TestSymlink {
        async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
            Ok(b"target".to_vec())
        }
    }

    #[fuchsia::test]
    async fn test_read_target() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) =
            create_proxy::<fio::SymlinkMarker>().expect("create_proxy failed");
        Connection::spawn(
            scope,
            Arc::new(TestSymlink),
            SymlinkOptions,
            fio::OpenFlags::RIGHT_READABLE.to_object_request(server_end),
        );

        assert_eq!(
            client_end.describe().await.expect("fidl failed").target.expect("missing target"),
            b"target"
        );
    }

    #[fuchsia::test]
    async fn test_validate_flags() {
        let scope = ExecutionScope::new();

        let check = |mut flags: fio::OpenFlags| {
            let (client_end, server_end) =
                create_proxy::<fio::SymlinkMarker>().expect("create_proxy failed");
            flags |= fio::OpenFlags::DESCRIBE;
            flags.to_object_request(server_end).handle(|object_request| {
                Connection::spawn(
                    scope.clone(),
                    Arc::new(TestSymlink),
                    flags.to_symlink_options()?,
                    object_request.take(),
                );
                Ok(())
            });
            async move {
                zx::Status::from_raw(
                    client_end
                        .take_event_stream()
                        .next()
                        .await
                        .expect("no event")
                        .expect("next failed")
                        .into_on_open_()
                        .expect("expected OnOpen")
                        .0,
                )
            }
        };

        for flags in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
            fio::OpenFlags::POSIX_WRITABLE,
            fio::OpenFlags::POSIX_EXECUTABLE,
            fio::OpenFlags::CLONE_SAME_RIGHTS,
            fio::OpenFlags::BLOCK_DEVICE,
        ] {
            assert_eq!(check(flags).await, zx::Status::INVALID_ARGS, "{flags:?}");
        }

        assert_eq!(
            check(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY).await,
            zx::Status::OK
        );
    }

    #[fuchsia::test]
    async fn test_get_attr() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) =
            create_proxy::<fio::SymlinkMarker>().expect("create_proxy failed");
        let flags = fio::OpenFlags::RIGHT_READABLE;
        Connection::spawn(
            scope,
            Arc::new(TestSymlink),
            flags.to_symlink_options().unwrap(),
            flags.to_object_request(server_end.into_channel()),
        );

        assert_matches!(
            client_end.get_attr().await.expect("fidl failed"),
            (
                0,
                fio::NodeAttributes {
                    mode,
                    id: fio::INO_UNKNOWN,
                    content_size: 6,
                    storage_size: 0,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            ) if mode == fio::MODE_TYPE_SYMLINK
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false)
        );
    }

    #[fuchsia::test]
    async fn test_describe() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) =
            create_proxy::<fio::SymlinkMarker>().expect("create_proxy failed");
        let flags = fio::OpenFlags::RIGHT_READABLE;
        Connection::spawn(
            scope,
            Arc::new(TestSymlink),
            flags.to_symlink_options().unwrap(),
            flags.to_object_request(server_end),
        );

        assert_matches!(
            client_end.describe().await.expect("fidl failed"),
            fio::SymlinkInfo {
                target: Some(target),
                ..
            } if target == b"target"
        );
    }
}
