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
        path::Path,
    },
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::{ControlHandle as _, ServerEnd},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{channel::oneshot, select, StreamExt},
    std::sync::Arc,
};

#[async_trait]
pub trait Symlink: Send + Sync {
    async fn read_target(&self) -> Result<Vec<u8>, zx::Status>;
}

pub struct Connection {
    control_handle: fio::SymlinkControlHandle,
    symlink: Arc<dyn Symlink>,
    scope: ExecutionScope,
}

fn validate_flags(flags: fio::OpenFlags) -> Result<(), zx::Status> {
    // TODO(fxbug.dev/123390): Support NODE_REFERENCE.

    if flags.intersects(fio::OpenFlags::DIRECTORY) {
        return Err(zx::Status::NOT_DIR);
    }

    // We allow write and executable access because the client might not know this is a symbolic
    // link and they want to open the target of the link with write or executable rights.
    let optional = fio::OpenFlags::NOT_DIRECTORY
        | fio::OpenFlags::DESCRIBE
        | fio::OpenFlags::RIGHT_WRITABLE
        | fio::OpenFlags::RIGHT_EXECUTABLE;

    if flags & !optional != fio::OpenFlags::RIGHT_READABLE {
        return Err(zx::Status::INVALID_ARGS);
    }

    Ok(())
}

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

impl Connection {
    /// Spawns a new task to run the connection.
    pub fn spawn(
        scope: ExecutionScope,
        symlink: Arc<dyn Symlink>,
        flags: fio::OpenFlags,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        scope.clone().spawn_with_shutdown(move |shutdown| {
            Self::run(scope, symlink, flags, server_end, shutdown)
        });
    }

    /// Processes requests for this connection until either `shutdown` is notfied, the connection is
    /// closed, or an error with the connection is encountered.
    pub async fn run(
        scope: ExecutionScope,
        symlink: Arc<dyn Symlink>,
        flags: fio::OpenFlags,
        server_end: ServerEnd<fio::NodeMarker>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        if let Err(status) = validate_flags(flags) {
            send_on_open_with_error(describe, server_end, status);
            return;
        }

        let target = if describe {
            match symlink.read_target().await {
                Ok(target) => Some(target),
                Err(status) => {
                    send_on_open_with_error(describe, server_end, status);
                    return;
                }
            }
        } else {
            None
        };

        let (mut requests, control_handle) =
            match ServerEnd::<fio::SymlinkMarker>::new(server_end.into_channel())
                .into_stream_and_control_handle()
            {
                Ok((requests, control_handle)) => (requests, control_handle),
                Err(_) => {
                    // As we report all errors on `server_end`, if we failed to send an error over
                    // this connection, there is nowhere to send the error to.
                    return;
                }
            };

        let mut connection = Connection { control_handle, symlink, scope };

        if let Some(target) = target {
            if connection
                .control_handle
                .send_on_open_(
                    zx::Status::OK.into_raw(),
                    Some(fio::NodeInfoDeprecated::Symlink(fio::SymlinkObject { target })),
                )
                .is_err()
            {
                return;
            }
        }

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
            if connection.handle_request(request).await.unwrap_or(true) {
                break;
            }
        }
    }

    // Returns true if the connection should terminate.
    async fn handle_request(&mut self, req: fio::SymlinkRequest) -> Result<bool, Error> {
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
                return Ok(true);
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
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
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
                Err(status) => {
                    self.control_handle.shutdown_with_epitaph(status);
                    return Ok(true);
                }
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
        Ok(false)
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        let flags = match inherit_rights_for_clone(fio::OpenFlags::RIGHT_READABLE, flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        Self::spawn(self.scope.clone(), self.symlink.clone(), flags, server_end);
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
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        if !path.is_empty() {
            send_on_open_with_error(describe, server_end, zx::Status::NOT_DIR);
            return;
        }
        Connection::spawn(scope, self, flags, server_end);
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Symlink)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Connection, Symlink},
        crate::{common::rights_to_posix_mode_bits, execution_scope::ExecutionScope},
        assert_matches::assert_matches,
        async_trait::async_trait,
        fidl::endpoints::{create_proxy, ServerEnd},
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
            fio::OpenFlags::RIGHT_READABLE,
            ServerEnd::new(server_end.into_channel()),
        );

        assert_eq!(
            client_end.describe().await.expect("fidl failed").target.expect("missing target"),
            b"target"
        );
    }

    #[fuchsia::test]
    async fn test_validate_flags() {
        let scope = ExecutionScope::new();

        let check = |flags| {
            let (client_end, server_end) =
                create_proxy::<fio::SymlinkMarker>().expect("create_proxy failed");
            Connection::spawn(
                scope.clone(),
                Arc::new(TestSymlink),
                flags | fio::OpenFlags::DESCRIBE,
                ServerEnd::new(server_end.into_channel()),
            );
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
        Connection::spawn(
            scope,
            Arc::new(TestSymlink),
            fio::OpenFlags::RIGHT_READABLE,
            ServerEnd::new(server_end.into_channel()),
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
        Connection::spawn(
            scope,
            Arc::new(TestSymlink),
            fio::OpenFlags::RIGHT_READABLE,
            ServerEnd::new(server_end.into_channel()),
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
