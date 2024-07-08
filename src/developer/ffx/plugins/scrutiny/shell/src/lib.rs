// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_shell_args::ScrutinyShellCommand;
use fho::{FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, LoggingVerbosity, ModelConfig};
use scrutiny_frontend::launcher;

#[derive(FfxTool)]
pub struct ScrutinyShellTool {
    #[command]
    pub cmd: ScrutinyShellCommand,
}

fho::embedded_plugin!(ScrutinyShellTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyShellTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let model = if let Some(product_bundle) = self.cmd.product_bundle {
            if self.cmd.recovery {
                ModelConfig::from_product_bundle_recovery(product_bundle)
            } else {
                ModelConfig::from_product_bundle(product_bundle)
            }?
        } else {
            ModelConfig::empty()
        };
        let mut config_builder = ConfigBuilder::with_model(model);

        if let Some(command) = self.cmd.command {
            config_builder.command(command);
        }
        if let Some(script) = self.cmd.script {
            config_builder.script(script);
        }

        let mut config = config_builder.build();

        if let Some(model_path) = self.cmd.model {
            config.runtime.model.uri = model_path;
        }

        if let Some(log_path) = self.cmd.log {
            config.runtime.logging.path = log_path;
        }

        if let Some(verbosity) = self.cmd.verbosity {
            config.runtime.logging.verbosity = LoggingVerbosity::from(verbosity);
        }

        launcher::launch_from_config(config)?;

        Ok(())
    }
}
