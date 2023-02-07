// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The qemu module encapsulates the interactions with the emulator instance
//! started via the QEMU emulator.
//! Some of the functions related to QEMU are pub(crate) to allow reuse by
//! femu module since femu is a wrapper around an older version of QEMU.

use crate::{qemu_based::QemuBasedEngine, serialization::SerializingEngine};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use ffx_emulator_common::{config::QEMU_TOOL, process, target::remove_target};
use ffx_emulator_config::{
    CpuArchitecture, EmulatorConfiguration, EmulatorEngine, EngineConsoleType, EngineState,
    EngineType, PointingDevice, ShowDetail,
};
use fidl_fuchsia_developer_ffx as ffx;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::get_host_tool;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct QemuEngine {
    #[serde(default)]
    pub(crate) emulator_binary: PathBuf,
    pub(crate) emulator_configuration: EmulatorConfiguration,
    pub(crate) pid: u32,
    pub(crate) engine_type: EngineType,
    #[serde(default)]
    pub(crate) engine_state: EngineState,
}

impl QemuEngine {
    fn validate_configuration(&self) -> Result<()> {
        if self.emulator_configuration.device.pointing_device == PointingDevice::Touch {
            eprintln!("Touchscreen as a pointing device is not available on Qemu.");
            eprintln!(
                "If you encounter errors, try changing the pointing device to 'mouse' in the \
                Virtual Device specification."
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
impl EmulatorEngine for QemuEngine {
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

    /// Build the Command to launch Qemu emulator running Fuchsia.
    fn build_emulator_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.emulator_binary);
        cmd.args(&self.emulator_configuration.flags.args);
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

    /// Loads the path to the qemu binary to execute. This is based on the guest OS architecture.
    ///
    /// Currently this is done by getting the default CLI which is for x64 images, and then
    /// replacing it if the guest OS is arm64.
    /// TODO(http://fxdev.bug/98862): Improve the SDK metadata to have multiple binaries per tool.
    async fn load_emulator_binary(&mut self) -> Result<()> {
        let cli_name = match self.emulator_configuration.device.cpu.architecture {
            CpuArchitecture::Arm64 => Some("qemu-system-aarch64"),
            CpuArchitecture::X64 => None,
            CpuArchitecture::Unsupported => None,
        };

        let qemu_x64_path = match get_host_tool(QEMU_TOOL).await {
            Ok(qemu_path) => qemu_path.canonicalize().context(format!(
                "Failed to canonicalize the path to the emulator binary: {:?}",
                qemu_path
            ))?,
            Err(e) => bail!("Cannot find {} in the SDK: {:?}", QEMU_TOOL, e),
        };

        // If we need to, replace the executable name.
        self.emulator_binary = if let Some(exe_name) = cli_name {
            // Realistically, the file is always in a directory, so the empty path is a reasonable
            // fallback since it will "never" happen
            let mut p = PathBuf::from(qemu_x64_path.parent().unwrap_or(Path::new("")));
            p.push(exe_name);
            p
        } else {
            qemu_x64_path
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

#[async_trait]
impl SerializingEngine for QemuEngine {}

impl QemuBasedEngine for QemuEngine {
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
