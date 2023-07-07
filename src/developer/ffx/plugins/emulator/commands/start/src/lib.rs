// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::pbm::{list_virtual_devices, make_configs};
use anyhow::{Context, Result};
use async_trait::async_trait;
use emulator_instance::{EmulatorConfiguration, EngineType};
use errors::ffx_bail;
use ffx_config::Sdk;
use ffx_emulator_config::EmulatorEngine;
use ffx_emulator_engines::EngineBuilder;
use ffx_emulator_start_args::StartCommand;
use fho::{daemon_protocol, FfxContext, FfxMain, FfxTool, SimpleWriter, TryFromEnvWith};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use std::{marker::PhantomData, str::FromStr};

mod editor;
mod pbm;

pub(crate) const DEFAULT_NAME: &str = "fuchsia-emulator";

/// EngineOperations trait is used to allow mocking of
/// these methods.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EngineOperations: Default + 'static {
    async fn get_engine_by_name(
        &self,
        name: &mut Option<String>,
    ) -> Result<Option<Box<dyn EmulatorEngine>>>;

    fn edit_configuration(&self, emu_config: &mut EmulatorConfiguration) -> Result<()>;

    async fn new_engine(&self, cmd: &StartCommand) -> Result<Box<dyn EmulatorEngine>>;
}

#[derive(Default)]
pub struct EngineOperationsData;

#[derive(Debug, Clone, Default)]
pub struct WithEngineOperations<P: EngineOperations>(PhantomData<P>);

#[async_trait(?Send)]
impl<T: EngineOperations> TryFromEnvWith for WithEngineOperations<T> {
    type Output = T;
    async fn try_from_env_with(self, _env: &fho::FhoEnvironment) -> Result<T, fho::Error> {
        Ok(T::default())
    }
}

#[async_trait]
impl EngineOperations for EngineOperationsData {
    async fn get_engine_by_name(
        &self,
        name: &mut Option<String>,
    ) -> Result<Option<Box<dyn EmulatorEngine>>> {
        ffx_emulator_commands::get_engine_by_name(name).await
    }
    fn edit_configuration(&self, emu_config: &mut EmulatorConfiguration) -> Result<()> {
        crate::editor::edit_configuration(emu_config)
    }

    async fn new_engine(&self, cmd: &StartCommand) -> Result<Box<dyn EmulatorEngine>> {
        let emulator_configuration = make_configs(&cmd).await?;

        // Initialize an engine of the requested type with the configuration defined in the manifest.
        let engine_type = EngineType::from_str(&cmd.engine().await.unwrap_or("femu".to_string()))
            .context("Couldn't retrieve engine type from ffx config.")?;

        EngineBuilder::new().config(emulator_configuration).engine_type(engine_type).build().await
    }
}

fn engine_operations_getter<P: EngineOperations>() -> WithEngineOperations<P> {
    WithEngineOperations(PhantomData::<P>::default())
}
/// Sub-sub tool for `emu start`
#[derive(FfxTool)]
pub struct EmuStartTool<T: EngineOperations> {
    #[command]
    cmd: StartCommand,
    // TODO(http://fxbug.dev/125356): FfxTool derive macro should handle generics
    // using #with is a workaround.
    #[with(engine_operations_getter())]
    engine_operations: T,
    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,
    sdk: Sdk,
}

// Since this is a part of a legacy plugin, add
// the legacy entry points. If and when this
// is migrated to a subcommand, this macro can be
// removed.
fho::embedded_plugin!(EmuStartTool<EngineOperationsData>);

#[async_trait(?Send)]
impl<T: EngineOperations> FfxMain for EmuStartTool<T> {
    type Writer = SimpleWriter;

    async fn main(mut self, _writer: Self::Writer) -> fho::Result<()> {
        // List the devices available in this product bundle
        if self.cmd.device_list {
            let sdk = self.sdk;
            let devices = list_virtual_devices(&self.cmd, &sdk)
                .await
                .user_message("Error listing available virtual device specifications")?;
            println!("Valid virtual device specifications are: {:?}", devices);
            return Ok(());
        }

        let mut engine = self.get_engine().await?;

        // We do an initial build here, because we need an initial configuration before staging.
        let mut emulator_cmd = engine.build_emulator_cmd();

        if self.cmd.config.is_none() && !self.cmd.reuse {
            // We don't stage files for custom configurations, because the EmulatorConfiguration
            // doesn't hold valid paths to the system images.
            engine
                .stage()
                .await
                .user_message("Error staging the emulator's instance directory.")?;

            // We rebuild the command, since staging likely changed the file paths.
            emulator_cmd = engine.build_emulator_cmd();

            if self.cmd.stage {
                if self.cmd.verbose {
                    println!("\n[emulator] Command line after Staging: {:?}\n", emulator_cmd);
                    println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
                }
                return Ok(());
            }
        }

        if self.cmd.edit {
            self.engine_operations
                .edit_configuration(engine.emu_config_mut())
                .user_message("Problem editing configuration.")?;
            // We rebuild the command again to pull in the user's changes.
            emulator_cmd = engine.build_emulator_cmd();
        }

        if self.cmd.verbose || self.cmd.dry_run {
            println!("\n[emulator] Final Command line: {:?}\n", emulator_cmd);
            println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
        }

        if self.cmd.dry_run {
            engine.save_to_disk().await?;
            return Ok(());
        }

        let rc = engine
            .start(emulator_cmd, &self.target_collection)
            .await
            .user_message("The emulator failed to start")?;
        if rc != 0 {
            ffx_bail!("The emulator failed to start and returned a non zero return code: {rc}");
        }
        Ok(())
    }
}

impl<T: EngineOperations> EmuStartTool<T> {
    async fn get_engine(&mut self) -> Result<Box<dyn EmulatorEngine>> {
        if self.cmd.reuse && self.cmd.config.is_none() {
            // Set the name from the default, and make sure it is not empty.
            let name_from_cmd = self.cmd.name().await?;
            let mut name = if &name_from_cmd == "" {
                Some(crate::DEFAULT_NAME.into())
            } else {
                Some(name_from_cmd)
            };

            let engine_result = self.engine_operations.get_engine_by_name(&mut name).await?;

            if let Some(engine) = engine_result {
                // Set the cmd name to the engine name.
                self.cmd.name = name;
                return Ok(engine);
            } else {
                let message = format!(
                    "Instance '{name}' not found with --reuse flag. \
                Creating a new emulator named '{name}'.",
                    name = name.unwrap()
                );
                tracing::debug!("{message}");
                println!("{message}");
                self.cmd.reuse = false;
                return self.engine_operations.new_engine(&self.cmd).await;
            }
        }
        self.engine_operations.new_engine(&self.cmd).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use emulator_instance::RuntimeConfig;
    use ffx_config::ConfigLevel;
    use ffx_emulator_config::EmulatorEngine;
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

    async fn make_test_emu_start_tool(cmd: StartCommand) -> EmuStartTool<MockEngineOperations> {
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();
        let sdk = Sdk::get_empty_sdk_with_version(ffx_config::sdk::SdkVersion::Unknown);

        EmuStartTool {
            cmd,
            engine_operations: MockEngineOperations::new(),
            target_collection: proxy,
            sdk,
        }
    }

    // Check that new_engine gets called by default and get_engine_by_name doesn't
    #[fuchsia::test]
    async fn test_get_engine_no_reuse_makes_new() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();

        let cmd = StartCommand::default();
        let mut tool = make_test_emu_start_tool(cmd).await;
        tool.engine_operations
            .expect_new_engine()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>));
        tool.engine_operations.expect_get_engine_by_name().times(0);

        let _ = tool.get_engine().await?;
        Ok(())
    }

    // Check that reuse and config together is still new_engine (i.e. config overrides reuse)
    #[fuchsia::test]
    async fn test_get_engine_with_config_doesnt_reuse() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd =
            StartCommand { reuse: true, config: Some("config.file".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            .times(1);
        tool.engine_operations.expect_get_engine_by_name().times(0);

        let _ = tool.get_engine().await?;
        Ok(())
    }

    // Check that reuse and config.is_none calls get_engine_by_name
    #[fuchsia::test]
    async fn test_get_engine_without_config_does_reuse() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { reuse: true, config: None, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations.expect_new_engine().times(0);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|_| Ok(Some(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>)))
            .times(1);

        let _ = tool.get_engine().await?;
        Ok(())
    }

    // Check that if get_engine_by_name returns DoesNotExist, new_engine still gets called and reuse is reset
    #[fuchsia::test]
    async fn test_get_engine_doesnotexist_creates_new() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();

        let cmd = StartCommand { reuse: true, config: None, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_| Ok(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            .times(1);
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        let _ = tool.get_engine().await?;
        Ok(())
    }

    // Check that if DoesExist, then cmd.name is updated too
    #[fuchsia::test]
    async fn test_get_engine_updates_cmd_name() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();

        let cmd = StartCommand {
            name: Some("".to_string()),
            reuse: true,
            config: None,
            ..Default::default()
        };

        let mut tool = make_test_emu_start_tool(cmd).await;
        tool.engine_operations.expect_new_engine().times(0);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|name| {
                *name = Some("NewName".to_string());
                Ok(Some(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        let _ = tool.get_engine().await?;
        assert_eq!(tool.cmd.name().await?, "NewName".to_string());
        Ok(())
    }

    // Check that if DoesExist, then cmd.name is updated too
    #[fuchsia::test]
    async fn test_get_engine_updates_cmd_name_when_blank() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        env.context.query("emu.name").level(Some(ConfigLevel::User)).set("".into()).await?;

        let cmd = StartCommand { name: None, reuse: true, config: None, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations.expect_new_engine().times(0);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|name| {
                assert_eq!(name, &Some(DEFAULT_NAME.to_string()));
                Ok(Some(Box::new(TestEngine::default()) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        let _ = tool.get_engine().await?;
        assert_eq!(tool.cmd.name().await?, DEFAULT_NAME.to_string());
        Ok(())
    }

    // Ensure dry-run stops after building command, doesn't stage/run
    #[fuchsia::test]
    async fn test_dry_run() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { dry_run: true, verbose: true, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Ensure stage stops after staging the files, doesn't run
    #[fuchsia::test]
    async fn test_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { stage: true, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Ensure start goes through config and staging by default and calls start
    #[fuchsia::test]
    async fn test_start() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand::default();

        let mut tool = make_test_emu_start_tool(cmd).await;
        tool.engine_operations
            .expect_new_engine()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);

        let result = tool.main(SimpleWriter::new()).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Ensure start() skips the stage() call if the reuse flag is true
    #[fuchsia::test]
    async fn test_reuse_doesnt_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { reuse: true, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Ensure start() skips the stage() call is a custom config is provided
    #[fuchsia::test]
    async fn test_custom_config_doesnt_stage() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { config: Some("filename".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_| {
                Ok(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Check that the final command reflects changes from the edit stage
    #[fuchsia::test]
    async fn test_edit() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand { edit: true, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
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
            .times(1);

        tool.engine_operations
            .expect_edit_configuration()
            .returning(|config| {
                config.runtime.name = "EditedValue".to_string();
                Ok(())
            })
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Check that the final command reflects changes from staging
    #[fuchsia::test]
    async fn test_staging_edits() -> Result<()> {
        let _env = ffx_config::test_init().await.unwrap();
        let cmd = StartCommand::default();

        let mut tool = make_test_emu_start_tool(cmd).await;

        tool.engine_operations
            .expect_new_engine()
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
            .times(1);
        let _ = tool.main(SimpleWriter::new()).await?;
        Ok(())
    }
}
