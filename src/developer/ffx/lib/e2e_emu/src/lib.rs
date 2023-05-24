// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Context as _};
use diagnostics_data::LogsData;
use ffx_config::TestEnv;
use ffx_isolate::Isolate;
use serde::Deserialize;
use tempfile::TempDir;
use tracing::info;

const PRODUCT_BUNDLE_PATH: &str = env!("PRODUCT_BUNDLE_PATH");

/// An isolated environment for testing ffx against a running emulator.
pub struct IsolatedEmulator {
    emu_name: String,
    ffx_isolate: Isolate,
    system_logs_child: Option<std::process::Child>,

    // We need to hold the below variables but not interact with them.
    _temp_dir: TempDir,
    _test_env: TestEnv,
}

impl IsolatedEmulator {
    /// Create an isolated ffx environment and start an emulator in it using the default product
    /// bundle and package repository from the Fuchsia build directory.
    pub async fn start(name: &str) -> anyhow::Result<Self> {
        let emu_name = format!("{name}-emu");

        info!(%name, "making ffx isolate");
        let temp_dir = tempfile::TempDir::new().context("making temp dir")?;
        let test_env = ffx_config::test_init().await.context("setting up ffx test config")?;

        // Create paths to the files to hold the ssh key pair.
        // The key is not generated here, since ffx will generate the
        // key if it is missing when starting an emulator or flashing a device.
        // If a  private key is supplied, it is used, but the public key path
        // is still in the temp dir.
        let ssh_priv_key = temp_dir.path().join("ssh_private_key");
        let ssh_pub_key = temp_dir.path().join("ssh_public_key");

        let ffx_isolate = Isolate::new_in_test(name, ssh_priv_key.clone(), &test_env.context)
            .await
            .context("creating ffx isolate")?;

        let mut this = Self {
            emu_name,
            ffx_isolate,
            _temp_dir: temp_dir,
            _test_env: test_env,
            system_logs_child: None,
        };

        // now we have our isolate and can call ffx commands to configure our env and start an emu
        this.ffx(&["config", "set", "ssh.priv", &ssh_priv_key.to_string_lossy()])
            .await
            .context("setting ssh private key config")?;
        this.ffx(&["config", "set", "ssh.pub", &ssh_pub_key.to_string_lossy()])
            .await
            .context("setting ssh public key config")?;
        this.ffx(&["config", "set", "log.level", "debug"])
            .await
            .context("setting ffx log level")?;

        info!("starting emulator {}", this.emu_name);
        let emulator_log = this.ffx_isolate.log_dir().join("emulator.log").display().to_string();
        this.ffx(&[
            "emu",
            "start",
            "--headless",
            "--net",
            "user",
            "--name",
            &this.emu_name,
            "--log",
            &*emulator_log,
            PRODUCT_BUNDLE_PATH,
        ])
        .await
        .context("starting emulator")?;

        info!("streaming system logs to output directory");
        let mut system_logs_command = this
            .ffx_isolate
            .ffx_cmd(&this.make_args(&["log", "--severity", "TRACE", "--no-color"]))
            .await
            .context("creating log streaming command")?;

        let emulator_system_log =
            std::fs::File::create(this.ffx_isolate.log_dir().join("system.log"))
                .context("creating system log file")?;
        system_logs_command.stdout(emulator_system_log);

        // ffx log prints lots of warnings about symbolization
        system_logs_command.stderr(std::process::Stdio::null());

        this.system_logs_child =
            Some(system_logs_command.spawn().context("spawning log streaming command")?);

        Ok(this)
    }

    fn make_args<'a>(&'a self, args: &[&'a str]) -> Vec<&str> {
        let mut prefixed = vec!["--target", &self.emu_name];
        prefixed.extend(args);
        prefixed
    }

    /// Run an ffx command, logging stdout & stderr as INFO messages.
    pub async fn ffx(&self, args: &[&str]) -> anyhow::Result<()> {
        info!("running `ffx {args:?}`");
        let output = self.ffx_isolate.ffx(&self.make_args(args)).await.context("running ffx")?;
        if !output.stdout.is_empty() {
            info!("stdout:\n{}", output.stdout);
        }
        if !output.stderr.is_empty() {
            info!("stderr:\n{}", output.stderr);
        }
        ensure!(output.status.success(), "ffx must complete successfully");
        Ok(())
    }

    /// Run an ffx command, returning stdout and logging stderr as an INFO message.
    pub async fn ffx_output(&self, args: &[&str]) -> anyhow::Result<String> {
        info!("running `ffx {args:?}`");
        let output = self.ffx_isolate.ffx(&self.make_args(args)).await.context("running ffx")?;
        if !output.stderr.is_empty() {
            info!("stderr:\n{}", output.stderr);
        }
        ensure!(output.status.success(), "ffx must complete successfully");
        Ok(output.stdout)
    }

    fn make_ssh_args<'a>(command: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["target", "ssh", "--"];
        args.extend(command);
        args
    }

    /// Run an ssh command, logging stdout & stderr as INFO messages.
    pub async fn ssh(&self, command: &[&str]) -> anyhow::Result<()> {
        self.ffx(&Self::make_ssh_args(command)).await
    }

    /// Run an ssh command, returning stdout and logging stderr as an INFO message.
    pub async fn ssh_output(&self, command: &[&str]) -> anyhow::Result<String> {
        self.ffx_output(&Self::make_ssh_args(command)).await
    }

    /// Collect the logs for a particular component.
    pub async fn logs_for_moniker(&self, moniker: &str) -> anyhow::Result<Vec<LogsData>> {
        /// ffx log wraps each line from archivist in its own JSON object, unwrap those here
        #[derive(Deserialize)]
        struct FfxMachineLogLine {
            data: FfxTargetLog,
        }
        #[derive(Deserialize)]
        struct FfxTargetLog {
            #[serde(rename = "TargetLog")]
            target_log: LogsData,
        }

        let output = self
            .ffx_output(&["--machine", "json", "log", "--moniker", moniker, "dump"])
            .await
            .context("running ffx log")?;

        let mut parsed = vec![];
        for line in output.lines() {
            if line.is_empty() {
                continue;
            }
            let ffx_message = serde_json::from_str::<FfxMachineLogLine>(line)
                .context("parsing log line from ffx")?;
            parsed.push(ffx_message.data.target_log);
        }
        Ok(parsed)
    }
}

impl Drop for IsolatedEmulator {
    fn drop(&mut self) {
        if let Some(child) = self.system_logs_child.as_mut() {
            // stream a few logs out before we exit
            std::thread::sleep(std::time::Duration::from_secs(1));
            child.kill().ok();
        }

        info!(
            "Tearing down isolated emulator instance. Logs are in {}.",
            self.ffx_isolate.log_dir().display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn get_monotonic_clock() {
        let emu = IsolatedEmulator::start("get_clock").await.unwrap();
        let time = emu.ffx_output(&["target", "get-time"]).await.unwrap();
        time.trim().parse::<u64>().expect("should have gotten a timestamp back");
    }

    #[fuchsia::test]
    async fn system_logs_are_streamed() {
        let emu = IsolatedEmulator::start("system_logs").await.unwrap();

        let system_log_path = emu.ffx_isolate.log_dir().join("system.log");
        loop {
            let contents = std::fs::read_to_string(&system_log_path).unwrap();
            if !contents.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    #[fuchsia::test]
    async fn get_remote_control_logs() {
        let emu = IsolatedEmulator::start("rcs_logs").await.unwrap();
        loop {
            let remote_control_logs = emu.logs_for_moniker("/core/remote-control").await.unwrap();
            if !remote_control_logs.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
