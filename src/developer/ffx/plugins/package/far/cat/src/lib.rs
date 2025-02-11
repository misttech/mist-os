// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use ffx_package_far_cat_args::CatCommand;
use ffx_writer::SimpleWriter;
use fho::{bug, user_error, FfxMain, FfxTool, Result};
use fuchsia_archive as far;
use std::fs::File;
use std::io::{self, Write as _};

#[derive(FfxTool)]
pub struct FarCatTool {
    #[command]
    pub cmd: CatCommand,
}

fho::embedded_plugin!(FarCatTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for FarCatTool {
    type Writer = SimpleWriter;
    async fn main(self, mut _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        let far_file = File::open(&self.cmd.far_file).map_err(|e| {
            user_error!("failed to open file: {}: {e}", self.cmd.far_file.display())
        })?;
        let mut reader = far::Reader::new(far_file)
            .map_err(|e| bug!("failed to parse FAR file: {}: {e}", self.cmd.far_file.display()))?;

        let bytes = reader.read_file(self.cmd.path.as_str().as_bytes()).with_context(|| {
            format!(
                "failed to read path {} from FAR file {}",
                self.cmd.path,
                self.cmd.far_file.display()
            )
        })?;
        io::stdout().write_all(&bytes).map_err(|e| bug!(e))?;

        Ok(())
    }
}
