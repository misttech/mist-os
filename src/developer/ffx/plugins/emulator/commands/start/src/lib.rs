// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::editor::edit_configuration;
use crate::pbm::{list_virtual_devices, make_configs};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::{get_engine_by_name, EngineOption};
use ffx_emulator_config::{EmulatorEngine, EngineType};
use ffx_emulator_engines::EngineBuilder;
use ffx_emulator_start_args::StartCommand;
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use std::str::FromStr;

mod editor;
mod pbm;

#[ffx_plugin(TargetCollectionProxy = "daemon::protocol")]
pub async fn start(mut cmd: StartCommand, proxy: TargetCollectionProxy) -> Result<()> {
    let sdk = ffx_config::global_env_context()
        .context("loading global environment context")?
        .get_sdk()
        .await?;

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

    let mut engine = get_engine(&mut cmd).await?;

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

async fn get_engine(cmd: &mut StartCommand) -> Result<Box<dyn EmulatorEngine>> {
    Ok(if cmd.reuse && cmd.config.is_none() {
        let mut name = Some(cmd.name.clone());
        match get_engine_by_name(&mut name)
            .await
            .or_else::<anyhow::Error, _>(|e| ffx_bail!("{:?}", e))?
        {
            EngineOption::DoesExist(engine) => {
                cmd.name = name.unwrap();
                engine
            }
            EngineOption::DoesNotExist(warning) => {
                tracing::debug!("{}", warning);
                println!(
                    "Instance '{name}' not found with --reuse flag. \
                    Creating a new emulator named '{name}'.",
                    name = name.unwrap()
                );
                cmd.reuse = false;
                new_engine(&cmd).await.or_else::<anyhow::Error, _>(|e| ffx_bail!("{:?}", e))?
            }
        }
    } else {
        new_engine(&cmd).await.or_else::<anyhow::Error, _>(|e| ffx_bail!("{:?}", e))?
    })
}

async fn new_engine(cmd: &StartCommand) -> Result<Box<dyn EmulatorEngine>> {
    let emulator_configuration = make_configs(&cmd).await?;

    // Initialize an engine of the requested type with the configuration defined in the manifest.
    let engine_type = EngineType::from_str(&cmd.engine().await.unwrap_or("femu".to_string()))
        .context("Couldn't retrieve engine type from ffx config.")?;

    EngineBuilder::new().config(emulator_configuration).engine_type(engine_type).build().await
}
