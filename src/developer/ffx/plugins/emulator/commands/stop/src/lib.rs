// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use emulator_instance::{clean_up_instance_dir, get_all_instances, EmulatorInstanceInfo};
use errors::ffx_bail;
use ffx_emulator_commands::get_engine_by_name;
use ffx_emulator_stop_args::StopCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};

/// Sub-sub tool for `emu stop`
#[derive(FfxTool)]
pub struct EmuStopTool {
    #[command]
    cmd: StopCommand,
}

// Since this is a part of a legacy plugin, add
// the legacy entry points. If and when this
// is migrated to a subcommand, this macro can be
// removed.
fho::embedded_plugin!(EmuStopTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for EmuStopTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let mut names = vec![self.cmd.name];

        if self.cmd.all {
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
                ffx_bail!("{:?}", engine.err().unwrap());
            }
            let name = some_name.unwrap_or("<unspecified>".to_string());
            match engine {
                Err(e) => eprintln!(
                    "Couldn't deserialize engine '{name}' from disk. Continuing stop, \
                    but you may need to terminate the emulator process manually: {e:?}"
                ),
                Ok(None) => {
                    ffx_bail!("{name} does not exist.");
                }
                Ok(Some(mut engine)) => {
                    println!("Stopping emulator '{name}'...");
                    if let Err(e) = engine.stop().await {
                        eprintln!("Failed with the following error: {:?}", e);
                    }
                }
            }
            if !self.cmd.persist {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_instance::{get_instance_dir, write_to_disk, EmulatorInstanceData, EngineState};
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_existing() {
        let _env = ffx_config::test_init().await.unwrap();
        let the_name = "one_instance".to_string();
        let cmd = StopCommand { name: Some(the_name.clone()), ..Default::default() };
        let data = EmulatorInstanceData::new_with_state(&the_name, EngineState::Running);
        let instance_dir = get_instance_dir(&the_name, true).await.unwrap();
        write_to_disk(&data, &instance_dir).unwrap();

        let tool = EmuStopTool { cmd };
        tool.main(SimpleWriter::new()).await.expect("unexpected error");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_unknown() {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StopCommand { name: Some("unknown_instance".to_string()), ..Default::default() };
        let tool = EmuStopTool { cmd };
        let expected_phrase = "unknown_instance does not exist";
        let err = tool.main(SimpleWriter::new()).await.expect_err("expected error");
        assert!(err.to_string().contains(expected_phrase), "expected '{expected_phrase}' in {err}");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_multiple_running_error() {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StopCommand::default();
        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Staged);
        let instance_dir = get_instance_dir("one_instance", true).await.unwrap();
        write_to_disk(&data, &instance_dir).unwrap();
        let data2 = EmulatorInstanceData::new_with_state("two_instance", EngineState::Staged);
        let instance_dir2 = get_instance_dir("two_instance", true).await.unwrap();
        write_to_disk(&data2, &instance_dir2).unwrap();

        let tool = EmuStopTool { cmd };
        let expected_phrase = "Multiple emulators are running";
        let err = tool.main(SimpleWriter::new()).await.expect_err("expected error");
        assert!(err.to_string().contains(expected_phrase), "expected '{expected_phrase}' in {err}");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_multiple_running() {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StopCommand { all: true, ..Default::default() };
        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Staged);
        let instance_dir = get_instance_dir("one_instance", true).await.unwrap();
        write_to_disk(&data, &instance_dir).unwrap();
        let data2 = EmulatorInstanceData::new_with_state("two_instance", EngineState::Staged);
        let instance_dir2 = get_instance_dir("two_instance", true).await.unwrap();
        write_to_disk(&data2, &instance_dir2).unwrap();

        let tool = EmuStopTool { cmd };
        tool.main(SimpleWriter::new()).await.expect("unexpected error");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_not_running() {
        let _env = ffx_config::test_init().await.unwrap();
        let mut cmd = StopCommand::default();
        let data = EmulatorInstanceData::new_with_state("one_instance", EngineState::Staged);
        let instance_dir = get_instance_dir("one_instance", true).await.unwrap();
        write_to_disk(&data, &instance_dir).unwrap();
        cmd.name = Some("one_instance".to_string());

        let tool = EmuStopTool { cmd };
        tool.main(SimpleWriter::new()).await.expect("unexpected error");
    }
}
