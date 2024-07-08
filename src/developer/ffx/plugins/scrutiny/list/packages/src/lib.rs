// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_packages_list_args::ScrutinyPackagesCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::launcher;

#[derive(FfxTool)]
pub struct ScrutinyPackagesTool {
    #[command]
    pub cmd: ScrutinyPackagesCommand,
}

fho::embedded_plugin!(ScrutinyPackagesTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyPackagesTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let command = "packages.urls".to_string();
        let model = if self.cmd.recovery {
            ModelConfig::from_product_bundle_recovery(&self.cmd.product_bundle)
        } else {
            ModelConfig::from_product_bundle(&self.cmd.product_bundle)
        }?;
        let config = ConfigBuilder::with_model(model).command(command).build();
        launcher::launch_from_config(config)?;

        Ok(())
    }
}
