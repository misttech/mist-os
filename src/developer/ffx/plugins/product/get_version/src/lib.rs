// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for the product version of a Product Bundle..

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_product_get_version_args::GetVersionCommand;
use ffx_writer::SimpleWriter;
use fho::{FfxContext, FfxMain, FfxTool};
use sdk_metadata::ProductBundle;
use std::io::Write;

/// This plugin will get the the product version of a Product Bundle.
#[derive(FfxTool)]
pub struct PbGetVersionTool {
    #[command]
    cmd: GetVersionCommand,
}

fho::embedded_plugin!(PbGetVersionTool);

#[async_trait(?Send)]
impl FfxMain for PbGetVersionTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let product_bundle = ProductBundle::try_load_from(self.cmd.product_bundle)
            .context("Failed to load product bundle")?;
        let version = match product_bundle {
            ProductBundle::V2(pb) => pb.product_version,
        };
        write!(writer, "{}", &version).bug()?;
        Ok(())
    }
}
