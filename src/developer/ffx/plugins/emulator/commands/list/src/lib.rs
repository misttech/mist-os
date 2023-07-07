// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(http://fxbug.dev/125953): ffx_plugin template always adds anyhow as a dep.
use anyhow as _;

use async_trait::async_trait;
use emulator_instance::{EmulatorInstanceData, EmulatorInstanceInfo, EngineState};
use ffx_emulator_list_args::ListCommand;
use fho::{FfxContext, FfxMain, FfxTool, MachineWriter, ToolIO, TryFromEnv, TryFromEnvWith};
use serde::Serialize;
use std::marker::PhantomData;

// TODO(http://fxbug.dev/94232): Update this error message once shut down is more robust.
const BROKEN_MESSAGE: &str = r#"
One or more emulators are in a 'Broken' state. This is an uncommon state, but usually happens if
the Fuchsia source tree or SDK is updated while the emulator is still running. Communication with
a "Broken" emulator may still be possible, but errors will be encountered for any further `ffx emu`
commands. Running `ffx emu stop` will not shut down a broken emulator (this should be fixed as part
of fxbug.dev/94232), but it will clear that emulator's state from the system, so this error won't
appear anymore.
"#;

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait Instances: TryFromEnv + 'static {
    async fn get_all_instances(&self) -> Result<Vec<EmulatorInstanceData>, fho::Error>;
}

pub struct InstanceData;

#[derive(Debug, Clone, Default)]
pub struct WithInstances<P: Instances>(PhantomData<P>);

#[async_trait(?Send)]
impl TryFromEnv for InstanceData {
    async fn try_from_env(_env: &fho::FhoEnvironment) -> Result<Self, fho::Error> {
        Ok(InstanceData)
    }
}

#[async_trait]
impl Instances for InstanceData {
    async fn get_all_instances(&self) -> Result<Vec<EmulatorInstanceData>, fho::Error> {
        emulator_instance::get_all_instances().await.map_err(|e| e.into())
    }
}

#[async_trait(?Send)]
impl<T: Instances> TryFromEnvWith for WithInstances<T> {
    type Output = T;
    async fn try_from_env_with(self, _env: &fho::FhoEnvironment) -> Result<T, fho::Error> {
        Ok(T::try_from_env(_env).await?)
    }
}

fn instance_getter<P: Instances>() -> WithInstances<P> {
    WithInstances(PhantomData::<P>::default())
}

/// Sub-sub tool for `emu list`
#[derive(FfxTool)]
pub struct EmuListTool<T: Instances> {
    #[command]
    cmd: ListCommand,
    // TODO(http://fxbug.dev/125356): FfxTool derive macro should handle generics
    // using #with is a workaround.
    #[with(instance_getter())]
    instances: T,
}

/// This is the item representing the output for a single
/// emulator instance.
#[derive(Serialize)]
pub struct EmuListItem {
    pub name: String,
    pub state: EngineState,
}

// Since this is a part of a legacy plugin, add
// the legacy entry points. If and when this
// is migrated to a subcommand, this macro can be
// removed.
fho::embedded_plugin!(EmuListTool<InstanceData>);

#[async_trait(?Send)]
impl<T: Instances> FfxMain for EmuListTool<T> {
    type Writer = MachineWriter<EmuListItem>;
    async fn main(self, mut writer: MachineWriter<EmuListItem>) -> fho::Result<()> {
        let instance_list: Vec<EmulatorInstanceData> = self
            .instances
            .get_all_instances()
            .await
            .user_message("Error encountered looking up emulator instances")?;
        let mut broken = false;
        for instance in instance_list {
            let name = instance.get_name();
            let engine_state = instance.get_engine_state();

            let item = EmuListItem { name: name.into(), state: engine_state };
            if self.cmd.only_running && !instance.is_running() {
                continue;
            } else {
                writer.machine_or_else(&item, || {
                    let state = format!("[{}]", engine_state);
                    format!("{:16}{}", state, item.name)
                })?;
            }
            broken = broken || engine_state == EngineState::Error;
        }
        if broken {
            writeln!(writer.stderr(), "{}", BROKEN_MESSAGE).bug()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffx_writer::{Format, TestBuffers};

    #[async_trait(?Send)]
    impl TryFromEnv for MockInstances {
        async fn try_from_env(_env: &fho::FhoEnvironment) -> Result<Self, fho::Error> {
            Ok(MockInstances::new())
        }
    }

    #[fuchsia::test]
    async fn test_empty_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };

        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };
        // Text based writer
        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        // Mock the return data
        let mock_return = || Ok(vec![]);

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        // Run both tools.
        tool.main(writer).await?;

        // Validate output
        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");

        Ok(())
    }

    #[fuchsia::test]
    async fn test_empty_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        // JSON based writer
        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        // Mock the return data
        let mock_return = || Ok(vec![]);

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        assert_eq!(stdout, "");
        assert_eq!(stderr, "");
        Ok(())
    }

    #[fuchsia::test]
    async fn test_new_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return =
            || Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::New)]);

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[new]           notrunning_emu\n";
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_new_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return =
            || Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::New)]);

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", r#"{"name":"notrunning_emu","state":"new"}"#);
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_configured_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "notrunning_config_emu",
                EngineState::Configured,
            )])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[configured]    notrunning_config_emu\n";
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_configured_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "notrunning_config_emu",
                EngineState::Configured,
            )])
        };

        let machine_ctx = tool.instances.expect_get_all_instances();
        machine_ctx.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected =
            format!("{}\n", r#"{"name":"notrunning_config_emu","state":"configured"}"#);
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_staged_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::Staged)])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[staged]        notrunning_emu\n";
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_staged_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::Staged)])
        };

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", r#"{"name":"notrunning_emu","state":"staged"}"#);
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_running_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![running])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_running_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![running])
        };

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", r#"{"name":"running_emu","state":"running"}"#);
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_running_flag() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: true };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![
                EmulatorInstanceData::new_with_state("new_emu", EngineState::New),
                EmulatorInstanceData::new_with_state("config_emu", EngineState::Configured),
                EmulatorInstanceData::new_with_state("staged_emu", EngineState::Staged),
                running,
                EmulatorInstanceData::new_with_state("error_emu", EngineState::Error),
            ])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_running_flag_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: true };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![
                EmulatorInstanceData::new_with_state("new_emu", EngineState::New),
                EmulatorInstanceData::new_with_state("config_emu", EngineState::Configured),
                EmulatorInstanceData::new_with_state("staged_emu", EngineState::Staged),
                running,
                EmulatorInstanceData::new_with_state("error_emu", EngineState::Error),
            ])
        };

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", r#"{"name":"running_emu","state":"running"}"#);
        assert_eq!(stdout, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_all_instances() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![
                EmulatorInstanceData::new_with_state("new_emu", EngineState::New),
                EmulatorInstanceData::new_with_state("config_emu", EngineState::Configured),
                EmulatorInstanceData::new_with_state("staged_emu", EngineState::Staged),
                running,
                EmulatorInstanceData::new_with_state("error_emu", EngineState::Error),
            ])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let expected = "[new]           new_emu\n\
        [configured]    config_emu\n\
        [staged]        staged_emu\n\
        [running]       running_emu\n\
        [error]         error_emu\n";
        assert_eq!(stdout, expected);
        assert_eq!(
            stderr,
            format!("{BROKEN_MESSAGE}\n"),
            "Expected `BROKEN_MESSAGE` in stderr, got {stderr:?}"
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_all_instances_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![
                EmulatorInstanceData::new_with_state("new_emu", EngineState::New),
                EmulatorInstanceData::new_with_state("config_emu", EngineState::Configured),
                EmulatorInstanceData::new_with_state("staged_emu", EngineState::Staged),
                running,
                EmulatorInstanceData::new_with_state("error_emu", EngineState::Error),
            ])
        };

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let expected_json = vec![
            r#"{"name":"new_emu","state":"new"}"#,
            r#"{"name":"config_emu","state":"configured"}"#,
            r#"{"name":"staged_emu","state":"staged"}"#,
            r#"{"name":"running_emu","state":"running"}"#,
            r#"{"name":"error_emu","state":"error"}"#,
        ];

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", expected_json.join("\n"));
        assert_eq!(stdout, stdout_expected);
        assert_eq!(
            stderr,
            format!("{BROKEN_MESSAGE}\n"),
            "Expected `BROKEN_MESSAGE` in stderr, got {stderr:?}"
        );
        Ok(())
    }

    #[fuchsia::test]
    async fn test_error_list() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let test_buffers = TestBuffers::default();
        let writer: MachineWriter<EmuListItem> = MachineWriter::new_test(None, &test_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "error_emu_error_list",
                EngineState::Error,
            )])
        };

        let ctx = tool.instances.expect_get_all_instances();
        ctx.returning(mock_return);

        tool.main(writer).await?;

        let (stdout, stderr) = test_buffers.into_strings();
        let stdout_expected = "[error]         error_emu_error_list\n";
        assert_eq!(stdout, stdout_expected);
        assert_eq!(stderr, format!("{}\n", BROKEN_MESSAGE));
        Ok(())
    }

    #[fuchsia::test]
    async fn test_error_list_machine() -> Result<(), fho::Error> {
        let _test_env = ffx_config::test_init().await.unwrap();
        let cmd = ListCommand { only_running: false };
        let mut tool = EmuListTool { cmd, instances: MockInstances::new() };

        let machine_buffers = TestBuffers::default();
        let machine_writer: MachineWriter<EmuListItem> =
            MachineWriter::new_test(Some(Format::Json), &machine_buffers);

        let mock_return = || {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "error_emu_error_list",
                EngineState::Error,
            )])
        };

        let ctx_machine = tool.instances.expect_get_all_instances();
        ctx_machine.returning(mock_return);

        tool.main(machine_writer).await?;

        let (stdout, stderr) = machine_buffers.into_strings();
        let stdout_expected = format!("{}\n", r#"{"name":"error_emu_error_list","state":"error"}"#);
        assert_eq!(stdout, stdout_expected);
        assert_eq!(stderr, format!("{}\n", BROKEN_MESSAGE));
        Ok(())
    }
}
