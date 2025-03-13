// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
pub use ffx_package_archive_extract_args::PackageArchiveExtractCommand;
use ffx_writer::SimpleWriter;
use fho::{user_error, FfxMain, FfxTool};
use package_tool::cmd_package_archive_extract;

#[derive(FfxTool)]
pub struct ArchiveExtractTool {
    #[command]
    pub cmd: PackageArchiveExtractCommand,
}

fho::embedded_plugin!(ArchiveExtractTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ArchiveExtractTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        cmd_package_archive_extract(self.cmd)
            .await
            .map_err(|err| user_error!("Error: failed to extract archive: {err:?}"))?;
        Ok(())
    }
}
