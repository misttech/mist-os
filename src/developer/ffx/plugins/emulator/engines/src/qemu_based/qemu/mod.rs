// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The qemu module encapsulates the interactions with the emulator instance
//! started via the QEMU emulator.
//! Some of the functions related to QEMU are pub(crate) to allow reuse by
//! femu module since femu is a wrapper around an older version of QEMU.

use super::get_host_tool;
use crate::qemu_based::QemuBasedEngine;
use async_trait::async_trait;
use emulator_instance::{
    write_to_disk, CpuArchitecture, EmulatorConfiguration, EmulatorInstanceData,
    EmulatorInstanceInfo, EmulatorInstances, EngineState, EngineType, NetworkingMode,
    PointingDevice,
};
use ffx_config::EnvironmentContext;
use ffx_emulator_common::config::QEMU_TOOL;
use ffx_emulator_config::{EmulatorEngine, EngineConsoleType, ShowDetail};
use fho::{bug, return_bug, Result};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct QemuEngine {
    data: EmulatorInstanceData,
    emu_instances: EmulatorInstances,
}

impl QemuEngine {
    pub(crate) fn new(data: EmulatorInstanceData, emu_instances: EmulatorInstances) -> Self {
        Self { data, emu_instances }
    }

    fn validate_configuration(&self) -> Result<()> {
        if self.data.get_emulator_configuration().device.pointing_device == PointingDevice::Touch
            && !self.data.get_emulator_configuration().runtime.headless
        {
            let message = format!(
                "{}\n{}",
                "Touchscreen as a pointing device is not available on Qemu.",
                "If you encounter errors, try changing the pointing device to 'mouse' in the \
                Virtual Device specification."
            );
            tracing::info!("{message}");
            eprintln!("{message}");
        }
        self.validate_network_flags(&self.data.get_emulator_configuration())
            .and_then(|()| self.check_required_files(&self.data.get_emulator_configuration().guest))
    }

    fn validate_staging(&self) -> Result<()> {
        self.check_required_files(&self.data.get_emulator_configuration().guest)
    }
}

#[async_trait(?Send)]
impl EmulatorEngine for QemuEngine {
    fn get_instance_data(&self) -> &EmulatorInstanceData {
        &self.data
    }

    async fn stage(&mut self) -> Result<()> {
        // QEMU 9+ does not support auto-assignment of host port
        // when forwarding guest ports mapped via user networking.
        // So we need to assign any missing host ports now, before
        // continuing to stage the instance.
        if self.emu_config().host.networking == NetworkingMode::User {
            crate::finalize_port_mapping(self.emu_config_mut())?;
        }

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
                self.save_to_disk().await.and(Err(e))
            }
        }
    }

    async fn start(&mut self, context: &EnvironmentContext, emulator_cmd: Command) -> Result<i32> {
        self.run(context, emulator_cmd).await
    }

    fn show(&self, details: Vec<ShowDetail>) -> Vec<ShowDetail> {
        <Self as QemuBasedEngine>::show(self, details)
    }

    async fn stop(&mut self) -> Result<()> {
        self.stop_emulator().await
    }

    fn configure(&mut self) -> Result<()> {
        let result = if self.emu_config().runtime.config_override {
            let message = "Custom configuration provided; bypassing validation.";
            eprintln!("{message}");
            tracing::info!("{message}");
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

        // Can't have kernel args if there is no kernel, but if there is a custom configuration template,
        // add them anyway since the configuration and the custom template could be out of sync.
        if emulator_configuration.guest.kernel_image.is_some()
            || emulator_configuration.runtime.config_override
        {
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
        }
        if self.data.get_emulator_configuration().flags.envs.len() > 0 {
            // Add environment variables if not already present.
            // This does not overwrite any existing values.
            let unset_envs =
                emulator_configuration.flags.envs.iter().filter(|(k, _)| env::var(k).is_err());
            if unset_envs.clone().count() > 0 {
                cmd.envs(unset_envs);
            }
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
            CpuArchitecture::Riscv64 => Some("qemu-system-riscv64"),
            _ => None,
        };

        let qemu_x64_path = match get_host_tool(QEMU_TOOL).await {
            Ok(qemu_path) => qemu_path.canonicalize().map_err(|e| {
                bug!("Failed to canonicalize the path to the emulator binary: {qemu_path:?}: {e}")
            })?,
            Err(e) => return_bug!("Cannot find {QEMU_TOOL} in the SDK: {e}"),
        };

        // If we need to, replace the executable name.
        let emulator_binary = if let Some(exe_name) = cli_name {
            // Realistically, the file is always in a directory, so the empty path is a reasonable
            // fallback since it will "never" happen
            let mut p = PathBuf::from(qemu_x64_path.parent().unwrap_or_else(|| Path::new("")));
            p.push(exe_name);
            p
        } else {
            qemu_x64_path
        };

        if !emulator_binary.exists() || !emulator_binary.is_file() {
            return_bug!("Giving up finding emulator binary. Tried {:?}", emulator_binary)
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
            &self
                .emu_instances
                .get_instance_dir(self.data.get_name(), true)
                .unwrap_or_else(|_| panic!("instance directory for {}", self.data.get_name())),
        )
        .map_err(|e| bug!("Error saving instance to disk: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qemu_based::tests::make_fake_sdk;
    use crate::EngineBuilder;
    use emulator_instance::NetworkingMode;
    use std::ffi::OsStr;
    use std::fs;
    use tempfile::tempdir;

    #[fuchsia::test]
    fn test_build_emulator_cmd() {
        let program_name = "/test_femu_bin";
        let mut cfg = EmulatorConfiguration::default();
        cfg.host.networking = NetworkingMode::User;
        cfg.flags.envs.insert("FLAG_NAME_THAT_DOES_NOT_EXIST".into(), "1".into());

        let mut emu_data = EmulatorInstanceData::new(cfg, EngineType::Qemu, EngineState::New);
        emu_data.set_emulator_binary(program_name.into());
        let test_engine = QemuEngine::new(emu_data, EmulatorInstances::new(PathBuf::new()));
        let cmd = test_engine.build_emulator_cmd();
        assert_eq!(cmd.get_program(), program_name);
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            [(OsStr::new("FLAG_NAME_THAT_DOES_NOT_EXIST"), Some(OsStr::new("1")))]
        );
    }

    #[fuchsia::test]
    fn test_build_emulator_cmd_existing_env() {
        env::set_var("FLAG_NAME_THAT_DOES_EXIST", "preset_value");
        let program_name = "/test_femu_bin";
        let mut cfg = EmulatorConfiguration::default();
        cfg.host.networking = NetworkingMode::User;
        cfg.flags.envs.insert("FLAG_NAME_THAT_DOES_EXIST".into(), "1".into());

        let mut emu_data = EmulatorInstanceData::new(cfg, EngineType::Qemu, EngineState::New);
        emu_data.set_emulator_binary(program_name.into());
        let test_engine: QemuEngine =
            QemuEngine::new(emu_data, EmulatorInstances::new(PathBuf::new()));
        let cmd = test_engine.build_emulator_cmd();
        assert_eq!(cmd.get_program(), program_name);
        assert_eq!(cmd.get_envs().collect::<Vec<_>>(), []);
    }

    #[fuchsia::test]
    async fn test_build_cmd_with_custom_template() {
        let env = ffx_config::test_init().await.expect("test env");
        make_fake_sdk(&env).await;
        let temp = tempdir().expect("cannot get tempdir");
        let emu_instances = EmulatorInstances::new(temp.path().to_owned());

        let mut cfg = EmulatorConfiguration::default();
        let template_file = temp.path().join("custom-template.json");
        fs::write(
            &template_file,
            r#"
         {
         "args": [
             "-kernel",
             "boot-shim.bin",
             "-initrd",
             "test.zbi"
         ],
         "envs": {},
         "features": [],
         "kernel_args": ["zircon.nodename=some-emu","TERM=dumb"],
         "options": []
         }"#,
        )
        .expect("custom template contents");
        cfg.runtime.template = Some(template_file);
        cfg.runtime.config_override = true;
        let engine = EngineBuilder::new(emu_instances.clone())
            .config(cfg.clone())
            .engine_type(EngineType::Qemu)
            .build()
            .await
            .expect("engine built");

        let cmd = engine.build_emulator_cmd();
        let actual: Vec<_> = cmd.get_args().collect();

        let expected = vec![
            "-kernel",
            "boot-shim.bin",
            "-initrd",
            "test.zbi",
            "-append",
            "TERM=dumb zircon.nodename=some-emu",
        ];

        assert_eq!(actual, expected)
    }
}
