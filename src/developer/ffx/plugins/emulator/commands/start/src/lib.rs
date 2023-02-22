// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::pbm::{list_virtual_devices, make_configs};
use anyhow::{Context, Result};
use cfg_if::cfg_if;
use emulator_instance::{EmulatorConfiguration, EngineType};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::EngineOption;
use ffx_emulator_config::EmulatorEngine;
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
    pub(super) fn edit_configuration(emu_config: &mut EmulatorConfiguration) -> Result<()> {
        crate::editor::edit_configuration(emu_config)
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
        use self::mock_modules::edit_configuration;
        use self::mock_start::new_engine;
    } else {
        use self::modules::get_engine_by_name;
        use self::modules::edit_configuration;
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

    if cmd.config.is_none() && !cmd.reuse {
        // We don't stage files for custom configurations, because the EmulatorConfiguration
        // doesn't hold valid paths to the system images.
        if let Err(e) = engine.stage().await {
            ffx_bail!("{:?}", e.context("Problem staging to the emulator's instance directory."));
        }

        // We rebuild the command, since staging likely changed the file paths.
        emulator_cmd = engine.build_emulator_cmd();

        if cmd.stage {
            if cmd.verbose {
                println!("\n[emulator] Command line after Staging: {:?}\n", emulator_cmd);
                println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
            }
            return Ok(());
        }
    }

    if cmd.edit {
        if let Err(e) = edit_configuration(engine.emu_config_mut()) {
            ffx_bail!("{:?}", e.context("Problem editing configuration."));
        }
        // We rebuild the command again to pull in the user's changes.
        emulator_cmd = engine.build_emulator_cmd();
    }

    if cmd.verbose || cmd.dry_run {
        println!("\n[emulator] Final Command line: {:?}\n", emulator_cmd);
        println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
    }

    if cmd.dry_run {
        engine.save_to_disk().await?;
        return Ok(());
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
    use emulator_instance::RuntimeConfig;
    use ffx_emulator_config::EmulatorEngine;
    use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
    use std::process::Command;

    /// TestEngine is a test struct for implementing the EmulatorEngine trait. This version
    /// just captures when the stage and start functions are called, and asserts that they were
    /// supposed to be. On tear-down, if they were supposed to be and weren't, it will detect this
    /// in the Drop implementation and fail the test accordingly.
    pub struct TestEngine {
        do_stage: bool,
        do_start: bool,
        did_stage: bool,
        did_start: bool,
        stage_test_fn: fn(&mut EmulatorConfiguration) -> Result<()>,
        start_test_fn: fn(Command) -> Result<()>,
        config: EmulatorConfiguration,
    }

    impl Default for TestEngine {
        fn default() -> Self {
            Self {
                stage_test_fn: |_| Ok(()),
                start_test_fn: |_| Ok(()),
                do_stage: false,
                do_start: false,
                did_stage: false,
                did_start: false,
                config: EmulatorConfiguration::default(),
            }
        }
    }

    #[async_trait]
    impl EmulatorEngine for TestEngine {
        async fn save_to_disk(&self) -> Result<()> {
            Ok(())
        }
        fn build_emulator_cmd(&self) -> Command {
            Command::new(self.config.runtime.name.clone())
        }
        async fn stage(&mut self) -> Result<()> {
            self.did_stage = true;
            (self.stage_test_fn)(&mut self.config)?;
            if !self.do_stage {
                bail!("Test called stage() when it wasn't supposed to.")
            }
            Ok(())
        }
        async fn start(
            &mut self,
            emulator_cmd: Command,
            _proxy: &TargetCollectionProxy,
        ) -> Result<i32> {
            self.did_start = true;
            (self.start_test_fn)(emulator_cmd)?;
            if !self.do_start {
                bail!("Test called start() when it wasn't supposed to.")
            }
            Ok(0)
        }
        fn emu_config_mut(&mut self) -> &mut EmulatorConfiguration {
            &mut self.config
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

        let _ = get_engine(&mut cmd).await?;
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

        let _ = get_engine(&mut cmd).await?;
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

        let _ = get_engine(&mut cmd).await?;
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

        let _ = get_engine(&mut cmd).await?;
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

        let _ = get_engine(&mut cmd).await?;
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
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
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
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }

    // Ensure start goes through config and staging by default and calls start
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_start() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);

        let result = start(cmd.clone(), proxy.clone()).await;
        assert!(result.is_ok(), "{:?}", result.err());

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }

    // Ensure start() skips the stage() call if the reuse flag is true
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_reuse_doesnt_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let get_engine_by_name_ctx = mock_modules::get_engine_by_name_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        cmd.reuse = true;

        get_engine_by_name_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }

    // Ensure start() skips the stage() call is a custom config is provided
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_custom_config_doesnt_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        cmd.config = Some("filename".into());

        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }

    // Check that the final command reflects changes from the edit stage
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_edit() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let edit_ctx = mock_modules::edit_configuration_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        cmd.edit = true;

        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    start_test_fn: |command| {
                        assert_eq!(command.get_program(), "EditedValue");
                        Ok(())
                    },
                    config: EmulatorConfiguration {
                        runtime: RuntimeConfig { name: "name".to_string(), ..Default::default() },
                        ..Default::default()
                    },
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);

        edit_ctx
            .expect()
            .returning(|config| {
                config.runtime.name = "EditedValue".to_string();
                Ok(())
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }

    // Check that the final command reflects changes from staging
    #[fuchsia_async::run_singlethreaded(test)]
    #[serial_test::serial]
    async fn test_staging_edits() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let new_engine_ctx = mock_start::new_engine_context();
        let mut cmd = StartCommand::default();
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        new_engine_ctx
            .expect()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    stage_test_fn: |config| {
                        config.runtime.name = "EditedValue".to_string();
                        Ok(())
                    },
                    start_test_fn: |command| {
                        assert_eq!(command.get_program(), "EditedValue");
                        Ok(())
                    },
                    config: EmulatorConfiguration {
                        runtime: RuntimeConfig { name: "name".to_string(), ..Default::default() },
                        ..Default::default()
                    },
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(2);
        let _ = start(cmd.clone(), proxy.clone()).await?;

        // Verbose shouldn't change the sequence
        cmd.verbose = true;
        let _ = start(cmd, proxy).await?;
        Ok(())
    }
}
