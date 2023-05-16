// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    common::{inherit_rights_for_clone, send_on_open_with_error, IntoAny as _},
    directory::{
        common::check_child_connection_flags,
        connection::util::OpenDirectory,
        entry::DirectoryEntry,
        entry_container::{Directory, DirectoryWatcher},
        mutable::entry_constructor::NewEntryType,
        read_dirents,
        traversal_position::TraversalPosition,
        DirectoryOptions,
    },
    execution_scope::ExecutionScope,
    object_request::Representation,
    path::Path,
    ObjectRequestRef, ProtocolsExt, ToObjectRequest,
};

use {
    anyhow::Error,
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::channel::oneshot,
    pin_project::pin_project,
    std::{
        convert::TryInto as _,
        default::Default,
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    },
};

/// Return type for `BaseConnection::handle_request` and [`DerivedConnection::handle_request`].
pub enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message and should be closed.
    Closed,
}

/// This is an API a derived directory connection needs to implement, in order for the
/// `BaseConnection` to be able to interact with it.
pub trait DerivedConnection: Send + Sync {
    type Directory: Directory + ?Sized;

    /// Whether these connections support mutable connections.
    const MUTABLE: bool;

    fn new(
        scope: ExecutionScope,
        directory: OpenDirectory<Self::Directory>,
        options: DirectoryOptions,
    ) -> Self;

    fn entry_not_found(
        scope: ExecutionScope,
        parent: Arc<dyn DirectoryEntry>,
        entry_type: NewEntryType,
        create: bool,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, zx::Status>;
}

/// Handles functionality shared between mutable and immutable FIDL connections to a directory.  A
/// single directory may contain multiple connections.  Instances of the `BaseConnection`
/// will also hold any state that is "per-connection".  Currently that would be the access flags
/// and the seek position.
pub(in crate::directory) struct BaseConnection<Connection>
where
    Connection: DerivedConnection + 'static,
{
    /// Execution scope this connection and any async operations and connections it creates will
    /// use.
    pub(in crate::directory) scope: ExecutionScope,

    pub(in crate::directory) directory: OpenDirectory<Connection::Directory>,

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

/// Takes a stream and a shutdown receiver and creates a new stream that will terminate when the
/// shutdown receiver is ready (and ignore any outstanding items in the original stream).
#[pin_project]
pub struct StreamWithShutdown<T: futures::Stream>(#[pin] T, #[pin] oneshot::Receiver<()>);

impl<T: futures::Stream> futures::Stream for StreamWithShutdown<T> {
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if let Poll::Ready(Ok(())) = this.1.poll(cx) {
            return Poll::Ready(None);
        }
        this.0.poll_next(cx)
    }
}

pub trait WithShutdown {
    fn with_shutdown(self, shutdown: oneshot::Receiver<()>) -> StreamWithShutdown<Self>
    where
        Self: futures::Stream + Sized,
    {
        StreamWithShutdown(self, shutdown)
    }
}

impl<T: futures::Stream> WithShutdown for T {}

impl<Connection> BaseConnection<Connection>
where
    Connection: DerivedConnection,
{
    /// Constructs an instance of `BaseConnection` - to be used by derived connections, when they
    /// need to create a nested `BaseConnection` "sub-object".  But when implementing
    /// `create_connection`, derived connections should use the [`create_connection`] call.
    pub(in crate::directory) fn new(
        scope: ExecutionScope,
        directory: OpenDirectory<Connection::Directory>,
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
            fio::DirectoryRequest::Clone { flags, object, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "Directory::Clone");
                self.handle_clone(flags, object);
            }
            fio::DirectoryRequest::Reopen {
                rights_request: _,
                object_request,
                control_handle: _,
            } => {
                fuchsia_trace::duration!("storage", "Directory::Reopen");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            fio::DirectoryRequest::Close { responder } => {
                fuchsia_trace::duration!("storage", "Directory::Close");
                responder.send(&mut self.directory.close().map_err(|status| status.into_raw()))?;
                return Ok(ConnectionState::Closed);
            }
            fio::DirectoryRequest::GetConnectionInfo { responder } => {
                fuchsia_trace::duration!("storage", "Directory::GetConnectionInfo");
                // TODO(https://fxbug.dev/77623): Restrict GET_ATTRIBUTES, ENUMERATE, and TRAVERSE.
                // TODO(https://fxbug.dev/77623): Implement MODIFY_DIRECTORY and UPDATE_ATTRIBUTES.
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::DirectoryRequest::GetAttr { responder } => {
                fuchsia_trace::duration!("storage", "Directory::GetAttr");
                let (attrs, status) = match self.directory.get_attrs().await {
                    Ok(attrs) => (attrs, zx::Status::OK.into_raw()),
                    Err(status) => (
                        fio::NodeAttributes {
                            mode: 0,
                            id: fio::INO_UNKNOWN,
                            content_size: 0,
                            storage_size: 0,
                            link_count: 1,
                            creation_time: 0,
                            modification_time: 0,
                        },
                        status.into_raw(),
                    ),
                };
                responder.send(status, &attrs)?;
            }
            fio::DirectoryRequest::GetAttributes { query: _, responder } => {
                fuchsia_trace::duration!("storage", "Directory::GetAttributes");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::UpdateAttributes { payload: _, responder } => {
                fuchsia_trace::duration!("storage", "Directory::UpdateAttributes");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator, .. } => {
                fuchsia_trace::duration!("storage", "Directory::ListExtendedAttributes");
                iterator.close_with_epitaph(zx::Status::NOT_SUPPORTED)?;
            }
            fio::DirectoryRequest::GetExtendedAttribute { responder, .. } => {
                fuchsia_trace::duration!("storage", "Directory::GetExtendedAttribute");
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetExtendedAttribute { responder, .. } => {
                fuchsia_trace::duration!("storage", "Directory::SetExtendedAttribute");
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { responder, .. } => {
                fuchsia_trace::duration!("storage", "Directory::RemoveExtendedAttribute");
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetFlags { responder } => {
                fuchsia_trace::duration!("storage", "Directory::GetFlags");
                responder.send(zx::Status::OK.into_raw(), self.options.to_io1())?;
            }
            fio::DirectoryRequest::SetFlags { flags: _, responder } => {
                fuchsia_trace::duration!("storage", "Directory::SetFlags");
                responder.send(zx::Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Open { flags, mode, path, object, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "Directory::Open");
                // Temporarily allow clients that send POSIX modes when creating directories to succeed.
                //
                // TODO(https://fxbug.dev/120673): Remove this when all downstream consumers contain this commit.
                let flags = if mode.bits() & fio::MODE_TYPE_MASK == fio::MODE_TYPE_DIRECTORY {
                    flags | fio::OpenFlags::DIRECTORY
                } else {
                    flags
                };
                self.handle_open(flags, path, object);
            }
            fio::DirectoryRequest::Open2 {
                path,
                mut protocols,
                object_request,
                control_handle: _,
            } => {
                fuchsia_trace::duration!("storage", "Directory::Open2");
                // Fill in rights from the parent connection if it's absent.
                if let fio::ConnectionProtocols::Node(fio::NodeOptions { rights, .. }) =
                    &mut protocols
                {
                    if rights.is_none() {
                        *rights = Some(self.options.rights);
                    }
                }
                protocols
                    .to_object_request(object_request)
                    .handle(|req| self.handle_open2(path, protocols, req));
            }
            fio::DirectoryRequest::AddInotifyFilter {
                path,
                filter,
                watch_descriptor,
                socket: _,
                responder,
            } => {
                fuchsia_trace::duration!("storage", "Directory::AddInotifyFilter");
                tracing::error!(
                    %path,
                    ?filter,
                    watch_descriptor,
                    "AddInotifyFilter not implemented: https://fxbug.dev/77623"
                );
                responder.send()?;
            }
            fio::DirectoryRequest::AdvisoryLock { request: _, responder } => {
                fuchsia_trace::duration!("storage", "Directory::AdvisoryLock");
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ReadDirents { max_bytes, responder } => {
                fuchsia_trace::duration!("storage", "Directory::ReadDirents");
                let (status, entries) = self.handle_read_dirents(max_bytes).await;
                responder.send(status.into_raw(), entries.as_slice())?;
            }
            fio::DirectoryRequest::Enumerate { options: _, iterator, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "Directory::Enumerate");
                // TODO(https://fxbug.dev/77623): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `iterator` channel was provided.
                let _ = iterator.close_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
            fio::DirectoryRequest::Rewind { responder } => {
                fuchsia_trace::duration!("storage", "Directory::Rewind");
                self.seek = Default::default();
                responder.send(zx::Status::OK.into_raw())?;
            }
            fio::DirectoryRequest::Link { src, dst_parent_token, dst, responder } => {
                fuchsia_trace::duration!("storage", "Directory::Link");
                let status: zx::Status = self.handle_link(&src, dst_parent_token, dst).await.into();
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Watch { mask, options, watcher, responder } => {
                fuchsia_trace::duration!("storage", "Directory::Watch");
                let status = if options != 0 {
                    zx::Status::INVALID_ARGS
                } else {
                    let watcher = watcher.try_into()?;
                    self.handle_watch(mask, watcher).into()
                };
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Query { responder } => {
                let () = responder.send(
                    if self.options.node {
                        fio::NODE_PROTOCOL_NAME
                    } else {
                        fio::DIRECTORY_PROTOCOL_NAME
                    }
                    .as_bytes(),
                )?;
            }
            fio::DirectoryRequest::QueryFilesystem { responder } => {
                fuchsia_trace::duration!("storage", "Directory::QueryFilesystem");
                match self.directory.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
            fio::DirectoryRequest::Unlink { name: _, options: _, responder } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetToken { responder } => {
                responder.send(zx::Status::NOT_SUPPORTED.into_raw(), None)?;
            }
            fio::DirectoryRequest::Rename { src: _, dst_parent_token: _, dst: _, responder } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(zx::Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::CreateSymlink { responder, .. } => {
                responder.send(&mut Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
            }
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone(&self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
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

    fn handle_open(
        &self,
        mut flags: fio::OpenFlags,
        path: String,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        if self.options.node {
            send_on_open_with_error(describe, server_end, zx::Status::BAD_HANDLE);
            return;
        }

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
                send_on_open_with_error(describe, server_end, zx::Status::INVALID_ARGS);
                return;
            }
            if flags.intersects(fio::OpenFlags::CREATE_IF_ABSENT) {
                send_on_open_with_error(describe, server_end, zx::Status::ALREADY_EXISTS);
                return;
            }
        }

        // It is up to the open method to handle OPEN_FLAG_DESCRIBE from this point on.
        let directory = self.directory.clone();
        directory.open(self.scope.clone(), flags, path, server_end);
    }

    fn handle_open2(
        &self,
        path: String,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        let path = Path::validate_and_split(path)?;

        if let Some(rights) = protocols.rights() {
            if rights.intersects(!self.options.rights) {
                return Err(zx::Status::ACCESS_DENIED);
            }
        }

        // If creating an object, it's not legal to specify more than one protocol.
        if protocols.open_mode() != fio::OpenMode::OpenExisting
            && ((protocols.is_file_allowed() && protocols.is_dir_allowed())
                || protocols.is_symlink_allowed())
        {
            return Err(zx::Status::INVALID_ARGS);
        }

        if protocols.create_attributes().is_some() {
            if protocols.open_mode() == fio::OpenMode::OpenExisting {
                return Err(zx::Status::INVALID_ARGS);
            }
            // TODO(fxbug.dev/77623): Support setting attributes at creation time.
            return Err(zx::Status::NOT_SUPPORTED);
        }

        if path.is_dot() {
            if !protocols.is_dir_allowed() {
                return Err(zx::Status::INVALID_ARGS);
            }
            if protocols.open_mode() == fio::OpenMode::AlwaysCreate {
                return Err(zx::Status::ALREADY_EXISTS);
            }
        }

        self.directory.clone().open2(self.scope.clone(), path, protocols, object_request)
    }

    async fn handle_read_dirents(&mut self, max_bytes: u64) -> (zx::Status, Vec<u8>) {
        async {
            if self.options.node {
                return Err(zx::Status::BAD_HANDLE);
            }

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
                    zx::Status::NOT_SUPPORTED
                })?;
            Ok((status, buf))
        }
        .await
        .unwrap_or_else(|status| (status, Vec::new()))
    }

    async fn handle_link(
        &self,
        source_name: &str,
        target_parent_token: zx::Handle,
        target_name: String,
    ) -> Result<(), zx::Status> {
        if source_name.contains('/') || target_name.contains('/') {
            return Err(zx::Status::INVALID_ARGS);
        }

        let DirectoryOptions { node, rights } = self.options;
        if node {
            // TODO(https://fxbug.dev/77623): should this be an error?
            // return Err(zx::Status::NOT_SUPPORTED);
        }
        if !rights.contains(fio::W_STAR_DIR) {
            return Err(zx::Status::BAD_HANDLE);
        }

        let (target_parent, _flags) = self
            .scope
            .token_registry()
            .get_owner(target_parent_token)?
            .ok_or(Err(zx::Status::NOT_FOUND))?;

        target_parent.link(target_name, self.directory.clone().into_any(), source_name).await
    }

    fn handle_watch(
        &mut self,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        let directory = self.directory.clone();
        directory.register_watcher(self.scope.clone(), mask, watcher)
    }
}

#[async_trait]
impl<T: DerivedConnection + 'static> Representation for BaseConnection<T> {
    type Protocol = fio::DirectoryMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, zx::Status> {
        Ok(fio::Representation::Directory(fio::DirectoryInfo {
            attributes: Some(self.directory.get_attributes(requested_attributes).await?),
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, zx::Status> {
        Ok(if self.options.node {
            fio::NodeInfoDeprecated::Service(fio::Service)
        } else {
            fio::NodeInfoDeprecated::Directory(fio::DirectoryObject)
        })
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::directory::immutable::simple::simple, assert_matches::assert_matches,
        fidl_fuchsia_io as fio, fuchsia_zircon as zx, futures::prelude::*,
    };

    #[fuchsia::test]
    async fn test_open_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

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
                status: zx::Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

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
                status: zx::Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }

    #[fuchsia::test]
    async fn test_open_with_describe_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

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
                status: zx::Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_describe_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

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
            if zx::Status::from_raw(s) == zx::Status::NOT_FOUND
        );
        assert_matches!(
            event_stream.try_next().await,
            Err(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }

    #[fuchsia::test]
    async fn test_add_inotify_filter_does_not_crash() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (send_socket, _keep_socket) = fidl::Socket::create_stream();

        assert_matches!(
            dir_proxy
                .add_inotify_filter("foo", fio::InotifyWatchMask::ACCESS, 42, send_socket)
                .await,
            Ok(())
        );
    }
}
