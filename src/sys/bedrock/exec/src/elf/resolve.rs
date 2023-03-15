// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_io as fio, fidl_fuchsia_ldsvc as fldsvc, fuchsia_zircon_status::Status,
    std::sync::Arc, thiserror::Error,
};

/// Error encountered while resolving contents of a package.
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("failed to open path: {path:?}")]
    OpenPath {
        path: String,
        #[source]
        err: fuchsia_fs::node::OpenError,
    },

    #[error("failed to call get_backing_memory for file at {path:?}")]
    FidlGetBackingMemory {
        path: String,
        #[source]
        err: fidl::Error,
    },

    #[error("failed to obtain vmo for file at {path:?}: {status}")]
    GetBackingMemory { path: String, status: Status },
}

/// Returns a VMO for an executable binary in a package.
pub async fn get_binary_from_pkg_dir(
    pkg_dir: &fio::DirectoryProxy,
    bin_path: &str,
) -> Result<fidl::Vmo, ResolveError> {
    // Open the binary from the package dir as an executable VMO
    let binary = fuchsia_fs::directory::open_file(
        pkg_dir,
        bin_path,
        fio::OpenFlags::RIGHT_EXECUTABLE | fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .map_err(|err| ResolveError::OpenPath { path: bin_path.to_string(), err })?;

    let bin_vmo = binary
        .get_backing_memory(fio::VmoFlags::EXECUTE | fio::VmoFlags::READ)
        .await
        .map_err(|err| ResolveError::FidlGetBackingMemory { path: bin_path.to_string(), err })?
        .map_err(Status::from_raw)
        .map_err(|status| ResolveError::GetBackingMemory { path: bin_path.to_string(), status })?;

    Ok(bin_vmo)
}

/// Returns a loader service for a package.
pub async fn get_loader_from_pkg_dir(
    pkg_dir: &fio::DirectoryProxy,
) -> Result<fidl::endpoints::ClientEnd<fldsvc::LoaderMarker>, ResolveError> {
    // Construct a loader from the package library dir
    let lib_dir = fuchsia_fs::directory::open_directory(
        pkg_dir,
        "lib",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )
    .await
    .map_err(|err| ResolveError::OpenPath { path: "lib".to_string(), err })?;

    let (ldsvc, server_end) = fidl::endpoints::create_endpoints::<fldsvc::LoaderMarker>();
    let server_end = server_end.into_channel();
    library_loader::start(Arc::new(lib_dir), server_end);
    Ok(ldsvc)
}
