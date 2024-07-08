// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_scrutiny_structured_config_args::ScrutinyStructuredConfigCommand;
use fho::{bug, FfxMain, FfxTool, Result, SimpleWriter};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::command_builder::CommandBuilder;
use scrutiny_frontend::launcher;
use scrutiny_plugins::verify::ExtractStructuredConfigResponse;
use scrutiny_utils::path::relativize_path;

#[derive(FfxTool)]
pub struct ScrutinyStructuredConfigTool {
    #[command]
    pub cmd: ScrutinyStructuredConfigCommand,
}

fho::embedded_plugin!(ScrutinyStructuredConfigTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ScrutinyStructuredConfigTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let ScrutinyStructuredConfigCommand {
            product_bundle,
            build_path,
            depfile,
            output,
            recovery,
        } = self.cmd;

        let model = if recovery {
            ModelConfig::from_product_bundle_recovery(product_bundle)
        } else {
            ModelConfig::from_product_bundle(product_bundle)
        }?;
        let command = CommandBuilder::new("verify.structured_config.extract").build();
        let plugins = vec![
            "ZbiPlugin".to_string(),
            "CorePlugin".to_string(),
            "AdditionalBootConfigPlugin".to_string(),
            "StaticPkgsPlugin".to_string(),
            "VerifyPlugin".to_string(),
        ];
        let mut config = ConfigBuilder::with_model(model).command(command).plugins(plugins).build();
        config.runtime.logging.silent_mode = true;
        let scrutiny_output =
            launcher::launch_from_config(config).map_err(|e| bug!("running scrutiny: {e}"))?;

        let ExtractStructuredConfigResponse { components, deps } =
            serde_json::from_str(&scrutiny_output).map_err(|e: serde_json::Error| {
                bug!("deserializing verify response `{}`: {e}", scrutiny_output)
            })?;

        let result = serde_json::to_string_pretty(&components)
            .map_err(|e| bug!("prettifying response JSON: {e}"))?;
        std::fs::write(&output, result).map_err(|e| bug!("writing output to file: {e}"))?;

        let relative_dep_paths = deps
            .iter()
            .map(|dep_path| relativize_path(&build_path, dep_path).display().to_string())
            .collect::<Vec<_>>();
        let depfile_contents = format!("{}: {}", output.display(), relative_dep_paths.join(" "));
        std::fs::write(&depfile, depfile_contents).map_err(|e| bug!("writing depfile: {e}"))?;

        Ok(())
    }
}
