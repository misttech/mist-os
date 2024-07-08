// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_package_args::ScrutinyPackageCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::command_builder::CommandBuilder;
use scrutiny_frontend::launcher;

#[derive(FfxTool)]
pub struct ScrutinyPackageTool {
    #[command]
    pub cmd: ScrutinyPackageCommand,
}

fho::embedded_plugin!(ScrutinyPackageTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyPackageTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let model = if self.cmd.recovery {
            ModelConfig::from_product_bundle_recovery(self.cmd.product_bundle)
        } else {
            ModelConfig::from_product_bundle(self.cmd.product_bundle)
        }?;
        let command = CommandBuilder::new("package.extract")
            .param("url", self.cmd.url)
            .param("output", self.cmd.output)
            .build();
        let config = ConfigBuilder::with_model(model).command(command).build();
        launcher::launch_from_config(config)?;

        Ok(())
    }
}
