// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{inherit_rights_for_clone, send_on_open_with_error};
use crate::directory::common::check_child_connection_flags;
use crate::directory::entry_container::{Directory, DirectoryWatcher};
use crate::directory::traversal_position::TraversalPosition;
use crate::directory::{read_dirents, DirectoryOptions};
use crate::execution_scope::{yield_to_executor, ExecutionScope};
use crate::node::OpenNode;
use crate::object_request::Representation;
use crate::path::Path;
use crate::protocols::ToFlags as _;

use anyhow::Error;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use std::convert::TryInto as _;
use storage_trace::{self as trace, TraceFutureExt};
use zx_status::Status;

use crate::common::CreationMode;
use crate::{ObjectRequest, ObjectRequestRef, ProtocolsExt};

/// Return type for `BaseConnection::handle_request`.
pub enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message and should be closed.
    Closed,
}

/// Handles functionality shared between mutable and immutable FIDL connections to a directory.  A
/// single directory may contain multiple connections.  Instances of the `BaseConnection`
/// will also hold any state that is "per-connection".  Currently that would be the access flags
/// and the seek position.
pub(in crate::directory) struct BaseConnection<DirectoryType: Directory> {
    /// Execution scope this connection and any async operations and connections it creates will
    /// use.
    pub(in crate::directory) scope: ExecutionScope,

    pub(in crate::directory) directory: OpenNode<DirectoryType>,

    /// Flags set on this connection when it was opened or cloned.
    pub(in crate::directory) options: DirectoryOptions,

    /// Seek position for this connection to the directory.  We just store the element that was
    /// returned last by ReadDirents for this connection.  Next call will look for the next element
    /// in alphabetical order and resume from there.
    ///
    /// An alternative is to use an intrusive tree to have a dual index in both names and IDs that
    /// are assigned to the entries in insertion order.  Then we can store an ID instead of the
    /// full entry name.  This is what the C++ version is doing currently.
    ///
    /// It should be possible to do the same intrusive dual-indexing using, for example,
    ///
    ///     https://docs.rs/intrusive-collections/0.7.6/intrusive_collections/
    ///
    /// but, as, I think, at least for the pseudo directories, this approach is fine, and it simple
    /// enough.
    seek: TraversalPosition,
}

impl<DirectoryType: Directory> BaseConnection<DirectoryType> {
    /// Constructs an instance of `BaseConnection` - to be used by derived connections, when they
    /// need to create a nested `BaseConnection` "sub-object".  But when implementing
    /// `create_connection`, derived connections should use the [`create_connection`] call.
    pub(in crate::directory) fn new(
        scope: ExecutionScope,
        directory: OpenNode<DirectoryType>,
        options: DirectoryOptions,
    ) -> Self {
        BaseConnection { scope, directory, options, seek: Default::default() }
    }

    /// Handle a [`DirectoryRequest`].  This function is responsible for handing all the basic
    /// directory operations.
    pub(in crate::directory) async fn handle_request(
        &mut self,
        request: fio::DirectoryRequest,
    ) -> Result<ConnectionState, Error> {
        match request {
            #[cfg(fuchsia_api_level_at_least = "NEXT")]
            fio::DirectoryRequest::DeprecatedClone { flags, object, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::DeprecatedClone");
                self.handle_deprecated_clone(flags, object);
            }
            #[cfg(not(fuchsia_api_level_at_least = "NEXT"))]
            fio::DirectoryRequest::Clone { flags, object, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::Clone");
                self.handle_deprecated_clone(flags, object);
            }
            #[cfg(fuchsia_api_level_at_least = "NEXT")]
            fio::DirectoryRequest::Clone { request, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::Clone");
                self.handle_clone(request.into_channel());
            }
            #[cfg(not(fuchsia_api_level_at_least = "NEXT"))]
            fio::DirectoryRequest::Clone2 { request, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::Clone2");
                self.handle_clone(request.into_channel());
            }
            fio::DirectoryRequest::Close { responder } => {
                trace::duration!(c"storage", c"Directory::Close");
                responder.send(Ok(()))?;
                return Ok(ConnectionState::Closed);
            }
            fio::DirectoryRequest::GetConnectionInfo { responder } => {
                trace::duration!(c"storage", c"Directory::GetConnectionInfo");
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::DirectoryRequest::GetAttr { responder } => {
                async move {
                    let (status, attrs) = crate::common::io2_to_io1_attrs(
                        self.directory.as_ref(),
                        self.options.rights,
                    )
                    .await;
                    responder.send(status.into_raw(), &attrs)
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::GetAttr"))
                .await?;
            }
            fio::DirectoryRequest::GetAttributes { query, responder } => {
                async move {
                    // TODO(https://fxbug.dev/346585458): Restrict or remove GET_ATTRIBUTES.
                    let attrs = self.directory.get_attributes(query).await;
                    responder.send(
                        attrs
                            .as_ref()
                            .map(|attrs| (&attrs.mutable_attributes, &attrs.immutable_attributes))
                            .map_err(|status| status.into_raw()),
                    )
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::GetAttributes"))
                .await?;
            }
            fio::DirectoryRequest::UpdateAttributes { payload: _, responder } => {
                trace::duration!(c"storage", c"Directory::UpdateAttributes");
                // TODO(https://fxbug.dev/324112547): Handle unimplemented io2 method.
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator, .. } => {
                trace::duration!(c"storage", c"Directory::ListExtendedAttributes");
                iterator.close_with_epitaph(Status::NOT_SUPPORTED)?;
            }
            fio::DirectoryRequest::GetExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::GetExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::SetExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::RemoveExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetFlags { responder } => {
                trace::duration!(c"storage", c"Directory::GetFlags");
                responder.send(Status::OK.into_raw(), self.options.to_io1())?;
            }
            fio::DirectoryRequest::SetFlags { flags: _, responder } => {
                trace::duration!(c"storage", c"Directory::SetFlags");
                responder.send(Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Open { flags, mode: _, path, object, control_handle: _ } => {
                {
                    trace::duration!(c"storage", c"Directory::Open");
                    self.handle_open(flags, path, object);
                }
                // Since open typically spawns a task, yield to the executor now to give that task a
                // chance to run before we try and process the next request for this directory.
                yield_to_executor().await;
            }
            fio::DirectoryRequest::AdvisoryLock { request: _, responder } => {
                trace::duration!(c"storage", c"Directory::AdvisoryLock");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ReadDirents { max_bytes, responder } => {
                async move {
                    let (status, entries) = self.handle_read_dirents(max_bytes).await;
                    responder.send(status.into_raw(), entries.as_slice())
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::ReadDirents"))
                .await?;
            }
            fio::DirectoryRequest::Rewind { responder } => {
                trace::duration!(c"storage", c"Directory::Rewind");
                self.seek = Default::default();
                responder.send(Status::OK.into_raw())?;
            }
            fio::DirectoryRequest::Link { src, dst_parent_token, dst, responder } => {
                async move {
                    let status: Status = self.handle_link(&src, dst_parent_token, dst).await.into();
                    responder.send(status.into_raw())
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::Link"))
                .await?;
            }
            fio::DirectoryRequest::Watch { mask, options, watcher, responder } => {
                trace::duration!(c"storage", c"Directory::Watch");
                let status = if options != 0 {
                    Status::INVALID_ARGS
                } else {
                    let watcher = watcher.try_into()?;
                    self.handle_watch(mask, watcher).into()
                };
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Query { responder } => {
                let () = responder.send(fio::DIRECTORY_PROTOCOL_NAME.as_bytes())?;
            }
            fio::DirectoryRequest::QueryFilesystem { responder } => {
                trace::duration!(c"storage", c"Directory::QueryFilesystem");
                match self.directory.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
            fio::DirectoryRequest::Unlink { name: _, options: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetToken { responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw(), None)?;
            }
            fio::DirectoryRequest::Rename { src: _, dst_parent_token: _, dst: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::CreateSymlink { responder, .. } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::Open3 {
                path,
                mut flags,
                options,
                object,
                control_handle: _,
            } => {
                {
                    trace::duration!(c"storage", c"Directory::Open3");
                    // Remove POSIX flags when the respective rights are not available.
                    if !self.options.rights.contains(fio::INHERITED_WRITE_PERMISSIONS) {
                        flags &= !fio::Flags::PERM_INHERIT_WRITE;
                    }
                    if !self.options.rights.contains(fio::Rights::EXECUTE) {
                        flags &= !fio::Flags::PERM_INHERIT_EXECUTE;
                    }

                    ObjectRequest::new(flags, &options, object)
                        .handle(|req| self.handle_open3(path, flags, req));
                }
                // Since open typically spawns a task, yield to the executor now to give that task a
                // chance to run before we try and process the next request for this directory.
                yield_to_executor().await;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::DirectoryRequest::GetFlags2 { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::DirectoryRequest::SetFlags2 { flags: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::_UnknownMethod { .. } => (),
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_deprecated_clone(
        &self,
        flags: fio::OpenFlags,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        let flags = match inherit_rights_for_clone(self.options.to_io1(), flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        self.directory.clone().open(self.scope.clone(), flags, Path::dot(), server_end);
    }

    fn handle_clone(&mut self, object: fidl::Channel) {
        let flags = self.options.rights.to_flags() | fio::Flags::PROTOCOL_DIRECTORY;
        ObjectRequest::new(flags, &Default::default(), object).handle(|req| {
            self.directory.clone().open3(self.scope.clone(), Path::dot(), flags, req)
        });
    }

    fn handle_open(
        &self,
        mut flags: fio::OpenFlags,
        path: String,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);

        let path = match Path::validate_and_split(path) {
            Ok(path) => path,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        if path.is_dir() {
            flags |= fio::OpenFlags::DIRECTORY;
        }

        let flags = match check_child_connection_flags(self.options.to_io1(), flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };
        if path.is_dot() {
            if flags.intersects(fio::OpenFlags::NOT_DIRECTORY) {
                send_on_open_with_error(describe, server_end, Status::INVALID_ARGS);
                return;
            }
            if flags.intersects(fio::OpenFlags::CREATE_IF_ABSENT) {
                send_on_open_with_error(describe, server_end, Status::ALREADY_EXISTS);
                return;
            }
        }

        // It is up to the open method to handle OPEN_FLAG_DESCRIBE from this point on.
        let directory = self.directory.clone();
        directory.open(self.scope.clone(), flags, path, server_end);
    }

    fn handle_open3(
        &self,
        path: String,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        let path = Path::validate_and_split(path)?;

        // Child connection must have stricter or same rights as the parent connection.
        if let Some(rights) = flags.rights() {
            if rights.intersects(!self.options.rights) {
                return Err(Status::ACCESS_DENIED);
            }
        }

        // If requesting attributes, check permission.
        if !object_request.attributes().is_empty()
            && !self.options.rights.contains(fio::Operations::GET_ATTRIBUTES)
        {
            return Err(Status::ACCESS_DENIED);
        }

        match flags.creation_mode() {
            CreationMode::Never => {
                if object_request.create_attributes().is_some() {
                    return Err(Status::INVALID_ARGS);
                }
            }
            CreationMode::AllowExisting | CreationMode::Always => {
                // The parent connection must be able to modify directories if creating an object.
                if !self.options.rights.contains(fio::Rights::MODIFY_DIRECTORY) {
                    return Err(Status::ACCESS_DENIED);
                }

                let protocol_flags = flags & fio::MASK_KNOWN_PROTOCOLS;
                // If creating an object, exactly one protocol must be specified (the flags must be
                // a power of two and non-zero).
                if protocol_flags.is_empty()
                    || (protocol_flags.bits() & (protocol_flags.bits() - 1)) != 0
                {
                    return Err(Status::INVALID_ARGS);
                }
                // Only a directory or file object can be created.
                if !protocol_flags
                    .intersects(fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::PROTOCOL_FILE)
                {
                    return Err(Status::NOT_SUPPORTED);
                }
            }
        }

        if path.is_dot() {
            if !flags.is_dir_allowed() {
                return Err(Status::INVALID_ARGS);
            }
            if flags.creation_mode() == CreationMode::Always {
                return Err(Status::ALREADY_EXISTS);
            }
        }

        self.directory.clone().open3(self.scope.clone(), path, flags, object_request)
    }

    async fn handle_read_dirents(&mut self, max_bytes: u64) -> (Status, Vec<u8>) {
        async {
            let (new_pos, sealed) =
                self.directory.read_dirents(&self.seek, read_dirents::Sink::new(max_bytes)).await?;
            self.seek = new_pos;
            let read_dirents::Done { buf, status } = *sealed
                .open()
                .downcast::<read_dirents::Done>()
                .map_err(|_: Box<dyn std::any::Any>| {
                    #[cfg(debug)]
                    panic!(
                        "`read_dirents()` returned a `dirents_sink::Sealed`
                        instance that is not an instance of the \
                        `read_dirents::Done`. This is a bug in the \
                        `read_dirents()` implementation."
                    );
                    Status::NOT_SUPPORTED
                })?;
            Ok((status, buf))
        }
        .await
        .unwrap_or_else(|status| (status, Vec::new()))
    }

    async fn handle_link(
        &self,
        source_name: &str,
        target_parent_token: fidl::Handle,
        target_name: String,
    ) -> Result<(), Status> {
        if source_name.contains('/') || target_name.contains('/') {
            return Err(Status::INVALID_ARGS);
        }

        // To avoid rights escalation, we must make sure that the connection to the source directory
        // has the maximal set of file rights.  We do not check for EXECUTE because mutable
        // filesystems that support link don't currently support EXECUTE rights.  The target rights
        // are verified by virtue of the fact that it is not possible to get a token without the
        // MODIFY_DIRECTORY right (see `MutableConnection::handle_get_token`).
        if !self.options.rights.contains(fio::RW_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }

        let target_parent = self
            .scope
            .token_registry()
            .get_owner(target_parent_token)?
            .ok_or(Err(Status::NOT_FOUND))?;

        target_parent.link(target_name, self.directory.clone().into_any(), source_name).await
    }

    fn handle_watch(
        &mut self,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        let directory = self.directory.clone();
        directory.register_watcher(self.scope.clone(), mask, watcher)
    }
}

impl<DirectoryType: Directory> Representation for BaseConnection<DirectoryType> {
    type Protocol = fio::DirectoryMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, Status> {
        Ok(fio::Representation::Directory(fio::DirectoryInfo {
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.directory.get_attributes(requested_attributes).await?)
            },
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        Ok(fio::NodeInfoDeprecated::Directory(fio::DirectoryObject))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::immutable::Simple;
    use assert_matches::assert_matches;
    use fidl_fuchsia_io as fio;
    use futures::prelude::*;

    #[fuchsia::test]
    async fn test_open_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        let dir = Simple::new();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) = fidl::endpoints::create_proxy();

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end
            ),
            Ok(())
        );

        // The channel also be closed with a NOT_FOUND epitaph.
        assert_matches!(
            node_proxy.query().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "fuchsia.io.Node",
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        let dir = Simple::new();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) = fidl::endpoints::create_proxy();

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end
            ),
            Ok(())
        );

        // The event stream should be closed with the epitaph.
        let mut event_stream = node_proxy.take_event_stream();
        assert_matches!(
            event_stream.try_next().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "fuchsia.io.Node",
                ..
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }

    #[fuchsia::test]
    async fn test_open_with_describe_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        let dir = Simple::new();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) = fidl::endpoints::create_proxy();

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::DIRECTORY
                    | fio::OpenFlags::DESCRIBE
                    | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end,
            ),
            Ok(())
        );

        // The channel should be closed with a NOT_FOUND epitaph.
        assert_matches!(
            node_proxy.query().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "fuchsia.io.Node",
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_describe_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        let dir = Simple::new();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) = fidl::endpoints::create_proxy();

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::DIRECTORY
                    | fio::OpenFlags::DESCRIBE
                    | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end,
            ),
            Ok(())
        );

        // The event stream should return that the file does not exist.
        let mut event_stream = node_proxy.take_event_stream();
        assert_matches!(
            event_stream.try_next().await,
            Ok(Some(fio::NodeEvent::OnOpen_ {
                s,
                info: None,
            }))
            if Status::from_raw(s) == Status::NOT_FOUND
        );
        assert_matches!(
            event_stream.try_next().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "fuchsia.io.Node",
                ..
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }
}
