// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{ensure, Context as _};
use async_stream::stream;
use diagnostics_data::LogsData;
use ffx_config::TestEnv;
use ffx_isolate::Isolate;
use futures::{channel::mpsc::TrySendError, Stream, StreamExt};
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};
use tempfile::TempDir;
use tracing::info;

const AMBER_FILES_PATH: &str = env!("PACKAGE_REPOSITORY_PATH");
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
    /// bundle and package repository from the Fuchsia build directory. Streams logs in the
    /// background and allows resolving packages from universe.
    pub async fn start(name: &str) -> anyhow::Result<Self> {
        Self::start_internal(name, Some(AMBER_FILES_PATH)).await
    }

    // This is private to be used for testing with a path to a different package repo. Path
    // to amber-files is optional for testing to ensure that other successful tests are actually
    // matching a developer workflow.
    async fn start_internal(name: &str, amber_files_path: Option<&str>) -> anyhow::Result<Self> {
        let emu_name = format!("{name}-emu");

        info!(%name, "making ffx isolate");
        let temp_dir = tempfile::TempDir::new().context("making temp dir")?;
        let test_env = ffx_config::test_init().await.context("setting up ffx test config")?;

        // Create paths to the files to hold the ssh key pair.
        // The key is not generated here, since ffx will generate the
        // key if it is missing when starting an emulator or flashing a device.
        // If a private key is supplied, it is used, but the public key path
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

        this.ffx_isolate.start_daemon().await?;

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

        // serve packages by creating a repository and a server, then registering the server
        if let Some(amber_files_path) = amber_files_path {
            this.ffx(&["repository", "add-from-pm", &amber_files_path])
                .await
                .context("adding repository from build dir")?;
            this.ffx(&[
                "repository",
                "server",
                "start",
                // ask the kernel to give us a random unused port
                "--address",
                "[::]:0",
            ])
            .await
            .context("starting repository server")?;

            this.ffx(&["target", "repository", "register", "--alias", "fuchsia.com"])
                .await
                .context("registering repository")?;
        }

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

    /// Create an ffx command, which allows for streaming stdout/stderr.
    pub async fn ffx_cmd_capture(&self, args: &[&str]) -> anyhow::Result<Command> {
        let mut cmd =
            self.ffx_isolate.ffx_cmd(&self.make_args(args)).await.context("running ffx")?;
        cmd.stdout(Stdio::piped());
        Ok(cmd)
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

    async fn log_stream(
        &self,
        mut receiver: futures::channel::mpsc::UnboundedReceiver<String>,
        reader_task: fuchsia_async::Task<Result<(), TrySendError<String>>>,
    ) -> impl Stream<Item = anyhow::Result<LogsData>> {
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

        stream! {
            while let Some(line) = receiver.next().await {
                if line.is_empty() {
                    continue;
                }
                let ffx_message = serde_json::from_str::<FfxMachineLogLine>(&line)
                    .context("parsing log line from ffx")?;
                yield Ok(ffx_message.data.target_log);
            }
            drop(reader_task)
        }
    }

    /// Collect the logs for a particular component.
    pub async fn log_stream_for_moniker(
        &self,
        moniker: &str,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<LogsData>>> {
        let mut output = self
            .ffx_cmd_capture(&["--machine", "json", "log", "--moniker", moniker])
            .await
            .context("running ffx log")?;

        let child = output.spawn()?;
        let stdout = child.stdout.context("no stdout")?;
        let mut reader = BufReader::new(stdout);
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let reader_task = fuchsia_async::Task::local(fuchsia_async::unblock(move || {
            let mut output = String::new();
            while let Ok(_) = reader.read_line(&mut output) {
                sender.unbounded_send(output)?;
                output = String::new();
            }
            Result::<(), TrySendError<String>>::Ok(())
        }));
        Ok(self.log_stream(receiver, reader_task).await)
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
    use std::pin::pin;

    #[fuchsia::test]
    async fn public_apis_succeed() {
        let emu = IsolatedEmulator::start("e2e_emu_public_apis").await.unwrap();

        info!("Checking target monotonic time to ensure we can connect and get stdout");
        let time = emu.ffx_output(&["target", "get-time"]).await.unwrap();
        time.trim().parse::<u64>().expect("should have gotten a timestamp back");

        info!("Checking that the emulator instance writes a system log.");
        let system_log_path = emu.ffx_isolate.log_dir().join("system.log");
        loop {
            let contents = std::fs::read_to_string(&system_log_path).unwrap();
            if !contents.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        info!("Checking that we can read streaming logs.");
        let mut remote_control_logs =
            pin!(emu.log_stream_for_moniker("/core/remote-control").await.unwrap());
        remote_control_logs.next().await.unwrap().unwrap();

        info!("Checking that we can read RCS' logs.");
        let remote_control_logs = emu.logs_for_moniker("/core/remote-control").await.unwrap();
        assert_eq!(remote_control_logs.is_empty(), false);
    }

    const TEST_PACKAGE_URL: &str = concat!("fuchsia-pkg://fuchsia.com/", env!("TEST_PACKAGE_NAME"));

    #[fuchsia::test]
    async fn resolve_package_from_server() {
        let emu = IsolatedEmulator::start_internal(
            "pkg_resolve",
            Some(env!("TEST_PACKAGE_REPOSITORY_PATH")),
        )
        .await
        .unwrap();
        emu.ssh(&["pkgctl", "resolve", TEST_PACKAGE_URL]).await.unwrap();
    }

    /// This ensures the above test is actually resolving the package from the package server by
    /// demonstrating that the same package is unavailable when there's no server running.
    #[fuchsia::test]
    async fn fail_to_resolve_package_when_no_package_server_running() {
        let emu = IsolatedEmulator::start_internal("pkg_resolve_fail", None).await.unwrap();
        emu.ssh(&["pkgctl", "resolve", TEST_PACKAGE_URL]).await.unwrap_err();
    }
}
