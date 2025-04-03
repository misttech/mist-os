// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl_fuchsia_io as fio;
use fuchsia_fs::file::{ReadError, WriteError};
use futures::future::BoxFuture;
use futures::FutureExt;
use zx::Status;

/// Copies all data from src to dst recursively.
pub fn recursive_copy<'a>(
    src: &'a fio::DirectoryProxy,
    dst: &'a fio::DirectoryProxy,
) -> BoxFuture<'a, Result<(), Error>> {
    async move {
        for entry in fuchsia_fs::directory::readdir(src).await.context("readdir")? {
            if entry.kind == fuchsia_fs::directory::DirentKind::Directory {
                let src = fuchsia_fs::directory::open_directory_async(
                    src,
                    entry.name.as_str(),
                    fio::PERM_READABLE,
                )
                .context("open src dir")?;
                // Verifying the OnOpen event when creating the destination directory, otherwise we
                // never confirm that this directory was successfully created if the source
                // directory turns out to be empty.
                let dst = fuchsia_fs::directory::open_directory(
                    dst,
                    entry.name.as_str(),
                    fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_WRITABLE,
                )
                .await
                .context("open dst dir")?;
                recursive_copy(&src, &dst)
                    .await
                    .with_context(|| format!("path {}", entry.name.as_str()))?;
            } else {
                let src = fuchsia_fs::directory::open_file_async(
                    src,
                    entry.name.as_str(),
                    fio::PERM_READABLE,
                )
                .context("open src file")?;
                // Verifying the OnOpen event when creating the destination file, otherwise we never
                // confirm that this file was successfully created if the source file turns out to
                // be empty.
                let dst = fuchsia_fs::directory::open_file(
                    dst,
                    entry.name.as_str(),
                    fio::Flags::FLAG_MAYBE_CREATE | fio::PERM_WRITABLE,
                )
                .await
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
