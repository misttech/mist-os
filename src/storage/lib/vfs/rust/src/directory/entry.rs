// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common trait for all the directory entry objects.

#![warn(missing_docs)]

use crate::{
    common::IntoAny,
    directory::entry_container::Directory,
    execution_scope::ExecutionScope,
    file::{self, FileLike},
    node::IsDirectory,
    path::Path,
    service::{self, ServiceLike},
    symlink::{self, Symlink},
    ObjectRequestRef, ToObjectRequest,
};

use {
    fidl::endpoints::{create_endpoints, ClientEnd},
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::{fmt, sync::Arc},
};

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

/// Pseudo directories contain items that implement this trait.  Pseudo directories refer to the
/// items they contain as `Arc<dyn DirectoryEntry>`.
///
/// *NOTE*: This trait only needs to be implemented if you want to add your nodes to a pseudo
/// directory.  If you don't need to add your nodes to a pseudo directory, consider implementing
/// node::IsDirectory instead.
pub trait DirectoryEntry: IntoAny + Sync + Send + 'static {
    /// This method is used to populate ReadDirents() output.
    fn entry_info(&self) -> EntryInfo;

    /// Opens this entry.
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status>;
}

impl<T: DirectoryEntry> IsDirectory for T {
    fn is_directory(&self) -> bool {
        self.entry_info().type_() == fio::DirentType::Directory
    }
}

/// An open request.
pub struct OpenRequest<'a> {
    scope: ExecutionScope,
    flags_or_protocols: FlagsOrProtocols<'a>,
    path: Path,
    object_request: ObjectRequestRef<'a>,
}

/// Wraps flags (io1) or protocols (io2).
pub enum FlagsOrProtocols<'a> {
    /// io1
    Flags(fio::OpenFlags),
    /// io2
    Protocols(&'a fio::ConnectionProtocols),
}

impl From<fio::OpenFlags> for FlagsOrProtocols<'_> {
    fn from(value: fio::OpenFlags) -> Self {
        FlagsOrProtocols::Flags(value)
    }
}

impl<'a> From<&'a fio::ConnectionProtocols> for FlagsOrProtocols<'a> {
    fn from(value: &'a fio::ConnectionProtocols) -> Self {
        FlagsOrProtocols::Protocols(value)
    }
}

impl<'a> OpenRequest<'a> {
    /// Creates a new open request.
    pub fn new(
        scope: ExecutionScope,
        flags_or_protocols: impl Into<FlagsOrProtocols<'a>>,
        path: Path,
        object_request: ObjectRequestRef<'a>,
    ) -> Self {
        Self { scope, flags_or_protocols: flags_or_protocols.into(), path, object_request }
    }

    /// Opens a directory.
    pub fn open_dir(self, dir: Arc<impl Directory>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Flags(flags),
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
                flags_or_protocols: FlagsOrProtocols::Protocols(protocols),
                path,
                object_request,
            } => {
                // We should fix the copy of protocols here.
                dir.open2(scope, path, protocols.clone(), object_request)
            }
        }
    }

    /// Opens a file.
    pub fn open_file(self, file: Arc<impl FileLike>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Flags(flags),
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
                flags_or_protocols: FlagsOrProtocols::Protocols(protocols),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                file::serve(file, scope, protocols, object_request)
            }
        }
    }

    /// Opens a symlink.
    pub fn open_symlink(self, service: Arc<impl Symlink>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Flags(flags),
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
                flags_or_protocols: FlagsOrProtocols::Protocols(protocols),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                symlink::serve(service, scope, protocols, object_request)
            }
        }
    }

    /// Opens a service.
    pub fn open_service(self, service: Arc<impl ServiceLike>) -> Result<(), Status> {
        match self {
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Flags(flags),
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
                flags_or_protocols: FlagsOrProtocols::Protocols(protocols),
                path,
                object_request,
            } => {
                if !path.is_empty() {
                    return Err(Status::NOT_DIR);
                }
                service::serve(service, scope, protocols, object_request)
            }
        }
    }

    /// Forwards the request to a remote.
    pub fn open_remote(
        self,
        remote: Arc<impl crate::remote::RemoteLike + Send + Sync + 'static>,
    ) -> Result<(), Status> {
        use crate::object_request::ObjectRequestSend;
        match self {
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Flags(flags),
                path,
                object_request,
            } => {
                if object_request.what_to_send() == ObjectRequestSend::Nothing && remote.lazy(&path)
                {
                    let object_request = object_request.take();
                    scope.clone().spawn(async move {
                        object_request.wait_till_ready().await;
                        remote.open(scope, flags, path, object_request.into_server_end());
                    });
                } else {
                    remote.open(scope, flags, path, object_request.take().into_server_end());
                }
                Ok(())
            }
            OpenRequest {
                scope,
                flags_or_protocols: FlagsOrProtocols::Protocols(protocols),
                path,
                object_request,
            } => {
                let protocols = protocols.clone();
                if object_request.what_to_send() == ObjectRequestSend::Nothing && remote.lazy(&path)
                {
                    let object_request = object_request.take();
                    scope.clone().spawn(async move {
                        object_request.wait_till_ready().await;
                        object_request.handle(|object_request| {
                            remote.open2(scope, path, protocols, object_request)
                        });
                    });
                    Ok(())
                } else {
                    remote.open2(scope, path, protocols, object_request)
                }
            }
        }
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

impl<T: DirectoryEntry + ?Sized> DirectoryEntry for SubNode<T> {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
    }

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

#[cfg(all(test, target_os = "fuchsia"))]
mod tests {
    use {
        super::SubNode,
        crate::{
            assert_read, directory::entry_container::Directory, execution_scope::ExecutionScope,
            file::vmo::read_only, path::Path, pseudo_directory,
        },
        fidl::endpoints::{create_endpoints, ClientEnd},
        fidl_fuchsia_io as fio,
        std::sync::Arc,
    };

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
        assert_read!(
            ClientEnd::<fio::FileMarker>::from(client.into_channel()).into_proxy().unwrap(),
            "foo"
        );
    }
}
