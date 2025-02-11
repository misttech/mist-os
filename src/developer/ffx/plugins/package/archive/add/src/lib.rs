// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
pub use ffx_package_archive_add_args::PackageArchiveAddCommand;
use ffx_writer::SimpleWriter;
use fho::{user_error, FfxMain, FfxTool};
use package_tool::cmd_package_archive_add;

#[derive(FfxTool)]
pub struct ArchiveAddTool {
    #[command]
    pub cmd: PackageArchiveAddCommand,
}

fho::embedded_plugin!(ArchiveAddTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ArchiveAddTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_package_archive_add(self.cmd)
            .await
            .map_err(|err| user_error!("Error: failed to add to archive: {err:?}"))?;
        Ok(())
    }
}
