// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use emulator_instance::EmulatorInstances;
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_emulator_config::ShowDetail;
use ffx_emulator_engines::EngineBuilder;
use ffx_emulator_show_args::ShowCommand;
use fho::{bug, FfxMain, FfxTool, MachineWriter, ToolIO};
use std::path::PathBuf;

/// Sub-sub tool for `emu show`
#[derive(Clone, FfxTool)]
pub struct EmuShowTool {
    #[command]
    cmd: ShowCommand,
    context: EnvironmentContext,
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
        let instance_dir: PathBuf = self
            .context
            .get(emulator_instance::EMU_INSTANCE_ROOT_DIR)
            .await
            .map_err(|e| bug!("{e}"))?;
        let emu_instances = EmulatorInstances::new(instance_dir);
        self.show(&emu_instances, writer).await.map_err(|e| e.into())
    }
}

impl EmuShowTool {
    pub async fn show(
        &self,
        emu_instances: &EmulatorInstances,
        mut writer: MachineWriter<ShowDetail>,
    ) -> Result<()> {
        let mut instance_name = self.cmd.name.clone();
        let builder = EngineBuilder::new(emu_instances.clone());
        match builder.get_engine_by_name(&mut instance_name) {
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
    use emulator_instance::{write_to_disk, EmulatorInstanceData, EngineState};
    use ffx_writer::{Format, TestBuffers};

    #[fuchsia::test]
    async fn test_show() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let mut tool = EmuShowTool { cmd: ShowCommand::default(), context: env.context.clone() };
        let emu_instances = EmulatorInstances::new(PathBuf::from(env.isolate_root.path()));

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ShowDetail> = MachineWriter::new_test(None, &test_buffers);

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<ShowDetail> =
            MachineWriter::new_test(Some(Format::JsonPretty), &machine_buffers);

        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Running);
        let instance_dir = emu_instances.get_instance_dir("one_instance", true)?;
        write_to_disk(&data, &instance_dir)?;
        tool.cmd.name = Some("one_instance".to_string());

        tool.clone().show(&emu_instances, writer).await?;
        tool.show(&emu_instances, machine_writer).await?;

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
        let env = ffx_config::test_init().await.unwrap();

        let mut tool = EmuShowTool { cmd: ShowCommand::default(), context: env.context.clone() };
        let emu_instances = EmulatorInstances::new(PathBuf::from(env.isolate_root.path()));

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<ShowDetail> = MachineWriter::new_test(None, &test_buffers);

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<ShowDetail> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        tool.cmd.name = Some("unknown_instance".to_string());

        tool.show(&emu_instances, writer).await?;
        tool.show(&emu_instances, machine_writer).await?;

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
