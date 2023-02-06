// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use ffx_core::ffx_plugin;
use ffx_scrutiny_package_args::ScrutinyPackageCommand;
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::{command_builder::CommandBuilder, launcher};

#[ffx_plugin()]
pub async fn scrutiny_package(cmd: ScrutinyPackageCommand) -> Result<(), Error> {
    let model = if cmd.recovery {
        ModelConfig::from_product_bundle_recovery(cmd.product_bundle)
    } else {
        ModelConfig::from_product_bundle(cmd.product_bundle)
    }?;
    let command = CommandBuilder::new("package.extract")
        .param("url", cmd.url)
        .param("output", cmd.output)
        .build();
    let config = ConfigBuilder::with_model(model).command(command).build();
    launcher::launch_from_config(config)?;

    Ok(())
}
