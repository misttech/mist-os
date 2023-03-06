// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::get_engine_by_name;
use ffx_emulator_config::ShowDetail;
use ffx_emulator_show_args::ShowCommand;

fn which_details(cmd: ShowCommand) -> Vec<ShowDetail> {
    let mut details = vec![];
    if cmd.raw {
        details = vec![ShowDetail::Raw]
    } else {
        if cmd.cmd || cmd.all {
            details.push(ShowDetail::Cmd)
        }
        if cmd.config || cmd.all {
            details.push(ShowDetail::Config)
        }
        if cmd.device || cmd.all {
            details.push(ShowDetail::Device)
        }
        if cmd.net || cmd.all {
            details.push(ShowDetail::Net)
        }
    }
    if details.is_empty() {
        details = vec![ShowDetail::Raw]
    }
    details
}

#[ffx_plugin()]
pub async fn show(mut cmd: ShowCommand) -> Result<()> {
    match get_engine_by_name(&mut cmd.name).await {
        Ok(Some(engine)) => {
            engine.show(which_details(cmd));
        }
        Ok(None) => {
            if let Some(name) = cmd.name {
                println!("Instance {name} not found.");
            } else {
                println!("No instances found");
            }
        }
        Err(e) => ffx_bail!("{:?}", e),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_instance::{get_instance_dir, write_to_disk, EmulatorInstanceData, EngineState};

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_show() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = ShowCommand::default();

        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Running);
        let instance_dir = get_instance_dir("one_instance", true).await?;
        write_to_disk(&data, &instance_dir)?;
        cmd.name = Some("one_instance".to_string());
        show(cmd).await?;
        Ok(())
    }
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_show_unknown() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = ShowCommand::default();
        cmd.name = Some("unknown_instance".to_string());

        show(cmd).await?;
        Ok(())
    }
}
