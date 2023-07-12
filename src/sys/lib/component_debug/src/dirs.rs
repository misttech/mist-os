// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Convenience functions for accessing directories of a component instance
//! and opening protocols that exist in them.

use {
    fidl::endpoints::{create_proxy, ProtocolMarker},
    fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys,
    moniker::AbsoluteMoniker,
    thiserror::Error,
};

/// Errors that can be returned from opening a component instance directory.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("instance {0} could not be found")]
    InstanceNotFound(AbsoluteMoniker),
    #[error("opening {0} requires {1} to be resolved")]
    InstanceNotResolved(OpenDirType, AbsoluteMoniker),
    #[error("opening {0} requires {1} to be running")]
    InstanceNotRunning(OpenDirType, AbsoluteMoniker),
    #[error("component manager's open request on the directory returned a FIDL error")]
    OpenFidlError,
    #[error("{0} does not have a {1}")]
    NoSuchDir(AbsoluteMoniker, OpenDirType),
    #[error("component manager could not parse moniker: {0}")]
    BadMoniker(AbsoluteMoniker),
    #[error("component manager could not parse dir type: {0}")]
    BadDirType(OpenDirType),
    #[error("component manager could not parse path: {0}")]
    BadPath(String),
    #[error("component manager responded with an unknown error code")]
    UnknownError,
    #[error(transparent)]
    Fidl(#[from] fidl::Error),
}

/// The directories of a component instance that can be opened.
#[derive(Clone, Debug)]
pub enum OpenDirType {
    /// Served by the component's program. Rights unknown.
    Outgoing,
    /// Served by the component's runner. Rights unknown.
    Runtime,
    /// Served by the component's resolver. Rights unknown.
    Package,
    /// Served by component manager. Directory has RW rights.
    Exposed,
    /// Served by component manager. Directory has RW rights.
    Namespace,
}

impl std::fmt::Display for OpenDirType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outgoing => write!(f, "outgoing directory"),
            Self::Runtime => write!(f, "runtime directory"),
            Self::Package => write!(f, "package directory"),
            Self::Exposed => write!(f, "exposed directory"),
            Self::Namespace => write!(f, "namespace directory"),
        }
    }
}

impl Into<fsys::OpenDirType> for OpenDirType {
    fn into(self) -> fsys::OpenDirType {
        match self {
            Self::Outgoing => fsys::OpenDirType::OutgoingDir,
            Self::Runtime => fsys::OpenDirType::RuntimeDir,
            Self::Package => fsys::OpenDirType::PackageDir,
            Self::Exposed => fsys::OpenDirType::ExposedDir,
            Self::Namespace => fsys::OpenDirType::NamespaceDir,
        }
    }
}

/// Opens a protocol in a component instance directory, assuming it is located at the root.
pub async fn connect_to_instance_protocol_at_dir_root<P: ProtocolMarker>(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    realm: &fsys::RealmQueryProxy,
) -> Result<P::Proxy, OpenError> {
    connect_to_instance_protocol_at_path::<P>(moniker, dir_type, P::DEBUG_NAME, realm).await
}

/// Opens a protocol in a component instance directory, assuming it is located under `/svc`.
pub async fn connect_to_instance_protocol_at_dir_svc<P: ProtocolMarker>(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    realm: &fsys::RealmQueryProxy,
) -> Result<P::Proxy, OpenError> {
    let path = format!("/svc/{}", P::DEBUG_NAME);
    connect_to_instance_protocol_at_path::<P>(moniker, dir_type, &path, realm).await
}

/// Opens a protocol in a component instance directory at the given |path|.
pub async fn connect_to_instance_protocol_at_path<P: ProtocolMarker>(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    path: &str,
    realm: &fsys::RealmQueryProxy,
) -> Result<P::Proxy, OpenError> {
    let (proxy, server_end) = create_proxy::<P>().unwrap();
    let server_end = server_end.into_channel();
    open_in_instance_dir(
        &moniker,
        dir_type,
        fio::OpenFlags::RIGHT_READABLE,
        fio::ModeType::empty(),
        path,
        server_end,
        realm,
    )
    .await?;
    Ok(proxy)
}

/// Opens the root of a component instance directory with read rights.
pub async fn open_instance_dir_root_readable(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    realm: &fsys::RealmQueryProxy,
) -> Result<fio::DirectoryProxy, OpenError> {
    let (root_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = server_end.into_channel();
    open_in_instance_dir(
        moniker,
        dir_type,
        fio::OpenFlags::RIGHT_READABLE,
        fio::ModeType::empty(),
        ".",
        server_end,
        realm,
    )
    .await?;
    Ok(root_dir)
}

/// Opens the subdirectory of a component instance directory with read rights.
pub async fn open_instance_subdir_readable(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    path: &str,
    realm: &fsys::RealmQueryProxy,
) -> Result<fio::DirectoryProxy, OpenError> {
    let (root_dir, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = server_end.into_channel();
    open_in_instance_dir(
        moniker,
        dir_type,
        fio::OpenFlags::RIGHT_READABLE,
        fio::ModeType::empty(),
        path,
        server_end,
        realm,
    )
    .await?;
    Ok(root_dir)
}

/// Opens an object in a component instance directory with the given |flags|, |mode| and |path|.
/// Component manager will make the corresponding `fuchsia.io.Directory/Open` call on
/// the directory.
pub async fn open_in_instance_dir(
    moniker: &AbsoluteMoniker,
    dir_type: OpenDirType,
    flags: fio::OpenFlags,
    mode: fio::ModeType,
    path: &str,
    object: fidl::Channel,
    realm: &fsys::RealmQueryProxy,
) -> Result<(), OpenError> {
    let moniker_str = moniker.to_string();
    realm
        .open(&moniker_str, dir_type.clone().into(), flags, mode, path, object.into())
        .await?
        .map_err(|e| match e {
            fsys::OpenError::InstanceNotFound => OpenError::InstanceNotFound(moniker.clone()),
            fsys::OpenError::InstanceNotResolved => {
                OpenError::InstanceNotResolved(dir_type, moniker.clone())
            }
            fsys::OpenError::InstanceNotRunning => {
                OpenError::InstanceNotRunning(dir_type, moniker.clone())
            }
            fsys::OpenError::NoSuchDir => OpenError::NoSuchDir(moniker.clone(), dir_type),
            fsys::OpenError::BadDirType => OpenError::BadDirType(dir_type),
            fsys::OpenError::BadPath => OpenError::BadPath(path.to_string()),
            fsys::OpenError::BadMoniker => OpenError::BadMoniker(moniker.clone()),
            fsys::OpenError::FidlError => OpenError::OpenFidlError,
            _ => OpenError::UnknownError,
        })
}
