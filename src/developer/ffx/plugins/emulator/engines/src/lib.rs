// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The ffx_emulator_engines crate contains the implementation
//! of each emulator "engine" such as aemu and qemu.

mod arg_templates;
mod qemu_based;
pub mod serialization;
mod show_output;

use anyhow::{bail, Context, Result};
pub use arg_templates::process_flag_template;
use emulator_instance::{
    get_instance_dir, read_from_disk, DeviceConfig, EmulatorConfiguration, EmulatorInstanceData,
    EmulatorInstanceInfo, EngineOption, EngineState, EngineType, FlagData, GuestConfig, HostConfig,
    RuntimeConfig,
};
use ffx_emulator_config::EmulatorEngine;
use qemu_based::{femu::FemuEngine, qemu::QemuEngine};

/// The EngineBuilder is used to create and configure an EmulatorEngine, while ensuring the
/// configuration will result in a valid emulation instance.
///
/// Create an EngineBuilder using EngineBuilder::new(). This will populate the builder with the
/// defaults for all configuration options. Then use the setter methods to update configuration
/// options, and call "build()" when configuration is complete.
///
/// Setters are independent, optional, and idempotent; i.e. callers may call as many or as few of
/// the setters as needed, and repeat calls if necessary. However, setters consume the data that
/// are passed in, so the caller must set up a new structure for each call.
///
/// Once "build" is called, an engine will be instantiated of the indicated type, the configuration
/// will be loaded into that engine, and the engine's "configure" function will be invoked to
/// trigger validation and ensure the configuration is acceptable. If validation fails, the engine
/// will be destroyed. The EngineBuilder instance is consumed when invoking "build" regardless of
/// the outcome.
///
/// Example:
///
///    let builder = EngineBuilder::new()
///         .engine_type(EngineType::Femu)
///         .device(my_device_config)
///         .guest(my_guest_config)
///         .host(my_host_config)
///         .runtime(my_runtime_config);
///
///     let mut engine: Box<dyn EmulatorEngine> = builder.build()?;
///     (*engine).start().await
///
pub struct EngineBuilder {
    emulator_configuration: EmulatorConfiguration,
    engine_type: EngineType,
}

impl EngineBuilder {
    /// Create a new EngineBuilder, populated with default values for all configuration.
    pub fn new() -> Self {
        Self {
            emulator_configuration: EmulatorConfiguration::default(),
            engine_type: EngineType::default(),
        }
    }

    /// Set the configuration to use when building a new engine.
    pub fn config(mut self, config: EmulatorConfiguration) -> EngineBuilder {
        self.emulator_configuration = config;
        self
    }

    /// Set the engine's virtual device configuration.
    pub fn device(mut self, device_config: DeviceConfig) -> EngineBuilder {
        self.emulator_configuration.device = device_config;
        self
    }

    /// Set the type of the engine to be built.
    pub fn engine_type(mut self, engine_type: EngineType) -> EngineBuilder {
        self.engine_type = engine_type;
        self
    }

    /// Set the engine's guest configuration.
    pub fn guest(mut self, guest_config: GuestConfig) -> EngineBuilder {
        self.emulator_configuration.guest = guest_config;
        self
    }

    /// Set the engine's host configuration.
    pub fn host(mut self, host_config: HostConfig) -> EngineBuilder {
        self.emulator_configuration.host = host_config;
        self
    }

    /// Set the engine's runtime configuration.
    pub fn runtime(mut self, runtime_config: RuntimeConfig) -> EngineBuilder {
        self.emulator_configuration.runtime = runtime_config;
        self
    }

    /// Create from an existing EmulatorInstanceData,
    /// Does not validate or perform any configuration steps. Call
    /// |build| for those steps to be performed.
    pub fn from_data(data: EmulatorInstanceData) -> Result<Box<dyn EmulatorEngine>> {
        let engine: Box<dyn EmulatorEngine> = match data.get_engine_type() {
            EngineType::Femu => Box::new(FemuEngine::new(data)),
            EngineType::Qemu => Box::new(QemuEngine::new(data)),
        };
        Ok(engine)
    }

    /// Finalize and validate the configuration, set up the engine's instance directory,
    /// and return the built engine.
    pub async fn build(mut self) -> Result<Box<dyn EmulatorEngine>> {
        // Set up the instance directory, now that we have enough information.
        let name = &self.emulator_configuration.runtime.name;
        self.emulator_configuration.runtime.engine_type = self.engine_type;
        self.emulator_configuration.runtime.instance_directory =
            get_instance_dir(name, true).await?;

        // Make sure we don't overwrite an existing instance.
        if let Ok(EngineOption::DoesExist(instance_data)) = read_from_disk(name).await {
            if instance_data.is_running() {
                bail!(
                    "An emulator named {} is already running. \
                    Use a different name, or run `ffx emu stop {}` \
                    to stop the running emulator.",
                    name,
                    name
                );
            }
        }

        // Build and complete configuration on the engine, then pass it back to the caller.
        let instance_data = EmulatorInstanceData::new(
            self.emulator_configuration,
            self.engine_type,
            EngineState::Configured,
        );

        let mut engine: Box<dyn EmulatorEngine> = Self::from_data(instance_data)?;
        engine.configure()?;

        engine.load_emulator_binary().await.with_context(|| {
            format!("Failed to load the emulator binary path for {}", self.engine_type)
        })?;

        engine.emu_config_mut().flags = process_flag_template(engine.emu_config())
            .context("Failed to process the flags template file.")?;
        engine
            .save_to_disk()
            .await
            .context("Failed to write the emulation configuration file to disk.")?;

        Ok(engine)
    }
}

// Given the string representation of a flag template, apply the provided configuration to resolve
// the template into a FlagData object.
pub fn process_flags_from_str(text: &str, emu_config: &EmulatorConfiguration) -> Result<FlagData> {
    arg_templates::process_flags_from_str(text, emu_config)
}
