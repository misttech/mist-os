// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_io::DirectoryProxy;
use fuchsia_fs::file::{ReadError, WriteError};
use fuchsia_zircon::Status;
use futures::future::BoxFuture;
use futures::FutureExt;

/// Copies all data from src to dst recursively.
pub fn recursive_copy<'a>(
    src: &'a DirectoryProxy,
    dst: &'a DirectoryProxy,
) -> BoxFuture<'a, Result<(), Error>> {
    async move {
        for entry in fuchsia_fs::directory::readdir(src).await.context("readdir")? {
            if entry.kind == fuchsia_fs::directory::DirentKind::Directory {
                let src = fuchsia_fs::directory::open_directory_no_describe(
                    src,
                    entry.name.as_str(),
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                )
                .context("open src dir")?;
                let dst = fuchsia_fs::directory::open_directory_no_describe(
                    dst,
                    entry.name.as_str(),
                    fuchsia_fs::OpenFlags::CREATE
                        | fuchsia_fs::OpenFlags::RIGHT_WRITABLE
                        | fuchsia_fs::OpenFlags::DIRECTORY,
                )
                .context("open dst dir")?;
                recursive_copy(&src, &dst)
                    .await
                    .with_context(|| format!("path {}", entry.name.as_str()))?;
            } else {
                let src = fuchsia_fs::directory::open_file_no_describe(
                    src,
                    entry.name.as_str(),
                    fuchsia_fs::OpenFlags::RIGHT_READABLE,
                )
                .context("open src file")?;
                let dst = fuchsia_fs::directory::open_file_no_describe(
                    dst,
                    entry.name.as_str(),
                    fuchsia_fs::OpenFlags::CREATE | fuchsia_fs::OpenFlags::RIGHT_WRITABLE,
                )
                .context("open dst file")?;
                loop {
                    let bytes = src
                        .read(fidl_fuchsia_io::MAX_BUF)
                        .await
                        .context("read")?
                        .map_err(|s| ReadError::ReadError(Status::from_raw(s)))
                        .context("read src file")?;
                    if bytes.is_empty() {
                        break;
                    }
                    dst.write(&bytes)
                        .await
                        .context("write")?
                        .map_err(|s| WriteError::WriteError(Status::from_raw(s)))
                        .context("write dst file")?;
                }
            }
        }
        Ok(())
    }
    .boxed()
}
