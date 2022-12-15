// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The femu module encapsulates the interactions with the emulator instance
//! started via the Fuchsia emulator, Femu.

use crate::{qemu_based::QemuBasedEngine, serialization::SerializingEngine};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use ffx_emulator_common::{config, process, target::remove_target};
use ffx_emulator_config::{
    EmulatorConfiguration, EmulatorEngine, EngineConsoleType, EngineState, EngineType, ShowDetail,
};
use fidl_fuchsia_developer_ffx as ffx;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Command};

use super::get_host_tool;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FemuEngine {
    #[serde(default)]
    pub(crate) emulator_binary: PathBuf,
    pub(crate) emulator_configuration: EmulatorConfiguration,
    pub(crate) pid: u32,
    pub(crate) engine_type: EngineType,
    #[serde(default)]
    pub(crate) engine_state: EngineState,
}

impl FemuEngine {
    fn validate_configuration(&self) -> Result<()> {
        if !self.emulator_configuration.runtime.headless && std::env::var("DISPLAY").is_err() {
            eprintln!(
                "DISPLAY not set in the local environment, try running with --headless if you \
                encounter failures related to display or Qt.",
            );
        }
        self.validate_network_flags(&self.emulator_configuration)
            .and_then(|()| self.check_required_files(&self.emulator_configuration.guest))
    }

    fn validate_staging(&self) -> Result<()> {
        self.check_required_files(&self.emulator_configuration.guest)
    }
}

#[async_trait]
impl EmulatorEngine for FemuEngine {
    async fn stage(&mut self) -> Result<()> {
        let result = <Self as QemuBasedEngine>::stage(&mut self)
            .await
            .and_then(|()| self.validate_staging());
        if result.is_ok() {
            self.engine_state = EngineState::Staged;
            self.save_to_disk()
        } else {
            self.engine_state = EngineState::Error;
            self.save_to_disk().with_context(|| format!("{:?}", result.unwrap_err()))
        }
    }

    async fn start(
        &mut self,
        emulator_cmd: Command,
        proxy: &ffx::TargetCollectionProxy,
    ) -> Result<i32> {
        self.run(emulator_cmd, proxy).await
    }

    fn show(&self, details: Vec<ShowDetail>) {
        <Self as QemuBasedEngine>::show(self, details)
    }

    async fn stop(&mut self, proxy: &ffx::TargetCollectionProxy) -> Result<()> {
        let name = &self.emu_config().runtime.name;
        if let Err(e) = remove_target(proxy, name).await {
            // Even if we can't remove it, still continue shutting down.
            tracing::warn!("Couldn't remove target from ffx during shutdown: {:?}", e);
        }
        self.stop_emulator()
    }

    fn configure(&mut self) -> Result<()> {
        let result = if self.emu_config().runtime.config_override {
            println!("Custom configuration provided; bypassing validation.");
            Ok(())
        } else {
            self.validate_configuration()
        };
        if result.is_ok() {
            self.engine_state = EngineState::Configured;
        } else {
            self.engine_state = EngineState::Error;
        }
        result
    }

    fn engine_state(&self) -> EngineState {
        self.get_engine_state()
    }

    fn engine_type(&self) -> EngineType {
        self.engine_type
    }

    fn is_running(&mut self) -> bool {
        let running = process::is_running(self.pid);
        if self.engine_state() == EngineState::Running && running == false {
            self.set_engine_state(EngineState::Staged);
            if self.save_to_disk().is_err() {
                tracing::warn!("Problem saving serialized emulator to disk during state update.");
            }
        }
        running
    }

    fn attach(&self, console: EngineConsoleType) -> Result<()> {
        self.attach_to(&self.emulator_configuration.runtime.instance_directory, console)
    }

    /// Build the Command to launch Android emulator running Fuchsia.
    fn build_emulator_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.emulator_binary);
        let feature_arg = self
            .emulator_configuration
            .flags
            .features
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if feature_arg.len() > 0 {
            cmd.arg("-feature").arg(feature_arg);
        }
        cmd.args(&self.emulator_configuration.flags.options)
            .arg("-fuchsia")
            .args(&self.emulator_configuration.flags.args);
        let extra_args = self
            .emulator_configuration
            .flags
            .kernel_args
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if extra_args.len() > 0 {
            cmd.args(["-append", &extra_args]);
        }
        if self.emulator_configuration.flags.envs.len() > 0 {
            cmd.envs(&self.emulator_configuration.flags.envs);
        }
        cmd
    }

    /// Get the AEMU binary path from the SDK manifest and verify it exists.
    async fn load_emulator_binary(&mut self) -> Result<()> {
        self.emulator_binary = match get_host_tool(config::FEMU_TOOL).await {
            Ok(aemu_path) => aemu_path.canonicalize().context(format!(
                "Failed to canonicalize the path to the emulator binary: {:?}",
                aemu_path
            ))?,
            Err(e) => {
                bail!("Cannot find {} in the SDK: {:?}", config::FEMU_TOOL, e);
            }
        };

        if !self.emulator_binary.exists() || !self.emulator_binary.is_file() {
            bail!("Giving up finding emulator binary. Tried {:?}", self.emulator_binary)
        }
        Ok(())
    }

    fn emu_config(&self) -> &EmulatorConfiguration {
        return &self.emulator_configuration;
    }

    fn emu_config_mut(&mut self) -> &mut EmulatorConfiguration {
        return &mut self.emulator_configuration;
    }

    fn save_to_disk(&self) -> Result<()> {
        self.write_to_disk(&self.emu_config().runtime.instance_directory)
    }
}

impl SerializingEngine for FemuEngine {}

impl QemuBasedEngine for FemuEngine {
    fn set_pid(&mut self, pid: u32) {
        self.pid = pid;
    }

    fn get_pid(&self) -> u32 {
        self.pid
    }

    fn set_engine_state(&mut self, state: EngineState) {
        self.engine_state = state;
    }

    fn get_engine_state(&self) -> EngineState {
        self.engine_state
    }
}
