// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use ffx_core::ffx_plugin;
use ffx_scrutiny_shell_args::ScrutinyShellCommand;
use scrutiny_config::{ConfigBuilder, LoggingVerbosity, ModelConfig};
use scrutiny_frontend::launcher;

#[ffx_plugin()]
pub async fn scrutiny_shell(cmd: ScrutinyShellCommand) -> Result<(), Error> {
    let model = if let Some(product_bundle) = cmd.product_bundle {
        if cmd.recovery {
            ModelConfig::from_product_bundle_recovery(product_bundle)
        } else {
            ModelConfig::from_product_bundle(product_bundle)
        }?
    } else {
        ModelConfig::empty()
    };
    let mut config_builder = ConfigBuilder::with_model(model);

    if let Some(command) = cmd.command {
        config_builder.command(command);
    }
    if let Some(script) = cmd.script {
        config_builder.script(script);
    }

    let mut config = config_builder.build();

    if let Some(model_path) = cmd.model {
        config.runtime.model.uri = model_path;
    }

    if let Some(log_path) = cmd.log {
        config.runtime.logging.path = log_path;
    }

    if let Some(verbosity) = cmd.verbosity {
        config.runtime.logging.verbosity = LoggingVerbosity::from(verbosity);
    }

    launcher::launch_from_config(config)?;

    Ok(())
}
