// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This library contains the shared functions that are specific to commands.

use anyhow::{anyhow, bail, Context, Result};
use emulator_instance::{get_all_instances, EmulatorInstanceInfo};
use ffx_emulator_config::EmulatorEngine;
use ffx_emulator_engines::serialization::read_engine_from_disk;

pub enum EngineOption {
    DoesExist(Box<dyn EmulatorEngine>),
    DoesNotExist(String),
}

pub async fn get_engine_by_name(name: &mut Option<String>) -> Result<EngineOption> {
    if name.is_none() {
        let mut all_instances = match get_all_instances().await {
            Ok(list) => list,
            Err(e) => {
                return Err(anyhow!("Error encountered looking up emulator instances: {:?}", e))
            }
        };
        if all_instances.len() == 1 {
            *name = Some(all_instances.pop().unwrap().get_name().to_string());
        } else if all_instances.len() == 0 {
            return Err(anyhow!("No emulators are running."));
        } else {
            return Err(anyhow!(
                "Multiple emulators are running. Indicate which emulator to access\n\
                by specifying the emulator name with your command.\n\
                See all the emulators available using `ffx emu list`."
            ));
        }
    }

    // If we got this far, name is set to either what the user asked for, or the only one running.
    if let Some(local_name) = name {
        read_engine_from_disk(&local_name)
            .await
            .map(|engine| EngineOption::DoesExist(engine))
            .context("Couldn't read the emulator information from disk.")
    } else {
        bail!("instance name not known")
    }
}
