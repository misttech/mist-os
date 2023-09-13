// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::pbm::{get_virtual_devices, make_configs};
use anyhow::{Context, Result};
use async_trait::async_trait;
use emulator_instance::{clean_up_instance_dir, EmulatorConfiguration, EngineType, NetworkingMode};
use errors::ffx_bail;
use ffx_config::Sdk;
use ffx_emulator_common::get_file_hash;
use ffx_emulator_config::EmulatorEngine;
use ffx_emulator_engines::{process_flag_template, EngineBuilder};
use ffx_emulator_start_args::StartCommand;
use fho::{daemon_protocol, FfxContext, FfxMain, FfxTool, SimpleWriter, TryFromEnv};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use pbms::{ListingMode, LoadedProductBundle};
use std::str::FromStr;

mod editor;
mod pbm;

pub(crate) const DEFAULT_NAME: &str = "fuchsia-emulator";

/// EngineOperations trait is used to allow mocking of
/// these methods.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait EngineOperations: TryFromEnv + 'static {
    async fn get_engine_by_name(
        &self,
        name: &mut Option<String>,
    ) -> Result<Option<Box<dyn EmulatorEngine>>>;

    fn edit_configuration(&self, emu_config: &mut EmulatorConfiguration) -> Result<()>;

    async fn new_engine(
        &self,
        emulator_configuration: &EmulatorConfiguration,
        engine_type: EngineType,
    ) -> Result<Box<dyn EmulatorEngine>>;

    async fn load_product_bundle(
        &self,
        sdk: &ffx_config::Sdk,
        product_bundle: &Option<String>,
        mode: ListingMode,
    ) -> Result<LoadedProductBundle>;

    async fn clean_up_instance_dir(&self, instance_name: &str) -> Result<()>;
}

#[derive(Default)]
pub struct EngineOperationsData;

#[async_trait(?Send)]
impl TryFromEnv for EngineOperationsData {
    async fn try_from_env(_env: &fho::FhoEnvironment) -> Result<Self, fho::Error> {
        Ok(Self::default())
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

    async fn new_engine(
        &self,
        emulator_configuration: &EmulatorConfiguration,
        engine_type: EngineType,
    ) -> Result<Box<dyn EmulatorEngine>> {
        EngineBuilder::new()
            .config(emulator_configuration.clone())
            .engine_type(engine_type)
            .build()
            .await
    }

    async fn load_product_bundle(
        &self,
        sdk: &ffx_config::Sdk,
        product_bundle: &Option<String>,
        mode: ListingMode,
    ) -> Result<LoadedProductBundle> {
        pbms::load_product_bundle(sdk, product_bundle, mode).await
    }

    async fn clean_up_instance_dir(&self, instance_name: &str) -> Result<()> {
        clean_up_instance_dir(instance_name).await
    }
}

/// Sub-sub tool for `emu start`
#[derive(FfxTool)]
pub struct EmuStartTool<T: EngineOperations> {
    #[command]
    cmd: StartCommand,
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
        let loaded_product_bundle = self.finalize_start_command().await?;

        let product_bundle: Option<pbms::ProductBundle> = loaded_product_bundle.map(|b| b.into());

        // List the devices available in this product bundle
        if self.cmd.device_list {
            let virtual_devices = get_virtual_devices(&product_bundle.unwrap(), &self.sdk).await?;
            if virtual_devices.is_empty() {
                println!("There are no virtual devices configured for this product bundle");
            } else {
                println!("Valid virtual device specifications are: {:?}", virtual_devices);
            }
            return Ok(());
        }

        let emulator_configuration = make_configs(&self.cmd, product_bundle.clone()).await?;
        let engine_type =
            EngineType::from_str(&self.cmd.engine().await.unwrap_or("femu".to_string()))
                .context("Reading engine type from ffx config.")?;

        // Get the staged instance, if any
        let mut existing = self.engine_operations.get_engine_by_name(&mut self.cmd.name).await?;

        // Check that it is not running.
        if let Some(ref mut existing_instance) = existing {
            let name = self.cmd.name.as_ref().unwrap();
            if existing_instance.is_running().await {
                ffx_bail!("An existing emulator instance named {name} is already running");
            } else if !self.cmd.reuse && !self.cmd.reuse_with_check {
                if let Some(cleanup_err) =
                    self.engine_operations.clean_up_instance_dir(&name).await.err()
                {
                    ffx_bail!("Cleanup of '{name}' failed with the following error: {cleanup_err}");
                }
                existing = None;
            }
        }

        let mut engine = self.get_engine(&emulator_configuration, engine_type, existing).await?;

        if self.cmd.config.is_none() && !self.cmd.reuse && !self.cmd.dry_run {
            // We don't stage files for custom configurations, because the EmulatorConfiguration
            // doesn't hold valid paths to the system images.
            engine
                .stage()
                .await
                .user_message("Error staging the emulator's instance directory.")?;

            if self.cmd.stage {
                if self.cmd.verbose {
                    let emulator_cmd = engine.build_emulator_cmd();
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
        }

        let emulator_cmd = engine.build_emulator_cmd();
        if self.cmd.verbose || self.cmd.dry_run {
            println!("\n[emulator] Final Command line: {:?}\n", emulator_cmd);
            println!("[emulator] With ENV: {:?}\n", emulator_cmd.get_envs());
        }

        // If we're just staging the instance, do not call start.
        if !self.cmd.stage && !self.cmd.dry_run {
            match engine.start(emulator_cmd, &self.target_collection).await {
                Ok(0) => Ok(()),
                Ok(_) => ffx_bail!("Non zero return code"),
                Err(e) => ffx_bail!("{:?}", e.context("The emulator failed to start.")),
            }
        } else {
            Ok(())
        }
    }
}

impl<T: EngineOperations> EmuStartTool<T> {
    async fn get_engine(
        &mut self,
        emulator_configuration: &EmulatorConfiguration,
        engine_type: EngineType,
        existing_engine: Option<Box<dyn EmulatorEngine>>,
    ) -> Result<Box<dyn EmulatorEngine>> {
        let mut engine: Box<dyn EmulatorEngine>;
        // For reuse with check, we need to compare the existing hashes of the zbi and disk
        // to the product bundle. If they are the same, reuse the existing configuration and
        // data.
        //
        // If they are different, then restage from the product bundle.
        if self.cmd.reuse_with_check {
            if let Some(existing_engine) = existing_engine {
                let reused: bool;
                (reused, engine) =
                    self.check_if_reusable(&emulator_configuration, existing_engine).await?;

                if reused {
                    self.cmd.reuse = true;
                    println!("Reusing existing instance.");
                } else {
                    // They do not match, so don't reuse and reset the engine.
                    self.cmd.reuse = false;
                    println!("Created new instance. Product bundle data has changed.");
                }
            } else {
                tracing::debug!("No existing instance to check as reusable.");
                engine = self
                    .engine_operations
                    .new_engine(&emulator_configuration, engine_type)
                    .await
                    .user_message("Error creating new engine")?;
                let config = engine.emu_config_mut();
                Self::save_disk_hashes(config)?;
            }
        } else {
            if self.cmd.reuse && !emulator_configuration.runtime.config_override {
                if let Some(existing_instance) = existing_engine {
                    engine = existing_instance;
                    // Reset the runtime config before reusing
                    // Reset the host port map.
                    if engine.emu_config().host.networking == NetworkingMode::User {
                        engine.emu_config_mut().host.port_map =
                            emulator_configuration.host.port_map.clone();
                    }
                    // Set the log file
                    let config = engine.emu_config_mut();
                    config.host.log = emulator_configuration.host.log.clone();
                    config.runtime.startup_timeout =
                        emulator_configuration.runtime.startup_timeout.clone();
                    config.runtime.log_level = emulator_configuration.runtime.log_level.clone();

                    // And regenerate the flags
                    config.flags = process_flag_template(config)
                        .context("Failed to process the flags template file.")?;

                    engine.save_to_disk().await?;
                    println!("Reusing existing instance.");
                } else {
                    let message = format!(
                        "Instance '{name}' not found with --reuse flag. \
                        Creating a new emulator named '{name}'.",
                        name = emulator_configuration.runtime.name
                    );
                    tracing::debug!("{message}");
                    println!("{message}");
                    self.cmd.reuse = false;
                    engine = self
                        .engine_operations
                        .new_engine(&emulator_configuration, engine_type)
                        .await
                        .user_message("Error creating new engine")?
                }
            } else {
                engine = if !emulator_configuration.runtime.config_override
                    && existing_engine.is_some()
                {
                    existing_engine.expect("existing engine instance")
                } else {
                    self.engine_operations
                        .new_engine(&emulator_configuration, engine_type)
                        .await
                        .user_message("Error creating new engine")?
                }
            }
        }
        Ok(engine)
    }

    async fn finalize_start_command(&mut self) -> Result<Option<LoadedProductBundle>> {
        // name is important to not be empty since it is used to
        // create a directory path.
        let mut name = self.cmd.name().await?;
        if self.cmd.name.is_none() || name == "" {
            if name == "" {
                name = DEFAULT_NAME.into();
            }

            self.cmd.name = Some(name.into());
        }

        // if a custom config is used, skip the product bundle checks.
        if self.cmd.config.is_none() {
            let loaded_product_bundle = self
                .engine_operations
                .load_product_bundle(
                    &self.sdk,
                    &self.cmd.product_bundle,
                    ListingMode::ReadyBundlesOnly,
                )
                .await?;

            // if we're just printing a device list, return
            if self.cmd.device_list {
                return Ok(Some(loaded_product_bundle));
            }

            let engine = self.cmd.engine().await?;
            if self.cmd.engine.is_none() && engine != "" {
                self.cmd.engine = Some(engine);
            }

            let gpu = self.cmd.gpu().await?;
            if self.cmd.gpu.is_none() && gpu != "" {
                self.cmd.gpu = Some(gpu);
            }

            let net = self.cmd.net().await?;
            if self.cmd.net.is_none() && net != "" {
                self.cmd.net = Some(net);
            }

            let startup_timeout = self.cmd.startup_timeout().await?;
            if self.cmd.startup_timeout.is_none() && startup_timeout > 0 {
                self.cmd.startup_timeout = Some(startup_timeout);
            }

            if self.cmd.product_bundle.is_none() {
                self.cmd.product_bundle = Some(loaded_product_bundle.loaded_from_path().to_string())
            }

            if self.cmd.device.is_none() {
                let virtual_devices =
                    get_virtual_devices(&loaded_product_bundle, &self.sdk).await?;
                if virtual_devices.is_empty() {
                    ffx_bail!("There are no virtual devices configured for this product bundle")
                }
                // Virtual device spec name
                if let Some(device_name) = self.cmd.device().await? {
                    if self.cmd.device.is_none() && device_name != "" {
                        self.cmd.device = Some(device_name);
                    } else {
                        self.cmd.device = Some(loaded_product_bundle.device_refs()?[0].clone());
                    }
                } else {
                    self.cmd.device = Some(loaded_product_bundle.device_refs()?[0].clone());
                }
            }
            return Ok(Some(loaded_product_bundle));
        }
        Ok(None)
    }

    /// Checks the configuration of the given engine against the
    /// product bundle based on the command line. If they match,
    /// the given engine is reusable, and returned, otherwise a
    /// new engine instance is returned.
    async fn check_if_reusable(
        &self,
        new_config: &EmulatorConfiguration,
        mut engine: Box<dyn EmulatorEngine>,
    ) -> Result<(bool, Box<dyn EmulatorEngine>)> {
        let new_zbi_hash: u64;
        let new_disk_hash: u64;

        tracing::debug!(
            "New config image files zbi:{zbi:?} disk:{fvm:?}",
            zbi = new_config.guest.zbi_image,
            fvm = new_config.guest.disk_image
        );

        new_zbi_hash = get_file_hash(&new_config.guest.zbi_image)
            .bug_context("could not calculate zbi hash")?;

        if let Some(disk) = &new_config.guest.disk_image {
            new_disk_hash =
                get_file_hash(disk.as_ref()).bug_context("could not calculate disk hash")?;
        } else {
            new_disk_hash = 0;
        }

        let new_zbi = format!("{new_zbi_hash:x}");
        let new_disk = format!("{new_disk_hash:x}");
        let config = engine.emu_config();
        let zbi_hash = &config.guest.zbi_hash;
        let disk_hash = &config.guest.disk_hash;

        // If the hashes match, reuse the instance. Reset the config properties that are
        // dynamic and should be set from the command line.
        if &new_zbi == zbi_hash && &new_disk == disk_hash {
            // Reset the host port map.
            if engine.emu_config().host.networking == NetworkingMode::User {
                engine.emu_config_mut().host.port_map = new_config.host.port_map.clone();
            }
            // Set the log file
            let config = engine.emu_config_mut();
            config.host.log = new_config.host.log.clone();
            config.runtime.startup_timeout = new_config.runtime.startup_timeout.clone();
            config.runtime.log_level = new_config.runtime.log_level.clone();

            // And regenerate the flags
            config.flags = process_flag_template(config)
                .context("Failed to process the flags template file.")?;

            engine.save_to_disk().await?;
            return Ok((true, engine));
        } else {
            let engine_type =
                EngineType::from_str(&self.cmd.engine().await.unwrap_or("femu".to_string()))
                    .context("Reading engine type from ffx config.")?;
            engine = self.engine_operations.new_engine(&new_config, engine_type).await?;
            let config = engine.emu_config_mut();
            config.guest.zbi_hash = new_zbi.clone();
            config.guest.disk_hash = new_disk.clone();
            return Ok((false, engine));
        }
    }

    fn save_disk_hashes(config: &mut EmulatorConfiguration) -> Result<()> {
        let new_disk_hash: u64;
        let new_zbi_hash =
            get_file_hash(&config.guest.zbi_image).bug_context("could not calculate zbi hash")?;

        if let Some(disk) = &config.guest.disk_image {
            new_disk_hash =
                get_file_hash(disk.as_ref()).bug_context("could not calculate disk hash")?;
        } else {
            new_disk_hash = 0;
        }
        config.guest.zbi_hash = format!("{new_zbi_hash:x}");
        config.guest.disk_hash = format!("{new_disk_hash:x}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use assembly_manifest::Image;
    use assembly_partitions_config::PartitionsConfig;
    use camino::Utf8PathBuf;
    use emulator_instance::{LogLevel, RuntimeConfig};
    use ffx_config::{BuildOverride, ConfigLevel, SdkRoot, TestEnv};
    use ffx_emulator_config::EmulatorEngine;
    use pbms::ProductBundle;
    use sdk_metadata::ProductBundleV2;
    use serde_json::json;
    use std::{fs, path::Path, process::Command};

    const VIRTUAL_DEVICE_VALID: &str = include_str!("../test_data/virtual_device.json");
    const VIRTUAL_DEVICE_TEMPLATE_VALID: &str = include_str!("../test_data/device_1.json.template");
    const CORE_JSON: &str = include_str!("../test_data/test_core.json");
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
        running: bool,
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
                running: false,
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

        fn emu_config(&self) -> &EmulatorConfiguration {
            &self.config
        }

        async fn is_running(&mut self) -> bool {
            self.running
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

    #[async_trait(?Send)]
    impl TryFromEnv for MockEngineOperations {
        async fn try_from_env(_env: &fho::FhoEnvironment) -> fho::Result<Self> {
            Ok(Self::default())
        }
    }

    async fn make_intree_sdk(env: &TestEnv) -> Result<Sdk> {
        let manifest_path = env.isolate_root.path().join("sdk/manifest");
        std::fs::create_dir_all(&manifest_path).expect("create sdk dir");
        fs::write(manifest_path.join("core"), CORE_JSON)?;

        env.context
            .query("sdk.root")
            .build(Some(BuildOverride::NoBuild))
            .level(Some(ConfigLevel::User))
            .set(json!(env.isolate_root.path().to_path_buf()))
            .await
            .expect("set sdk.root");

        let s = SdkRoot::Full(env.isolate_root.path().to_path_buf());
        s.get_sdk()
    }

    fn make_test_product_bundle(dir: &Path) -> Result<ProductBundleV2> {
        let dev_manifest = dir.join("virtual_device_manifest.json");
        fs::write(&dev_manifest,r#"
        {"recommended":"virtual_device_1","device_paths":{"virtual_device_1":"virtual_device_1.json","virtual_device_2":"virtual_device_2.json"}}
        "#).unwrap();

        let kernel_path = dir.join("kernel.dat");
        fs::write(&kernel_path, "this is kernel file").bug_context("writing test kernel")?;

        let zbi_path = dir.join("zbi-file.zbi");
        fs::write(&zbi_path, "this is zbi file").bug_context("writing test zbi")?;

        let fvm_path = dir.join("fvm-file.fvm");
        fs::write(&fvm_path, "this is fvm file").bug_context("writing test fvm")?;

        fs::write(dir.join("virtual_device_1.json"), VIRTUAL_DEVICE_VALID)
            .expect("writing device json");

        fs::write(dir.join("device_1.json.template"), VIRTUAL_DEVICE_TEMPLATE_VALID)
            .expect("writing template json");

        Ok(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![],
                bootloader_partitions: vec![],
                partitions: vec![],
                hardware_revision: "board".into(),
                unlock_credentials: vec![],
            },
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(kernel_path).expect("utf path from buf"),
                ),
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(zbi_path).expect("utf path from buf"),
                    signed: false,
                },
                Image::FVM(Utf8PathBuf::from_path_buf(fvm_path).expect("utf path from buf")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some(dev_manifest.to_str().unwrap().into()),
        })
    }

    async fn make_test_emu_start_tool(
        cmd: StartCommand,
        sdk: Sdk,
    ) -> EmuStartTool<MockEngineOperations> {
        let (proxy, _) = fidl::endpoints::create_proxy_and_stream::<
            <TargetCollectionProxy as fidl::endpoints::Proxy>::Protocol,
        >()
        .unwrap();

        EmuStartTool {
            cmd,
            engine_operations: MockEngineOperations::new(),
            target_collection: proxy,
            sdk,
        }
    }

    // Check that a running instance is an error
    #[fuchsia::test]
    async fn test_start_with_running_instance() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let cmd = StartCommand::default();
        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: false,
                    running: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        let pb =
            ProductBundle::V2(make_test_product_bundle(env.isolate_root.path()).expect("test pb"));
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        let result = tool.main(SimpleWriter::new()).await;
        assert!(result.is_err())
    }

    // Check that an existing instance that is not running is cleaned up.
    #[fuchsia::test]
    async fn test_start_with_instance_dir() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let cmd = StartCommand::default();
        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: false,
                    running: false,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        tool.engine_operations.expect_new_engine().returning(|_, _| {
            Ok(Box::new(TestEngine {
                do_stage: true,
                do_start: true,
                config: EmulatorConfiguration::default(),
                ..Default::default()
            }) as Box<dyn EmulatorEngine>)
        });

        tool.engine_operations.expect_clean_up_instance_dir().returning(|_| Ok(())).times(1);

        let pb =
            ProductBundle::V2(make_test_product_bundle(env.isolate_root.path()).expect("test pb"));
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        let result = tool.main(SimpleWriter::new()).await;
        assert!(result.is_ok())
    }

    // Check that new_engine gets called by default and get_engine_by_name doesn't
    #[fuchsia::test]
    async fn test_get_engine_no_reuse_makes_new() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand::default();
        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await.expect("main in test_get_engine_no_reuse_makes_new");
        Ok(())
    }

    // Check that reuse and config together is still new_engine (i.e. config overrides reuse)
    #[fuchsia::test]
    async fn test_get_engine_with_config_doesnt_reuse() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand {
            reuse: true,
            net: Some("user".into()),
            config: Some("config.file".into()),
            ..Default::default()
        };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(|_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: false,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        tool.engine_operations.expect_load_product_bundle().times(0);

        tool.main(SimpleWriter::new())
            .await
            .expect("main in test_get_engine_with_config_doesnt_reuse");

        Ok(())
    }

    // Check that reuse and config.is_none calls get_engine_by_name
    #[fuchsia::test]
    async fn test_get_engine_without_config_does_reuse() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        let cmd = StartCommand { reuse: true, net: Some("user".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        let reused_config =
            make_configs(&tool.cmd, Some(pb.clone())).await.expect("reused_config config");

        tool.engine_operations.expect_new_engine().times(0);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(move |_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: reused_config.clone(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new())
            .await
            .expect("main in test_get_engine_without_config_does_reuse");

        Ok(())
    }

    // Check that if get_engine_by_name returns DoesNotExist, new_engine still gets called and reuse is reset
    // reuse is checked to be false, by watching do_stage. stage() is only called on non-reused instances.
    #[fuchsia::test]
    async fn test_get_engine_doesnotexist_creates_new() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand { reuse: true, net: Some("user".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|config, _| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: config.clone(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new())
            .await
            .expect("main in test_get_engine_doesnotexist_creates_new");

        Ok(())
    }

    // Check that if DoesExist, then cmd.name is updated too
    #[fuchsia::test]
    async fn test_get_engine_updates_cmd_name_when_blank() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        env.context.query("emu.name").level(Some(ConfigLevel::User)).set("".into()).await?;
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand { name: None, reuse: true, config: None, ..Default::default() };

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        let reused_config =
            make_configs(&tool.cmd, Some(pb.clone())).await.expect("reused_config config");

        tool.engine_operations.expect_new_engine().times(0);
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(move |name| {
                assert_eq!(name, &Some(DEFAULT_NAME.to_string()));
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: reused_config.clone(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new())
            .await
            .expect("main in test_get_engine_updates_cmd_name_when_blank");
        Ok(())
    }

    // Ensure dry-run stops after building command, doesn't stage/run
    #[fuchsia::test]
    async fn test_dry_run() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand {
            dry_run: true,
            verbose: true,
            net: Some("user".into()),
            ..Default::default()
        };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: false,
                    do_start: false,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await?;

        Ok(())
    }

    // Ensure stage stops after staging the files, doesn't run
    #[fuchsia::test]
    async fn test_stage() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand { stage: true, net: Some("user".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: false,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);

        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None));

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Ensure start goes through config and staging by default and calls start
    #[fuchsia::test]
    async fn test_start() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand::default();

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;
        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);

        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None));

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        let result = tool.main(SimpleWriter::new()).await;
        assert!(result.is_ok(), "{:?}", result.err());
        Ok(())
    }

    // Ensure start() skips the stage() call if the reuse flag is true
    #[fuchsia::test]
    async fn test_reuse_doesnt_stage() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        let cmd = StartCommand { reuse: true, net: Some("user".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        let reused_config =
            make_configs(&tool.cmd, Some(pb.clone())).await.expect("reused_config config");

        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(move |_| {
                Ok(Some(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: reused_config.clone(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);
        tool.engine_operations.expect_new_engine().times(0);

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Ensure start() skips the stage() call is a custom config is provided
    #[fuchsia::test]
    async fn test_custom_config_doesnt_stage() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand { config: Some("filename".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
                Ok(Box::new(TestEngine {
                    do_stage: false,
                    do_start: true,
                    config: EmulatorConfiguration::default(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);

        tool.engine_operations.expect_load_product_bundle().times(0);

        tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Check that the final command reflects changes from the edit stage
    #[fuchsia::test]
    async fn test_edit() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand { edit: true, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
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
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        tool.engine_operations
            .expect_edit_configuration()
            .returning(|config| {
                config.runtime.name = "EditedValue".to_string();
                Ok(())
            })
            .times(1);

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");
        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Check that the final command reflects changes from staging
    #[fuchsia::test]
    async fn test_staging_edits() -> Result<()> {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await?;

        let cmd = StartCommand::default();

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_new_engine()
            .returning(|_, _| {
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

        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        let pb = ProductBundle::V2(make_test_product_bundle(env.isolate_root.path())?);
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.main(SimpleWriter::new()).await?;
        Ok(())
    }

    // Tests that if check_if_reusable is set, but there is no existing instance, it starts a new engine
    // and sets the disk hashes in the config.
    #[fuchsia::test]
    async fn test_check_if_reusable_new() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        let cmd = StartCommand {
            name: Some("reuse-test".into()),
            reuse_with_check: true,
            net: Some("user".into()),
            ..Default::default()
        };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        // Only load the product bundle once.
        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        // Only look for the existing engine once.
        tool.engine_operations.expect_get_engine_by_name().returning(|_| Ok(None)).times(1);

        // Only make the engine once.
        tool.engine_operations
            .expect_new_engine()
            .returning(|config, _| {
                Ok(Box::new(TestEngine {
                    do_stage: true,
                    do_start: true,
                    config: config.clone(),
                    stage_test_fn: |config| {
                        assert_eq!("d53f0b8a19b29d74", config.guest.zbi_hash);
                        assert_eq!("d547336219b6b160", config.guest.disk_hash);
                        Ok(())
                    },
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>)
            })
            .times(1);

        tool.main(SimpleWriter::new()).await.expect("main with stage");
    }

    // Checks that if the existing instance has matching disk hashes, it is reused.
    #[fuchsia::test]
    async fn test_check_if_reusable_matching() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );
        let loaded_pb = LoadedProductBundle::new(pb.clone(), "some/path/to_bundle");

        let cmd = StartCommand {
            name: Some("reuse-test".into()),
            reuse_with_check: true,
            net: Some("user".into()),
            ..Default::default()
        };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        let mut matching_config =
            make_configs(&tool.cmd, Some(pb.clone())).await.expect("matching config");
        matching_config.guest.zbi_hash = "d53f0b8a19b29d74".into();
        matching_config.guest.disk_hash = "d547336219b6b160".into();

        // Only load the product bundle once.
        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        // Only look for the existing engine once, and find it.
        tool.engine_operations
            .expect_get_engine_by_name()
            .returning(move |_| {
                Ok(Some(Box::new(TestEngine {
                    // no staging should happen since we're reusing an instance.
                    do_stage: false,
                    do_start: true,
                    config: matching_config.clone(),
                    ..Default::default()
                }) as Box<dyn EmulatorEngine>))
            })
            .times(1);

        // New engine should not be made.
        tool.engine_operations.expect_new_engine().times(0);

        tool.main(SimpleWriter::new()).await.expect("main with stage");
    }

    // Checks that the command line options are applied to the existing instance when it is reused.
    #[fuchsia::test]
    async fn test_check_if_reusable_applies_args() {
        // Setup the test environment and SDK. This is boilerplate for
        // any test running ffx.
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        // Create a test product bundle. This is boilerplate for
        // any test that needs to use a product bundle. See the
        // make_test_product_bundle function to get the specific contents
        // that are staged in the product bundle.
        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );

        // Create the command line arguments for this test.
        let cmd = StartCommand {
            name: Some("reuse-test".into()),
            reuse_with_check: true,
            net: Some("user".into()),
            verbose: true,
            log: Some(env.isolate_root.path().join("emu.log")),
            ..Default::default()
        };

        // Create a configuration based on the product bundle, adding the
        // disk hashes so it appears that the product bundle has not changed since
        // when the instance was created.
        let mut default_config =
            make_configs(&StartCommand::default(), Some(pb.clone())).await.expect("default config");
        default_config.guest.zbi_hash = "d53f0b8a19b29d74".into();
        default_config.guest.disk_hash = "d547336219b6b160".into();

        // Create the fake test engine instance. In this case since we are only testing if the engine
        // is reusable, set do_stage and do_start to false.
        let existing_engine = Box::new(TestEngine {
            // no staging should happen since we're reusing an instance.
            do_stage: false,
            do_start: false,
            config: default_config.clone(),
            ..Default::default()
        });

        // Make the test instance of the tool, this uses mocks for the engine_operations
        // object.
        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        // Create the configuration that is based on the command line and the product bundle.
        let mut emulator_configuration =
            make_configs(&tool.cmd, Some(pb.clone())).await.expect("cmd configs");
        emulator_configuration.guest.zbi_hash = "d53f0b8a19b29d74".into();
        emulator_configuration.guest.disk_hash = "d547336219b6b160".into();

        // Set the mock expectation new_engine(). New engine should not be called.
        tool.engine_operations.expect_new_engine().times(0);

        // Call the function under test.
        let (reused, engine) = tool
            .check_if_reusable(&emulator_configuration, existing_engine)
            .await
            .expect("check_if_reusable");

        // assert that it was reused and the log path and verbose flag
        // were set to match the command line.
        assert!(reused, "Expected engine to be reused");
        assert_eq!(engine.emu_config().host.log, env.isolate_root.path().join("emu.log"));
        assert_eq!(engine.emu_config().runtime.log_level, LogLevel::Verbose);
    }

    #[fuchsia::test]
    async fn test_finalize_start_command_named() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        let cmd = StartCommand { name: Some("test-instance-name".into()), ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.finalize_start_command().await.unwrap();

        assert_eq!(tool.cmd.name, Some("test-instance-name".into()));
    }

    #[fuchsia::test]
    async fn test_finalize_start_command_noname() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        let cmd = StartCommand { name: None, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.finalize_start_command().await.unwrap();

        assert_eq!(tool.cmd.name, Some(DEFAULT_NAME.into()));
    }

    #[fuchsia::test]
    async fn test_finalize_start_command_nodevice() {
        let env = ffx_config::test_init().await.unwrap();
        let sdk = make_intree_sdk(&env).await.expect("test sdk");

        let pb = ProductBundle::V2(
            make_test_product_bundle(env.isolate_root.path()).expect("test product bundle"),
        );
        let loaded_pb = LoadedProductBundle::new(pb, "some/path/to_bundle");

        let cmd = StartCommand { device: None, ..Default::default() };

        let mut tool = make_test_emu_start_tool(cmd, sdk).await;

        tool.engine_operations
            .expect_load_product_bundle()
            .returning(move |_, _, _| Ok(loaded_pb.clone()))
            .times(1);

        tool.finalize_start_command().await.unwrap();

        // This is set as the recommended device in the product bundle.
        assert_eq!(tool.cmd.device, Some("virtual_device_1".into()));
    }
}
