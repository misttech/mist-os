// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::scrutiny::Scrutiny;
use anyhow::Result;
use scrutiny_config::{Config, ConfigBuilder, ModelConfig};
use scrutiny_plugins::unified_plugin::UnifiedPlugin;

/// Launches scrutiny from a configuration file. This is intended to be used by binaries that
/// want to launch custom configurations of the Scrutiny framework with select features enabled.
pub fn launch_from_config(config: Config) -> Result<String> {
    let mut scrutiny = Scrutiny::new(config, UnifiedPlugin::with_model())?;
    scrutiny.run()
}

/// Provides a utility launcher for the Scruity frontend. This is intended to
/// be used by consumer libraries that simply want to launch the framework to
/// run a single command.
pub fn run_command(command: String) -> Result<String> {
    let model = ModelConfig::empty();
    let config = ConfigBuilder::with_model(model).command(command).build();
    launch_from_config(config)
}

/// Provides a utility launcher for the Scrutiny frontend. This is inteded to
/// be used by consumer libraries that simply want to launch the framework to
/// run a Scrutiny script.
pub fn run_script(script_path: String) -> Result<String> {
    let model = ModelConfig::empty();
    let config = ConfigBuilder::with_model(model).script(script_path).build();
    launch_from_config(config)
}
