// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module for directory operations.
//
// TODO(fxbug.dev/132998): These operations can be merged into `fuchsia-fs` if Rust FIDL bindings
// support making one-way calls on a client endpoint without turning it into a proxy.

use anyhow::{Context, Error};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_io as fio;
use fuchsia_zircon as zx;
use std::sync::Arc;

/// A trait for opening filesystem nodes.
pub trait Directory {
    /// Open a node relative to the directory.
    fn open(&self, path: &str, flags: fio::OpenFlags, server_end: zx::Channel)
        -> Result<(), Error>;
}

impl Directory for fio::DirectoryProxy {
    fn open(
        &self,
        path: &str,
        flags: fio::OpenFlags,
        server_end: zx::Channel,
    ) -> Result<(), Error> {
        let () = self.open(flags, fio::ModeType::empty(), path, server_end.into())?;
        Ok(())
    }
}

impl Directory for ClientEnd<fio::DirectoryMarker> {
    fn open(
        &self,
        path: &str,
        flags: fio::OpenFlags,
        server_end: zx::Channel,
    ) -> Result<(), Error> {
        let () = fdio::open_at(self.channel(), path, flags, server_end)?;
        Ok(())
    }
}

/// A trait for types that can vend out a [`Directory`] reference.
///
/// A new trait is needed because both `DirectoryProxy` and `AsRef` are external types.
/// As a result, implementing `AsRef<&dyn Directory>` for `DirectoryProxy` is not allowed
/// under coherence rules.
pub trait AsRefDirectory {
    /// Get a [`Directory`] reference.
    fn as_ref_directory(&self) -> &dyn Directory;
}

impl AsRefDirectory for fio::DirectoryProxy {
    fn as_ref_directory(&self) -> &dyn Directory {
        self
    }
}

impl AsRefDirectory for ClientEnd<fio::DirectoryMarker> {
    fn as_ref_directory(&self) -> &dyn Directory {
        self
    }
}

impl<T: Directory> AsRefDirectory for Box<T> {
    fn as_ref_directory(&self) -> &dyn Directory {
        &**self
    }
}

impl<T: Directory> AsRefDirectory for Arc<T> {
    fn as_ref_directory(&self) -> &dyn Directory {
        &**self
    }
}

impl<T: Directory> AsRefDirectory for &T {
    fn as_ref_directory(&self) -> &dyn Directory {
        *self
    }
}

/// Opens the given `path` from the given `parent` directory as a [`DirectoryProxy`]. The target is
/// not verified to be any particular type and may not implement the fuchsia.io/Directory protocol.
pub fn open_directory_no_describe(
    parent: &impl AsRefDirectory,
    path: &str,
    flags: fio::OpenFlags,
) -> Result<fio::DirectoryProxy, Error> {
    let (dir, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
        .context("creating directory proxy")?;

    let flags = flags | fio::OpenFlags::DIRECTORY;
    let () = parent
        .as_ref_directory()
        .open(path, flags, server_end.into_channel())
        .context("opening directory without describe")?;

    Ok(dir)
}

/// Opens the given `path` from the given `parent` directory as a [`FileProxy`]. The target is not
/// verified to be any particular type and may not implement the fuchsia.io/File protocol.
pub fn open_file_no_describe(
    parent: &impl AsRefDirectory,
    path: &str,
    flags: fio::OpenFlags,
) -> Result<fio::FileProxy, Error> {
    let (file, server_end) =
        fidl::endpoints::create_proxy::<fio::FileMarker>().context("creating file proxy")?;

    let flags = flags | fio::OpenFlags::NOT_DIRECTORY;
    let () = parent
        .as_ref_directory()
        .open(path, flags, server_end.into_channel())
        .context("opening file without describe")?;

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_util::run_directory_server;
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use vfs::{directory::mutable::simple, file::vmo::read_only, pseudo_directory};

    #[fasync::run_singlethreaded(test)]
    async fn open_directory_no_describe_real() {
        let dir = pseudo_directory! {
            "dir" => simple(),
        };
        let dir = run_directory_server(dir);
        let dir = open_directory_no_describe(&dir, "dir", fio::OpenFlags::empty()).unwrap();
        fuchsia_fs::directory::close(dir).await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_directory_no_describe_fake() {
        let dir = pseudo_directory! {
            "dir" => simple(),
        };
        let dir = run_directory_server(dir);
        let dir = open_directory_no_describe(&dir, "fake", fio::OpenFlags::empty()).unwrap();
        // The open error is not detected until the proxy is interacted with.
        assert_matches!(fuchsia_fs::directory::close(dir).await, Err(_));
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_file_no_describe_real() {
        let dir = pseudo_directory! {
            "file" => read_only("read_only"),
        };
        let dir = run_directory_server(dir);
        let file = open_file_no_describe(&dir, "file", fio::OpenFlags::RIGHT_READABLE).unwrap();
        fuchsia_fs::file::close(file).await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_file_no_describe_fake() {
        let dir = pseudo_directory! {
            "file" => read_only("read_only"),
        };
        let dir = run_directory_server(dir);
        let fake = open_file_no_describe(&dir, "fake", fio::OpenFlags::RIGHT_READABLE).unwrap();
        // The open error is not detected until the proxy is interacted with.
        assert_matches!(fuchsia_fs::file::close(fake).await, Err(_));
    }
}
