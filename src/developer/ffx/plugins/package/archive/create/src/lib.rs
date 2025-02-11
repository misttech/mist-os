// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
pub use ffx_package_archive_create_args::PackageArchiveCreateCommand;
use ffx_writer::SimpleWriter;
use fho::{user_error, FfxMain, FfxTool};
use package_tool::cmd_package_archive_create;

#[derive(FfxTool)]
pub struct ArchiveCreateTool {
    #[command]
    pub cmd: PackageArchiveCreateCommand,
}

fho::embedded_plugin!(ArchiveCreateTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ArchiveCreateTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_package_archive_create(self.cmd)
            .await
            .map_err(|err| user_error!("Error: failed to create archive: {err:?}"))?;
        Ok(())
    }
}
