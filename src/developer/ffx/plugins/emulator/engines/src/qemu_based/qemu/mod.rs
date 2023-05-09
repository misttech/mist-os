// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The qemu module encapsulates the interactions with the emulator instance
//! started via the QEMU emulator.
//! Some of the functions related to QEMU are pub(crate) to allow reuse by
//! femu module since femu is a wrapper around an older version of QEMU.

use super::get_host_tool;
use crate::qemu_based::QemuBasedEngine;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use emulator_instance::{
    get_instance_dir, write_to_disk, CpuArchitecture, EmulatorConfiguration, EmulatorInstanceData,
    EmulatorInstanceInfo, EngineState, EngineType, PointingDevice,
};
use ffx_emulator_common::config::QEMU_TOOL;
use ffx_emulator_config::{EmulatorEngine, EngineConsoleType, ShowDetail};
use fidl_fuchsia_developer_ffx as ffx;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct QemuEngine {
    data: EmulatorInstanceData,
}

impl QemuEngine {
    pub(crate) fn new(data: EmulatorInstanceData) -> Self {
        Self { data }
    }

    fn validate_configuration(&self) -> Result<()> {
        if self.data.get_emulator_configuration().device.pointing_device == PointingDevice::Touch
            && !self.data.get_emulator_configuration().runtime.headless
        {
            eprintln!("Touchscreen as a pointing device is not available on Qemu.");
            eprintln!(
                "If you encounter errors, try changing the pointing device to 'mouse' in the \
                Virtual Device specification."
            );
        }
        self.validate_network_flags(&self.data.get_emulator_configuration())
            .and_then(|()| self.check_required_files(&self.data.get_emulator_configuration().guest))
    }

    fn validate_staging(&self) -> Result<()> {
        self.check_required_files(&self.data.get_emulator_configuration().guest)
    }
}

#[async_trait]
impl EmulatorEngine for QemuEngine {
    fn get_instance_data(&self) -> &EmulatorInstanceData {
        &self.data
    }

    async fn stage(&mut self) -> Result<()> {
        let result = <Self as QemuBasedEngine>::stage(&mut self)
            .await
            .and_then(|()| self.validate_staging());
        match result {
            Ok(()) => {
                self.data.set_engine_state(EngineState::Staged);
                self.save_to_disk().await
            }
            Err(e) => {
                self.data.set_engine_state(EngineState::Error);
                self.save_to_disk().await.with_context(|| format!("{:?}", &e)).and(Err(e))
            }
        }
    }

    async fn start(
        &mut self,
        emulator_cmd: Command,
        proxy: &ffx::TargetCollectionProxy,
    ) -> Result<i32> {
        self.run(emulator_cmd, proxy).await
    }

    fn show(&self, details: Vec<ShowDetail>) -> Vec<ShowDetail> {
        <Self as QemuBasedEngine>::show(self, details)
    }

    async fn stop(&mut self) -> Result<()> {
        self.stop_emulator().await
    }

    fn configure(&mut self) -> Result<()> {
        let result = if self.emu_config().runtime.config_override {
            println!("Custom configuration provided; bypassing validation.");
            Ok(())
        } else {
            self.validate_configuration()
        };
        if result.is_ok() {
            self.data.set_engine_state(EngineState::Configured);
        } else {
            self.data.set_engine_state(EngineState::Error);
        }
        result
    }

    fn engine_state(&self) -> EngineState {
        self.get_engine_state()
    }

    fn engine_type(&self) -> EngineType {
        self.data.get_engine_type()
    }

    async fn is_running(&mut self) -> bool {
        let running = self.data.is_running();
        if self.engine_state() == EngineState::Running && running == false {
            self.set_engine_state(EngineState::Staged);
            if self.save_to_disk().await.is_err() {
                tracing::warn!("Problem saving serialized emulator to disk during state update.");
            }
        }
        running
    }

    fn attach(&self, console: EngineConsoleType) -> Result<()> {
        self.attach_to(&self.data.get_emulator_configuration().runtime.instance_directory, console)
    }

    /// Build the Command to launch Qemu emulator running Fuchsia.
    fn build_emulator_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.data.get_emulator_binary());
        let emulator_configuration = self.data.get_emulator_configuration();
        cmd.args(&emulator_configuration.flags.args);
        let extra_args = emulator_configuration
            .flags
            .kernel_args
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if extra_args.len() > 0 {
            cmd.args(["-append", &extra_args]);
        }
        if emulator_configuration.flags.envs.len() > 0 {
            cmd.envs(&emulator_configuration.flags.envs);
        }
        cmd
    }

    /// Loads the path to the qemu binary to execute. This is based on the guest OS architecture.
    ///
    /// Currently this is done by getting the default CLI which is for x64 images, and then
    /// replacing it if the guest OS is arm64.
    /// TODO(http://fxdev.bug/98862): Improve the SDK metadata to have multiple binaries per tool.
    async fn load_emulator_binary(&mut self) -> Result<()> {
        let cli_name = match self.data.get_emulator_configuration().device.cpu.architecture {
            CpuArchitecture::Arm64 => Some("qemu-system-aarch64"),
            _ => None,
        };

        let qemu_x64_path = match get_host_tool(QEMU_TOOL).await {
            Ok(qemu_path) => qemu_path.canonicalize().context(format!(
                "Failed to canonicalize the path to the emulator binary: {:?}",
                qemu_path
            ))?,
            Err(e) => bail!("Cannot find {} in the SDK: {:?}", QEMU_TOOL, e),
        };

        // If we need to, replace the executable name.
        let emulator_binary = if let Some(exe_name) = cli_name {
            // Realistically, the file is always in a directory, so the empty path is a reasonable
            // fallback since it will "never" happen
            let mut p = PathBuf::from(qemu_x64_path.parent().unwrap_or(Path::new("")));
            p.push(exe_name);
            p
        } else {
            qemu_x64_path
        };

        if !emulator_binary.exists() || !emulator_binary.is_file() {
            bail!("Giving up finding emulator binary. Tried {:?}", emulator_binary)
        }
        self.data.set_emulator_binary(emulator_binary);

        Ok(())
    }

    fn emu_config(&self) -> &EmulatorConfiguration {
        self.data.get_emulator_configuration()
    }

    fn emu_config_mut(&mut self) -> &mut EmulatorConfiguration {
        self.data.get_emulator_configuration_mut()
    }

    async fn save_to_disk(&self) -> Result<()> {
        write_to_disk(
            &self.data,
            &get_instance_dir(self.data.get_name(), true)
                .await
                .unwrap_or_else(|_| panic!("instance directory for {}", self.data.get_name())),
        )
    }
}

impl QemuBasedEngine for QemuEngine {
    fn set_pid(&mut self, pid: u32) {
        self.data.set_pid(pid);
    }

    fn get_pid(&self) -> u32 {
        self.data.get_pid()
    }

    fn set_engine_state(&mut self, state: EngineState) {
        self.data.set_engine_state(state)
    }

    fn get_engine_state(&self) -> EngineState {
        self.data.get_engine_state()
    }
}
