// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use cfg_if::cfg_if;
use emulator_instance::{EmulatorInstanceData, EmulatorInstanceInfo, EngineState};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_list_args::ListCommand;
#[cfg(test)]
use mockall::{automock, predicate::*};

// Redeclare some methods we use from other crates so that
// we can mock them for tests.
#[cfg_attr(test, automock)]
#[allow(dead_code)]
mod modules {
    use super::*;

    pub(super) async fn get_all_instances() -> Result<Vec<EmulatorInstanceData>> {
        emulator_instance::get_all_instances().await
    }
}

// if we're testing, use the mocked methods, otherwise use the
// ones from the other crates.
cfg_if! {
    if #[cfg(test)] {
        use self::mock_modules::get_all_instances;
    } else {
        use self::modules::get_all_instances;
    }
}

// TODO(http://fxbug.dev/94232): Update this error message once shut down is more robust.
const BROKEN_MESSAGE: &str = r#"
One or more emulators are in a 'Broken' state. This is an uncommon state, but usually happens if
the Fuchsia source tree or SDK is updated while the emulator is still running. Communication with
a "Broken" emulator may still be possible, but errors will be encountered for any further `ffx emu`
commands. Running `ffx emu stop` will not shut down a broken emulator (this should be fixed as part
of fxbug.dev/94232), but it will clear that emulator's state from the system, so this error won't
appear anymore.
"#;

#[ffx_plugin()]
pub async fn list(cmd: ListCommand) -> Result<()> {
    exec_list_impl(&mut std::io::stdout(), &mut std::io::stderr(), cmd).await
}

/// Entry point for the list command that allows specifying the writer for the output.
pub async fn exec_list_impl<W: std::io::Write, E: std::io::Write>(
    writer: &mut W,
    error_writer: &mut E,
    cmd: ListCommand,
) -> Result<()> {
    let instance_list = match get_all_instances().await {
        Ok(list) => list,
        Err(e) => ffx_bail!("Error encountered looking up emulator instances: {:?}", e),
    };
    let mut broken = false;
    for instance in instance_list {
        let name = instance.get_name();
        let engine_state = instance.get_engine_state();
        let state = format!("[{}]", engine_state);
        if cmd.only_running && !instance.is_running() {
            continue;
        } else {
            writeln!(writer, "{:16}{}", state, name)?;
        }
        broken = broken || engine_state == EngineState::Error;
    }
    if broken {
        writeln!(error_writer, "{}", BROKEN_MESSAGE)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::lock::Mutex;
    use lazy_static::lazy_static;
    use std::str;

    // Use a mutex to protect the mocked module context.
    lazy_static! {
        static ref MTX: Mutex<()> = Mutex::new(());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_empty_list() -> Result<()> {
        let cmd = ListCommand { only_running: false };
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let _m = MTX.lock().await;

        // no existing instances
        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec![]));

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        assert_eq!(str::from_utf8(&stdout)?, "");
        assert_eq!(str::from_utf8(&stderr)?, "");
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_new_list() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: false };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
            Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::New)])
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[new]           notrunning_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_configured_list() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: false };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "notrunning_config_emu",
                EngineState::Configured,
            )])
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[configured]    notrunning_config_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_staged_list() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: false };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
            Ok(vec![EmulatorInstanceData::new_with_state("notrunning_emu", EngineState::Staged)])
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[staged]        notrunning_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_running_list() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: false };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
            let mut running =
                EmulatorInstanceData::new_with_state("running_emu", EngineState::Running);
            running.set_pid(std::process::id());
            Ok(vec![running])
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_running_flag() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: true };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
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
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(str::from_utf8(&stderr)?.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_not_running_flag() -> Result<()> {
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let cmd = ListCommand { only_running: false };
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
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
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let actual = str::from_utf8(&stdout)?;
        let expected = "[new]           new_emu\n\
        [configured]    config_emu\n\
        [staged]        staged_emu\n\
        [running]       running_emu\n\
        [error]         error_emu\n";
        assert_eq!(actual, expected);
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_error_list() -> Result<()> {
        let cmd = ListCommand { only_running: false };
        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        let _m = MTX.lock().await;

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| {
            Ok(vec![EmulatorInstanceData::new_with_state(
                "error_emu_error_list",
                EngineState::Error,
            )])
        });

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[error]         error_emu_error_list\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert_eq!(str::from_utf8(&stderr)?, format!("{}\n", BROKEN_MESSAGE));
        Ok(())
    }
}
