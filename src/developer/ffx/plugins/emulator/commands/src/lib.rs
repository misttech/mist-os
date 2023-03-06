// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This library contains the shared functions that are specific to commands.

use anyhow::{bail, Result};
use emulator_instance::{get_all_instances, read_from_disk, EmulatorInstanceInfo, EngineOption};
use ffx_emulator_config::EmulatorEngine;
use ffx_emulator_engines::EngineBuilder;

/// Returns the EmulatorEngine instance based on the name.
/// If name is none, and there is only 1 emulator instance found, that instance is returned, and the
///    name parameter is updated to the name of the instance.
/// If the name is some, then return that instance, or an error.
/// If there is no name, and not exactly 1 instance running, it is an error.
pub async fn get_engine_by_name(
    name: &mut Option<String>,
) -> Result<Option<Box<dyn EmulatorEngine>>> {
    if name.is_none() {
        let mut all_instances = match get_all_instances().await {
            Ok(list) => list,
            Err(e) => {
                bail!("Error encountered looking up emulator instances: {:?}", e);
            }
        };
        if all_instances.len() == 1 {
            *name = Some(all_instances.pop().unwrap().get_name().to_string());
        } else if all_instances.len() == 0 {
            tracing::debug!("No emulators are running.");
            return Ok(None);
        } else {
            bail!(
                "Multiple emulators are running. Indicate which emulator to access\n\
                by specifying the emulator name with your command.\n\
                See all the emulators available using `ffx emu list`."
            );
        }
    }

    // If we got this far, name is set to either what the user asked for, or the only one running.
    if let Some(local_name) = name {
        match read_from_disk(&local_name).await? {
            EngineOption::DoesExist(data) => {
                let engine = EngineBuilder::from_data(data)?;
                Ok(Some(engine))
            }
            EngineOption::DoesNotExist(_) => Ok(None),
        }
    } else {
        bail!("No emulator instances found")
    }
}
