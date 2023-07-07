// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use errors::ffx_bail;

use async_trait::async_trait;
use ffx_emulator_commands::get_engine_by_name;
use ffx_emulator_config::ShowDetail;
use ffx_emulator_show_args::ShowCommand;
use fho::{FfxMain, FfxTool, MachineWriter, ToolIO};

/// Sub-sub tool for `emu show`
#[derive(Clone, FfxTool)]
pub struct EmuShowTool {
    #[command]
    cmd: ShowCommand,
}

fn which_details(cmd: &ShowCommand) -> Vec<ShowDetail> {
    let mut details = vec![];
    if cmd.raw {
        details = vec![ShowDetail::Raw { config: None }]
    }
    if cmd.cmd || cmd.all {
        details.push(ShowDetail::Cmd { program: None, args: None, env: None })
    }
    if cmd.config || cmd.all {
        details.push(ShowDetail::Config { flags: None })
    }
    if cmd.device || cmd.all {
        details.push(ShowDetail::Device { device: None })
    }
    if cmd.net || cmd.all {
        details.push(ShowDetail::Net { mode: None, mac_address: None, upscript: None, ports: None })
    }

    if details.is_empty() {
        details = vec![
            ShowDetail::Cmd { program: None, args: None, env: None },
            ShowDetail::Config { flags: None },
            ShowDetail::Device { device: None },
            ShowDetail::Net { mode: None, mac_address: None, upscript: None, ports: None },
        ]
    }
    details
}

// Since this is a part of a legacy plugin, add
// the legacy entry points. If and when this
// is migrated to a subcommand, this macro can be
//removed.
fho::embedded_plugin!(EmuShowTool);

#[async_trait(?Send)]
impl FfxMain for EmuShowTool {
    type Writer = MachineWriter<ShowDetail>;
    async fn main(self, writer: MachineWriter<ShowDetail>) -> fho::Result<()> {
        // implementation here
        self.show(writer).await.map_err(|e| e.into())
    }
}

impl EmuShowTool {
    pub async fn show(&self, mut writer: MachineWriter<ShowDetail>) -> Result<()> {
        let mut instance_name = self.cmd.name.clone();
        match get_engine_by_name(&mut instance_name).await {
            Ok(Some(engine)) => {
                let info = engine.show(which_details(&self.cmd));

                if engine.emu_config().runtime.config_override && self.cmd.net {
                    writeln!(writer.stderr(),
                "Configuration was provided manually to the start command using the --config flag.\n\
                Network details for this instance cannot be shown with this tool; try\n    \
                    `ffx emu show --config`\n\
                to review the emulator flags directly."
            )?;
                }
                for d in info {
                    writer.machine_or(&d, &d)?;
                }
            }
            Ok(None) => {
                if let Some(name) = instance_name {
                    println!("Instance {name} not found.");
                } else {
                    println!("No instances found");
                }
            }
            Err(e) => ffx_bail!("{:?}", e),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_instance::{get_instance_dir, write_to_disk, EmulatorInstanceData, EngineState};
    use ffx_writer::{Format, TestBuffers};

    #[fuchsia::test]
    async fn test_show() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let mut tool = EmuShowTool { cmd: ShowCommand::default() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ShowDetail> = MachineWriter::new_test(None, &test_buffers);

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<ShowDetail> =
            MachineWriter::new_test(Some(Format::JsonPretty), &machine_buffers);

        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Running);
        let instance_dir = get_instance_dir("one_instance", true).await?;
        write_to_disk(&data, &instance_dir)?;
        tool.cmd.name = Some("one_instance".to_string());

        tool.clone().show(writer).await?;
        tool.show(machine_writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = include_str!("../test_data/test_show_expected.txt");
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = include_str!("../test_data/test_show_expected.json_pretty");

        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }
    #[fuchsia::test]
    async fn test_show_unknown() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();

        let mut tool = EmuShowTool { cmd: ShowCommand::default() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ShowDetail> = MachineWriter::new_test(None, &test_buffers);

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<ShowDetail> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        tool.cmd.name = Some("unknown_instance".to_string());

        tool.show(writer).await?;
        tool.show(machine_writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();

        assert_eq!(stdout, "");
        assert!(stderr.is_empty());

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = "";

        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }
}
