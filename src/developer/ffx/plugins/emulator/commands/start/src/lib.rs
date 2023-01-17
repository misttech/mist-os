// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::editor::edit_configuration;
use crate::pbm::{list_virtual_devices, make_configs};
use anyhow::{Context, Result};
use cfg_if::cfg_if;
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::EngineOption;
use ffx_emulator_config::{EmulatorEngine, EngineType};
use ffx_emulator_engines::EngineBuilder;
use ffx_emulator_start_args::StartCommand;
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use std::str::FromStr;

mod editor;
mod pbm;

#[cfg(test)]
use mockall::{automock, predicate::*};

// Redeclare some methods we use from other crates so that we can mock them for tests.
#[cfg_attr(test, automock)]
#[allow(dead_code)]
mod modules {
    use super::*;

    pub(super) async fn get_engine_by_name(name: &mut Option<String>) -> Result<EngineOption> {
        ffx_emulator_commands::get_engine_by_name(name).await
    }
}

#[cfg_attr(test, automock)]
#[allow(dead_code)]
mod start {
    use super::*;

    pub(super) async fn new_engine(cmd: &StartCommand) -> Result<Box<dyn EmulatorEngine>> {
        let emulator_configuration = make_configs(&cmd).await?;

        // Initialize an engine of the requested type with the configuration defined in the manifest.
        let engine_type = EngineType::from_str(&cmd.engine().await.unwrap_or("femu".to_string()))
            .context("Couldn't retrieve engine type from ffx config.")?;

        EngineBuilder::new().config(emulator_configuration).engine_type(engine_type).build().await
    }
}

// if we're testing, use the mocked methods, otherwise use the
// ones from the other crates.
cfg_if! {
    if #[cfg(test)] {
        use self::mock_modules::get_engine_by_name;
        use self::mock_start::new_engine;
    } else {
        use self::modules::get_engine_by_name;
        use self::start::new_engine;
    }
}

#[ffx_plugin(TargetCollectionProxy = "daemon::protocol")]
pub async fn start(mut cmd: StartCommand, proxy: TargetCollectionProxy) -> Result<()> {
    // List the devices available in this product bundle
    if cmd.device_list {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use async_trait::async_trait;
    use ffx_emulator_config::EmulatorEngine;
    use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
    use std::process::Command;

    /// TestEngine is a test struct for implementing the EmulatorEngine trait. This version
    /// just captures when the stage and start functions are called, and asserts that they were
    /// supposed to be. On tear-down, if they were supposed to be and weren't, it will fail the
    /// test accordingly.
    #[derive(Default)]
    struct TestEngine {
        pub do_stage: bool,
        pub do_start: bool,
        did_stage: bool,
        did_start: bool,
    }

    impl TestEngine {
        pub fn new(do_stage: bool, do_start: bool) -> Self {
            TestEngine { do_stage, do_start, ..Default::default() }
        }
    }

    #[async_trait]
    impl EmulatorEngine for TestEngine {
        fn save_to_disk(&self) -> Result<()> {
            Ok(())
        }
        fn build_emulator_cmd(&self) -> Command {
            Command::new("some_program")
        }
        async fn stage(&mut self) -> Result<()> {
            self.did_stage = true;
            if self.do_stage {
                Ok(())
            } else {
                bail!("Test called stage() when it wasn't supposed to.")
            }
        }
        async fn start(
            &mut self,
            mut _emulator_cmd: Command,
            _proxy: &TargetCollectionProxy,
        ) -> Result<i32> {
            self.did_start = true;
            if self.do_start {
                Ok(0)
            } else {
                bail!("Test called start() when it wasn't supposed to.")
            }
        }
    }

    impl Drop for TestEngine {
        fn drop(&mut self) {
            if self.do_stage {
                assert!(
                    self.did_stage,
                    "The stage() function was supposed to be called but never was."
                );
            }
            if self.do_start {
                assert!(
                    self.did_start,
                    "The start() function was supposed to be called but never was."
                );
            }
        }
    }

    // Check that new_engine gets called by default and get_engine_by_name doesn't
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_get_engine_no_reuse_makes_new() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();

        let mut cmd = StartCommand::default();
        new_engine_ctx
            .expect()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>));
        get_engine_by_name_ctx.expect().times(0);

        let result = get_engine(&mut cmd).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Check that reuse and config together is still new_engine (i.e. config overrides reuse)
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_get_engine_with_config_doesnt_reuse() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();

        let mut cmd = StartCommand::default();
        cmd.reuse = true;
        cmd.config = Some("config.file".into());
        new_engine_ctx
            .expect()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            .times(1);
        get_engine_by_name_ctx.expect().times(0);

        let result = get_engine(&mut cmd).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Check that reuse and config.is_none calls get_engine_by_name
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_get_engine_without_config_does_reuse() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();

        let mut cmd = StartCommand::default();
        cmd.reuse = true;
        cmd.config = None;
        new_engine_ctx.expect().times(0);
        get_engine_by_name_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(
                    Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>
                ))
            })
            .times(1);

        let result = get_engine(&mut cmd).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Check that if get_engine_by_name returns DoesNotExist, new_engine still gets called and reuse is reset
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_get_engine_doesnotexist_creates_new() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();

        let mut cmd = StartCommand::default();
        cmd.reuse = true;
        cmd.config = None;
        new_engine_ctx
            .expect()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            .times(1);
        get_engine_by_name_ctx
            .expect()
            .returning(|_| Ok(EngineOption::DoesNotExist("Warning message".to_string())))
            .times(1);

        let result = get_engine(&mut cmd).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Check that if DoesExist, then cmd.name is updated too
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_get_engine_updates_cmd_name() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();

        let mut cmd = StartCommand::default();
        cmd.name = "".to_string();
        cmd.reuse = true;
        cmd.config = None;
        new_engine_ctx.expect().times(0);
        get_engine_by_name_ctx
            .expect()
            .returning(|name| {
                *name = Some("NewName".to_string());
                Ok(EngineOption::DoesExist(
                    Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>
                ))
            })
            .times(1);

        let result = get_engine(&mut cmd).await;
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(cmd.name, "NewName".to_string());
        Ok(())
    }

    // Ensure dry-run stops after building command, doesn't stage/run
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_dry_run() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        cmd.dry_run = true;
        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine::new(/*stage=*/ false, /*start=*/ false))
                    as Box<dyn EmulatorEngine>)
            })
            .times(1);

        let result = start(cmd, proxy).await;

        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Ensure stage stops after staging the files, doesn't run
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        cmd.stage = true;
        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine::new(/*stage=*/ true, /*start=*/ false))
                    as Box<dyn EmulatorEngine>)
            })
            .times(1);

        let result = start(cmd, proxy).await;

        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Ensure start goes through config and staging by default and calls start
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_start() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine::new(/*stage=*/ true, /*start=*/ true))
                    as Box<dyn EmulatorEngine>)
            })
            .times(1);

        let result = start(cmd, proxy).await;

        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }
}
