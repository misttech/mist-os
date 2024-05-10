// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::overnet_connector::{OvernetConnection, OvernetConnector, BUFFER_SIZE};
use anyhow::Result;
use ffx_config::EnvironmentContext;
use ffx_ssh::ssh::build_ssh_command_with_env;
use fuchsia_async::Task;
use nix::sys::signal::{kill, Signal::SIGKILL};
use nix::sys::wait::waitpid;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

#[derive(Debug)]
pub struct SshConnector {
    pub(crate) cmd: Child,
}

impl SshConnector {
    pub async fn new(target: SocketAddr, env_context: &EnvironmentContext) -> Result<Self> {
        let rev: u64 =
            version_history::HISTORY.get_misleading_version_for_ffx().abi_revision.as_u64();
        let abi_revision = format!("{}", rev);
        // Converting milliseconds since unix epoch should have enough bits for u64. As of writing
        // it takes up 43 of the 128 bits to represent the number.
        let circuit_id =
            SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_millis() as u64;
        let circuit_id_str = format!("{}", circuit_id);
        let args = vec![
            "remote_control_runner",
            "--circuit",
            &circuit_id_str,
            "--abi-revision",
            &abi_revision,
        ];
        // Use ssh from the environment.
        let ssh_path = "ssh";
        let mut ssh = tokio::process::Command::from(
            build_ssh_command_with_env(ssh_path, target, env_context, args).await?,
        );
        tracing::debug!("SshConnector: invoking {ssh:?}");
        let ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());
        let ssh = ssh_cmd.spawn()?;
        Ok(Self { cmd: ssh })
    }

    async fn connect_impl(&mut self) -> Result<OvernetConnection> {
        let mut stdout = BufReader::with_capacity(
            BUFFER_SIZE,
            self.cmd.stdout.take().expect("process should have stdout"),
        );
        let mut stderr = BufReader::with_capacity(
            BUFFER_SIZE,
            self.cmd.stderr.take().expect("process should have stderr"),
        );
        let (_addr, compat) =
            ffx_ssh::parse::parse_ssh_output(&mut stdout, &mut stderr, false).await?;
        let stdin = self.cmd.stdin.take().expect("process should have stdin");
        let mut stderr = BufReader::new(stderr).lines();
        let (error_sender, errors_receiver) = async_channel::unbounded();
        let stderr_reader = async move {
            while let Ok(Some(line)) = stderr.next_line().await {
                match error_sender.send(anyhow::anyhow!("SSH stderr: {line}")).await {
                    Err(_e) => break,
                    Ok(_) => {}
                }
            }
        };
        let main_task = Some(Task::local(stderr_reader));
        Ok(OvernetConnection {
            output: Box::new(stdout),
            input: Box::new(stdin),
            errors: errors_receiver,
            compat,
            main_task,
        })
    }
}

impl OvernetConnector for SshConnector {
    fn connect(&mut self) -> impl Future<Output = Result<OvernetConnection>> {
        self.connect_impl()
    }
}

impl Drop for SshConnector {
    fn drop(&mut self) {
        let pid = Pid::from_raw(self.cmd.id().unwrap() as i32);
        match self.cmd.try_wait() {
            Ok(Some(result)) => {
                tracing::info!("FidlPipe exited with {}", result);
            }
            Ok(None) => {
                let _ = kill(pid, SIGKILL)
                    .map_err(|e| tracing::warn!("failed to kill FidlPipe command: {:?}", e));
                let _ = waitpid(pid, None)
                    .map_err(|e| tracing::warn!("failed to clean up FidlPipe command: {:?}", e));
            }
            Err(e) => {
                tracing::warn!("failed to soft-wait FidlPipe command: {:?}", e);
                let _ = kill(pid, SIGKILL)
                    .map_err(|e| tracing::warn!("failed to kill FidlPipe command: {:?}", e));
                let _ = waitpid(pid, None)
                    .map_err(|e| tracing::warn!("failed to clean up FidlPipe command: {:?}", e));
            }
        };
    }
}
