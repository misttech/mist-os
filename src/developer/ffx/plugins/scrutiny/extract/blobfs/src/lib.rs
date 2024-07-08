// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_blobfs_args::ScrutinyBlobfsCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::command_builder::CommandBuilder;
use scrutiny_frontend::launcher;

#[derive(FfxTool)]
pub struct ScrutinyBlobfsTool {
    #[command]
    pub cmd: ScrutinyBlobfsCommand,
}

fho::embedded_plugin!(ScrutinyBlobfsTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyBlobfsTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        // An empty model can be used, because we do not need any artifacts other than the blobfs in
        // order to complete the extraction.
        let model = ModelConfig::empty();
        let command = CommandBuilder::new("tool.blobfs.extract")
            .param("input", self.cmd.input)
            .param("output", self.cmd.output)
            .build();
        let config = ConfigBuilder::with_model(model).command(command).build();
        launcher::launch_from_config(config)?;

        Ok(())
    }
}
