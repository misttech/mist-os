// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuse_fs::FuseFs,
    async_trait::async_trait,
    fuse3::{
        raw::prelude::{Filesystem as FuseFilesystem, *},
        Result,
    },
    futures as _,
    futures_util::stream::Iter,
    fxfs::log::info,
    std::vec::IntoIter,
};

#[async_trait]
impl FuseFilesystem for FuseFs {
    // Stream of directory entries without attributes.
    type DirEntryStream = Iter<IntoIter<Result<DirectoryEntry>>>;

    // Stream of directory entries with attributes.
    type DirEntryPlusStream = Iter<IntoIter<Result<DirectoryEntryPlus>>>;

    /// Initializes filesystem. Called before any other filesystem method.
    async fn init(&self, _req: Request) -> Result<()> {
        info!("init");
        Ok(())
    }

    /// Gracefully closes filesystem. Currently fuse3 cannot handle unmount gracefully,
    /// see https://github.com/Sherlock-Holo/fuse3/issues/52.
    /// To fix it, destroy() is triggered when SIGTERM is received by the FUSE-Fxfs program.
    async fn destroy(&self, _req: Request) {
        info!("destroy");
        self.fs.close().await.expect("close failed");
    }
}

#[cfg(test)]
// TODO(fxbug.dev/117461): Add tests when there is enough functionality implemented to be tested.
mod tests {}
