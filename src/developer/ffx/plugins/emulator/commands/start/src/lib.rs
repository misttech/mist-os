// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::editor::edit_configuration;
use crate::pbm::{list_virtual_devices, make_configs};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::get_engine_by_name;
use ffx_emulator_config::EngineType;
use ffx_emulator_engines::EngineBuilder;
use ffx_emulator_start_args::StartCommand;
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use std::str::FromStr;

mod editor;
mod pbm;

#[ffx_plugin(TargetCollectionProxy = "daemon::protocol")]
pub async fn start(cmd: StartCommand, proxy: TargetCollectionProxy) -> Result<()> {
    let sdk = ffx_config::global_env_context()
        .context("loading global environment context")?
        .get_sdk()
        .await?;
    // If device name is list, list the available virtual devices and return.
    if cmd.device_list {
        match list_virtual_devices(&cmd, &sdk).await {
            Ok(devices) => {
                println!("Valid virtual device specifications are: {:?}", devices);
                return Ok(());
            }
            Err(e) => {
                ffx_bail!("{:?}", e.context("Listing available virtual device specifications"))
            }
        };
    }

    let mut engine = if cmd.reuse && cmd.config.is_none() {
        match get_engine_by_name(&mut Some(cmd.name)).await {
            Ok(engine) => engine,
            Err(e) => {
                ffx_bail!("{:?}", e);
            }
        }
    } else {
        let emulator_configuration = match make_configs(&cmd).await {
            Ok(config) => config,
            Err(e) => {
                ffx_bail!("{:?}", e);
            }
        };

        // Initialize an engine of the requested type with the configuration defined in the manifest.
        let engine_type =
            match EngineType::from_str(&cmd.engine().await.unwrap_or("femu".to_string())) {
                Ok(e) => e,
                Err(e) => {
                    ffx_bail!("{:?}", e.context("Couldn't retrieve engine type from ffx config."))
                }
            };

        match EngineBuilder::new()
            .config(emulator_configuration)
            .engine_type(engine_type)
            .build()
            .await
        {
            Ok(engine) => engine,
            Err(e) => ffx_bail!("{:?}", e.context("The emulator could not be configured.")),
        }
    };

    // We do an initial build here, because we need an initial configuration before staging.
    let mut emulator_cmd = engine.build_emulator_cmd();

    if cmd.verbose || cmd.dry_run {
        println!("\n[emulator] Command line after Configuration: {:?}\n", emulator_cmd);
        println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
        if cmd.dry_run {
            engine.save_to_disk()?;
            return Ok(());
        }
    }

    if cmd.config.is_none() && !cmd.reuse {
        // We don't stage files for custom configurations, because the EmulatorConfiguration
        // doesn't hold valid paths to the system images.
        if let Err(e) = engine.stage().await {
            ffx_bail!("{:?}", e.context("Problem staging to the emulator's instance directory."));
        }

        // We rebuild the command, since staging likely changed the file paths.
        emulator_cmd = engine.build_emulator_cmd();

        if cmd.verbose || cmd.stage {
            println!("\n[emulator] Command line after Staging: {:?}\n", emulator_cmd);
            println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
            if cmd.stage {
                return Ok(());
            }
        }
    }

    if cmd.edit {
        if let Err(e) = edit_configuration(engine.emu_config_mut()) {
            ffx_bail!("{:?}", e.context("Problem editing configuration."));
        }
    }

    if cmd.verbose {
        println!("\n[emulator] Final Command line: {:?}\n", emulator_cmd);
        println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
    }

    match engine.start(emulator_cmd, &proxy).await {
        Ok(0) => Ok(()),
        Ok(_) => ffx_bail!("Non zero return code"),
        Err(e) => ffx_bail!("{:?}", e.context("The emulator failed to start.")),
    }
}
