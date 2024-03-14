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
    ObjectRequestRef,
};

use {
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
    #[cfg(target_os = "fuchsia")]
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
