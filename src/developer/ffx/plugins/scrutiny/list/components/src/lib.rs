// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_components_list_args::ScrutinyComponentsCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::launcher;

#[derive(FfxTool)]
pub struct ScrutinyComponentsTool {
    #[command]
    pub cmd: ScrutinyComponentsCommand,
}

fho::embedded_plugin!(ScrutinyComponentsTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyComponentsTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let command = "components.urls".to_string();
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
