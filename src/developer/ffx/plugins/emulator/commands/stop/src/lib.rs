// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use emulator_instance::{clean_up_instance_dir, get_all_instances, EmulatorInstanceInfo};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::get_engine_by_name;
use ffx_emulator_stop_args::StopCommand;

#[ffx_plugin()]
pub async fn stop(cmd: StopCommand) -> Result<()> {
    let mut names = vec![cmd.name];
    if cmd.all {
        names = match get_all_instances().await {
            Ok(list) => list.into_iter().map(|v| Some(v.get_name().to_string())).collect(),
            Err(e) => ffx_bail!("Error encountered looking up emulator instances: {:?}", e),
        };
    }
    for mut some_name in names {
        let engine = get_engine_by_name(&mut some_name).await;
        if engine.is_err() && some_name.is_none() {
            // This happens when the program doesn't know which instance to use. The
            // get_engine_by_name returns a good error message, and the loop should terminate
            // early.
            eprintln!("{:?}", engine.err().unwrap());
            break;
        }
        let name = some_name.unwrap_or("<unspecified>".to_string());
        match engine {
            Err(e) => eprintln!(
                "{:?}",
                e.context(format!(
                    "Couldn't deserialize engine '{}' from disk. Continuing stop, \
                    but you may need to terminate the emulator process manually.",
                    name
                ))
            ),
            Ok(None) => {
                eprintln!("{} does not exist.", name);
            }
            Ok(Some(mut engine)) => {
                println!("Stopping emulator '{}'...", name);
                if let Err(e) = engine.stop().await {
                    eprintln!("Failed with the following error: {:?}", e);
                }
            }
        }

        if !cmd.persist {
            let cleanup = clean_up_instance_dir(&name).await;
            if cleanup.is_err() {
                eprintln!(
                    "Cleanup of '{}' failed with the following error: {:?}",
                    name,
                    cleanup.unwrap_err()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_instance::{get_instance_dir, write_to_disk, EmulatorInstanceData, EngineState};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_existing() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = StopCommand::default();

        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Running);
        let instance_dir = get_instance_dir("one_instance", true).await?;
        write_to_disk(&data, &instance_dir)?;
        cmd.name = Some("one_instance".to_string());
        stop(cmd).await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_unknown() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = StopCommand::default();

        cmd.name = Some("unknown_instance".to_string());
        stop(cmd).await?;
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_not_running() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = StopCommand::default();

        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Staged);
        let instance_dir = get_instance_dir("one_instance", true).await?;
        write_to_disk(&data, &instance_dir)?;
        cmd.name = Some("one_instance".to_string());
        stop(cmd).await?;
        Ok(())
    }
}
