// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common trait for all the directory entry objects.

#![warn(missing_docs)]

use crate::common::IntoAny;
use crate::directory::entry_container::Directory;
use crate::execution_scope::ExecutionScope;
use crate::file::{self, FileLike};
use crate::object_request::ObjectRequestSend;
use crate::path::Path;
use crate::service::{self, ServiceLike};
use crate::symlink::{self, Symlink};
use crate::{ObjectRequestRef, ToObjectRequest};

use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_io as fio;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use zx_status::Status;

/// Information about a directory entry, used to populate ReadDirents() output.
/// The first element is the inode number, or INO_UNKNOWN (from fuchsia.io) if not set, and the second
/// element is one of the DIRENT_TYPE_* constants defined in the fuchsia.io.
#[derive(PartialEq, Eq, Clone)]
pub struct EntryInfo(u64, fio::DirentType);

impl EntryInfo {
    /// Constructs a new directory entry information object.
    pub fn new(inode: u64, type_: fio::DirentType) -> Self {
        Self(inode, type_)
    }

    /// Retrives the `inode` argument of the [`EntryInfo::new()`] constructor.
    pub fn inode(&self) -> u64 {
        let Self(inode, _type) = self;
        *inode
    }

    /// Retrieves the `type_` argument of the [`EntryInfo::new()`] constructor.
    pub fn type_(&self) -> fio::DirentType {
        let Self(_inode, type_) = self;
        *type_
    }
}

impl fmt::Debug for EntryInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inode, type_) = self;
        if *inode == fio::INO_UNKNOWN {
            write!(f, "{:?}(fio::INO_UNKNOWN)", type_)
        } else {
            write!(f, "{:?}({})", type_, inode)
        }
    }
}

/// Give useful information about the entry, for example, the directory entry type.
pub trait GetEntryInfo {
    /// This method is used to populate ReadDirents() output.
    fn entry_info(&self) -> EntryInfo;
}

/// Pseudo directories contain items that implement this trait.  Pseudo directories refer to the
/// items they contain as `Arc<dyn DirectoryEntry>`.
///
/// *NOTE*: This trait only needs to be implemented if you want to add your nodes to a pseudo
/// directory.
pub trait DirectoryEntry: GetEntryInfo + IntoAny + Sync + Send + 'static {
    /// Opens this entry.
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status>;
}

/// Trait that can be implemented to process open requests asynchronously.
pub trait DirectoryEntryAsync: DirectoryEntry {
    /// Implementers may use this if desired by using the `spawn` method below.
    fn open_entry_async(
        self: Arc<Self>,
        request: OpenRequest<'_>,
    ) -> impl Future<Output = Result<(), Status>> + Send;
}

/// An open request.
#[derive(Debug)]
pub struct OpenRequest<'a> {
    scope: ExecutionScope,
    request_flags: RequestFlags,
    path: Path,
    object_request: ObjectRequestRef<'a>,
}

/// Wraps flags used for open requests based on which fuchsia.io/Directory.Open method was used.
/// Used to delegate [`OpenRequest`] to the corresponding method when the entry is opened.
#[derive(Debug)]
pub enum RequestFlags {
    /// fuchsia.io/Directory.Open1 (io1)
    Open1(fio::OpenFlags),
    /// fuchsia.io/Directory.Open3 (io2)
    Open3(fio::Flags),
}

impl From<fio::OpenFlags> for RequestFlags {
    fn from(value: fio::OpenFlags) -> Self {
        RequestFlags::Open1(value)
    }
}

impl From<fio::Flags> for RequestFlags {
    fn from(value: fio::Flags) -> Self {
        RequestFlags::Open3(value)
    }
}

impl<'a> OpenRequest<'a> {
    /// Creates a new open request.
    pub fn new(
        scope: ExecutionScope,
        request_flags: impl Into<RequestFlags>,
        path: Path,
        object_request: ObjectRequestRef<'a>,
    ) -> Self {
        Self { scope, request_flags: request_flags.into(), path, object_request }
    }

    /// Returns the path for this request.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Prepends `prefix` to the path.
    pub fn prepend_path(&mut self, prefix: &Path) {
        self.path = self.path.with_prefix(prefix);
    }

    /// Sets the path to `path`.
    pub fn set_path(&mut self, path: Path) {
        self.path = path;
    }

    /// Waits until the request has a request waiting in its channel.  Returns immediately if this
    /// request requires sending an initial event such as OnOpen or OnRepresentation.  Returns
    /// `true` if the channel is readable (rather than just clased).
    pub async fn wait_till_ready(&self) -> bool {
        self.object_request.wait_till_ready().await
    }

    /// Returns `true` if the request requires the server to send an event (e.g. either OnOpen or
    /// OnRepresentation).  If `true`, `wait_till_ready` will return immediately.  If `false`, the
    /// caller might choose to call `wait_till_ready` if other conditions are satisfied (checking
    /// for an empty path is usually a good idea since it is hard to know where a non-empty path
    /// might end up being terminated).
    pub fn requires_event(&self) -> bool {
        self.object_request.what_to_send() != ObjectRequestSend::Nothing
    }

    /// Opens a directory.
    pub fn open_dir(self, dir: Arc<impl Directory>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open1(flags),
                path,
                object_request,
            } => {
                dir.open(scope, flags, path, object_request.take().into_server_end());
                // This will cause issues for heavily nested directory structures because it thwarts
                // tail recursion optimization, but that shouldn't occur in practice.
                Ok(())
            }
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open3(flags),
                path,
                object_request,
            } => dir.open3(scope, path, flags, object_request),
        }
    }

    /// Opens a file.
    pub fn open_file(self, file: Arc<impl FileLike>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open1(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                file::serve(file, scope, &flags, object_request)
            }
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open3(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                file::serve(file, scope, &flags, object_request)
            }
        }
    }

    /// Opens a symlink.
    pub fn open_symlink(self, service: Arc<impl Symlink>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open1(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                symlink::serve(service, scope, &flags, object_request)
            }
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open3(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                symlink::serve(service, scope, &flags, object_request)
            }
        }
    }

    /// Opens a service.
    pub fn open_service(self, service: Arc<impl ServiceLike>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open1(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                service::serve(service, scope, &flags, object_request)
            }
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open3(flags),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                service::serve(service, scope, &flags, object_request)
            }
        }
    }

    /// Forwards the request to a remote.
    pub fn open_remote(
        self,
        remote: Arc<impl crate::remote::RemoteLike + Send + Sync + 'static>,
    ) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open1(flags),
                path,
                object_request,
            } => {
                if object_request.what_to_send() == ObjectRequestSend::Nothing && remote.lazy(&path)
                {
                    let object_request = object_request.take();
                    scope.clone().spawn(async move {
                        if object_request.wait_till_ready().await {
                            remote.open(scope, flags, path, object_request.into_server_end());
                        }
                    });
                } else {
                    remote.open(scope, flags, path, object_request.take().into_server_end());
                }
                Ok(())
            }
            OpenRequest {
                scope,
                request_flags: RequestFlags::Open3(flags),
                path,
                object_request,
            } => {
                if object_request.what_to_send() == ObjectRequestSend::Nothing && remote.lazy(&path)
                {
                    let object_request = object_request.take();
                    scope.clone().spawn(async move {
                        object_request.handle(|object_request| {
                            remote.open3(scope, path, flags, object_request)
                        });
                    });
                    Ok(())
                } else {
                    remote.open3(scope, path, flags, object_request)
                }
            }
        }
    }

    /// Spawns a task to handle the request.  `entry` must implement DirectoryEntryAsync.
    pub fn spawn(self, entry: Arc<impl DirectoryEntryAsync>) {
        let OpenRequest { scope, request_flags, path, object_request } = self;
        let mut object_request = object_request.take();
        match request_flags {
            RequestFlags::Open1(flags) => {
                scope.clone().spawn(async move {
                    match entry
                        .open_entry_async(OpenRequest::new(
                            scope,
                            RequestFlags::Open1(flags),
                            path,
                            &mut object_request,
                        ))
                        .await
                    {
                        Ok(()) => {}
                        Err(s) => object_request.shutdown(s),
                    }
                });
            }
            RequestFlags::Open3(flags) => {
                scope.clone().spawn(async move {
                    match entry
                        .open_entry_async(OpenRequest::new(
                            scope,
                            RequestFlags::Open3(flags),
                            path,
                            &mut object_request,
                        ))
                        .await
                    {
                        Ok(()) => {}
                        Err(s) => object_request.shutdown(s),
                    }
                });
            }
        }
    }

    /// Returns the execution scope for this request.
    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }

    /// Replaces the scope in this request.  This is the right thing to do if any subsequently
    /// spawned tasks should be in a different scope to the task that received this open request.
    pub fn set_scope(&mut self, scope: ExecutionScope) {
        self.scope = scope;
    }
}

/// A sub-node of a directory.  This will work with types that implement Directory as well as
/// RemoteDir.
pub struct SubNode<T: ?Sized> {
    parent: Arc<T>,
    path: Path,
    entry_type: fio::DirentType,
}

impl<T: DirectoryEntry + ?Sized> SubNode<T> {
    /// Returns a sub node of an existing entry.  The parent should be a directory (it accepts
    /// DirectoryEntry so that it works for remotes).
    pub fn new(parent: Arc<T>, path: Path, entry_type: fio::DirentType) -> SubNode<T> {
        assert_eq!(parent.entry_info().type_(), fio::DirentType::Directory);
        Self { parent, path, entry_type }
    }
}

impl<T: DirectoryEntry + ?Sized> GetEntryInfo for SubNode<T> {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
    }
}

impl<T: DirectoryEntry + ?Sized> DirectoryEntry for SubNode<T> {
    fn open_entry(self: Arc<Self>, mut request: OpenRequest<'_>) -> Result<(), Status> {
        request.path = request.path.with_prefix(&self.path);
        self.parent.clone().open_entry(request)
    }
}

/// Serves a directory with the given rights.  Returns a client end.  This takes a DirectoryEntry
/// so that it works for remotes.
pub fn serve_directory(
    dir: Arc<impl DirectoryEntry + ?Sized>,
    scope: &ExecutionScope,
    flags: fio::OpenFlags,
) -> Result<ClientEnd<fio::DirectoryMarker>, Status> {
    assert_eq!(dir.entry_info().type_(), fio::DirentType::Directory);
    let (client, server) = create_endpoints::<fio::DirectoryMarker>();
    flags
        .to_object_request(server)
        .handle(|object_request| {
            Ok(dir.open_entry(OpenRequest::new(scope.clone(), flags, Path::dot(), object_request)))
        })
        .unwrap()?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::{
        DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest, RequestFlags, SubNode,
    };
    use crate::directory::entry::GetEntryInfo;
    use crate::directory::entry_container::Directory;
    use crate::execution_scope::ExecutionScope;
    use crate::file::read_only;
    use crate::path::Path;
    use crate::{assert_read, pseudo_directory, ObjectRequest, ToObjectRequest};
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy, ClientEnd};
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use std::sync::Arc;
    use zx_status::Status;

    #[fuchsia::test]
    async fn sub_node() {
        let root = pseudo_directory!(
            "a" => pseudo_directory!(
                "b" => pseudo_directory!(
                    "c" => pseudo_directory!(
                        "d" => read_only(b"foo")
                    )
                )
            )
        );
        let sub_node = Arc::new(SubNode::new(
            root,
            Path::validate_and_split("a/b").unwrap(),
            fio::DirentType::Directory,
        ));
        let scope = ExecutionScope::new();
        let (client, server) = create_endpoints();

        let root2 = pseudo_directory!(
            "e" => sub_node
        );

        root2.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            Path::validate_and_split("e/c/d").unwrap(),
            server,
        );
        assert_read!(ClientEnd::<fio::FileMarker>::from(client.into_channel()).into_proxy(), "foo");
    }

    #[fuchsia::test]
    async fn object_request_spawn() {
        struct MockNode<F: Send + Sync + 'static>
        where
            for<'a> F: Fn(OpenRequest<'a>) -> Status,
        {
            callback: F,
        }
        impl<F: Send + Sync + 'static> DirectoryEntry for MockNode<F>
        where
            for<'a> F: Fn(OpenRequest<'a>) -> Status,
        {
            fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
                request.spawn(self);
                Ok(())
            }
        }
        impl<F: Send + Sync + 'static> GetEntryInfo for MockNode<F>
        where
            for<'a> F: Fn(OpenRequest<'a>) -> Status,
        {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Unknown)
            }
        }
        impl<F: Send + Sync + 'static> DirectoryEntryAsync for MockNode<F>
        where
            for<'a> F: Fn(OpenRequest<'a>) -> Status,
        {
            async fn open_entry_async(
                self: Arc<Self>,
                request: OpenRequest<'_>,
            ) -> Result<(), Status> {
                Err((self.callback)(request))
            }
        }

        let scope = ExecutionScope::new();
        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        let flags = fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE;
        let mut object_request = flags.to_object_request(server);

        let flags_copy = flags;
        Arc::new(MockNode {
            callback: move |request| {
                assert_matches!(
                    request,
                    OpenRequest {
                        request_flags: RequestFlags::Open1(f),
                        path,
                        ..
                    } if f == flags_copy && path.as_ref() == "a/b/c"
                );
                Status::BAD_STATE
            },
        })
        .open_entry(OpenRequest::new(
            scope.clone(),
            flags,
            "a/b/c".try_into().unwrap(),
            &mut object_request,
        ))
        .unwrap();

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status, .. }))
                if status == Status::BAD_STATE
        );

        let (proxy, server) = create_proxy::<fio::NodeMarker>();
        let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::FILE_APPEND;
        let mut object_request =
            ObjectRequest::new3(flags, &Default::default(), server.into_channel());

        Arc::new(MockNode {
            callback: move |request| {
                assert_matches!(
                    request,
                    OpenRequest {
                        request_flags: RequestFlags::Open3(f),
                        path,
                        ..
                    } if f == flags && path.as_ref() == "a/b/c"
                );
                Status::BAD_STATE
            },
        })
        .open_entry(OpenRequest::new(
            scope.clone(),
            flags,
            "a/b/c".try_into().unwrap(),
            &mut object_request,
        ))
        .unwrap();

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Err(fidl::Error::ClientChannelClosed { status, .. }))
                if status == Status::BAD_STATE
        );
    }
}
