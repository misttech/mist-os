// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
pub use ffx_package_archive_edit_args::PackageArchiveEditCommand;
use ffx_writer::SimpleWriter;
use fho::{user_error, FfxMain, FfxTool};
use package_tool::cmd_package_archive_edit;

#[derive(FfxTool)]
pub struct ArchiveEditTool {
    #[command]
    pub cmd: PackageArchiveEditCommand,
}

fho::embedded_plugin!(ArchiveEditTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ArchiveEditTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_package_archive_edit(self.cmd)
            .await
            .map_err(|err| user_error!("Error: failed to edit archive: {err:?}"))?;
        Ok(())
    }
}
