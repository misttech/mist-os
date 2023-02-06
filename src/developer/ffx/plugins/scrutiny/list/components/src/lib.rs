// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use ffx_core::ffx_plugin;
use ffx_scrutiny_components_list_args::ScrutinyComponentsCommand;
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::launcher;

#[ffx_plugin()]
pub async fn scrutiny_components(cmd: ScrutinyComponentsCommand) -> Result<()> {
    let command = "components.urls".to_string();
    let model = if cmd.recovery {
        ModelConfig::from_product_bundle_recovery(&cmd.product_bundle)
    } else {
        ModelConfig::from_product_bundle(&cmd.product_bundle)
    }?;
    let config = ConfigBuilder::with_model(model).command(command).build();
    launcher::launch_from_config(config)?;

    Ok(())
}
