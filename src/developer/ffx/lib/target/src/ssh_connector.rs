// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::target_connector::{
    FDomainConnection, OvernetConnection, TargetConnection, TargetConnectionError, TargetConnector,
    BUFFER_SIZE,
};
use anyhow::Result;
use ffx_command_error::FfxContext as _;
use ffx_config::EnvironmentContext;
use ffx_ssh::ssh::{build_ssh_command_with_env, SshError};
use fuchsia_async::Task;
use nix::sys::signal::kill;
use nix::sys::signal::Signal::SIGKILL;
use nix::sys::wait::waitpid;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader, ErrorKind};
use tokio::process::Child;

impl From<SshError> for TargetConnectionError {
    fn from(ssh_err: SshError) -> Self {
        use SshError::*;
        match &ssh_err {
            // These errors are considered potentially recoverable, as they can often surface when
            // a device is actively rebooting while trying to reconnect to it.
            Unknown(_) | Timeout | ConnectionRefused | UnknownNameOrService | NoRouteToHost
            | NetworkUnreachable => TargetConnectionError::NonFatal(ssh_err.into()),
            // These errors are unrecoverable, as they are fundamental errors in an existing
            // configuration.
            PermissionDenied
            | KeyVerificationFailure
            | InvalidArgument
            | TargetIncompatible
            | ConnectionClosedByRemoteHost => TargetConnectionError::Fatal(ssh_err.into()),
        }
    }
}

enum FDomainConnectionError {
    ConnectionError(TargetConnectionError),
    NotSupported,
}

#[derive(Debug)]
pub struct SshConnector {
    pub(crate) overnet_cmd: Option<Child>,
    target: SocketAddr,
    env_context: EnvironmentContext,
}

impl SshConnector {
    pub async fn new(target: SocketAddr, env_context: &EnvironmentContext) -> Result<Self> {
        Ok(Self { overnet_cmd: None, target, env_context: env_context.clone() })
    }
}

impl SshConnector {
    async fn connect_overnet(&mut self) -> Result<OvernetConnection, TargetConnectionError> {
        self.overnet_cmd = Some(start_overnet_ssh_command(self.target, &self.env_context).await?);
        let cmd = self.overnet_cmd.as_mut().unwrap();
        let mut stdout = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stdout.take().expect("process should have stdout"),
        );
        let mut stderr = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stderr.take().expect("process should have stderr"),
        );
        let (addr, compat) =
            // This function returns a PipeError on error, which necessitates terminating the SSH
            // command. This error must be converted into an `SshError` in order to be presentable
            // to the user.
            match ffx_ssh::parse::parse_ssh_output(&mut stdout, &mut stderr, false, &self.env_context).await {
                Ok(res) => res,
                Err(e) => {
                    tracing::warn!("SSH pipe error encountered {e:?}");
                    try_ssh_cmd_cleanup(
                        self.overnet_cmd.take().expect("ssh command must have started")
                    )
                    .await?;
                    return Err(ffx_ssh::ssh::SshError::from(e.to_string()).into());
                }
            };
        let stdin = cmd.stdin.take().expect("process should have stdin");
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
            ssh_host_address: Some(addr),
        })
    }

    async fn connect_fdomain(&mut self) -> Result<FDomainConnection, FDomainConnectionError> {
        self.overnet_cmd = Some(
            start_fdomain_ssh_command(self.target, &self.env_context)
                .await
                .map_err(|x| FDomainConnectionError::ConnectionError(x.into()))?,
        );
        let cmd = self.overnet_cmd.as_mut().unwrap();
        let mut stdout = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stdout.take().expect("process should have stdout"),
        );
        let stderr = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stderr.take().expect("process should have stderr"),
        );
        let mut ack = [0u8; 3];
        match stdout.read_exact(&mut ack).await {
            Ok(_) => (),
            Err(e) => {
                if e.kind() == ErrorKind::UnexpectedEof {
                    let mut lines = stderr.lines();
                    if let Ok(Some(line)) = lines.next_line().await {
                        if line.contains("fdomain_runner: not found") {
                            return Err(FDomainConnectionError::NotSupported);
                        }
                    }
                }
                return Err(FDomainConnectionError::ConnectionError(
                    TargetConnectionError::NonFatal(e.into()),
                ));
            }
        }

        if ack != *b"OK\n" {
            return Err(FDomainConnectionError::ConnectionError(
                ffx_ssh::ssh::SshError::Unknown(format!("Unknown Ack string {ack:?}")).into(),
            ));
        }
        let stdin = cmd.stdin.take().expect("process should have stdin");
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
        Ok(FDomainConnection {
            output: Box::new(stdout),
            input: Box::new(stdin),
            errors: errors_receiver,
            main_task,
        })
    }
}

async fn start_fdomain_ssh_command(
    target: SocketAddr,
    env_context: &EnvironmentContext,
) -> Result<Child> {
    let args = vec!["fdomain_runner"];
    // Use ssh from the environment.
    let ssh_path = "ssh";
    let mut ssh = tokio::process::Command::from(
        build_ssh_command_with_env(ssh_path, target, env_context, args).await?,
    );
    tracing::debug!("SshConnector: invoking {ssh:?}");
    let ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());
    Ok(ssh_cmd.spawn().bug_context("spawning ssh command")?)
}

async fn start_overnet_ssh_command(
    target: SocketAddr,
    env_context: &EnvironmentContext,
) -> Result<Child> {
    let rev: u64 =
        version_history_data::HISTORY.get_misleading_version_for_ffx().abi_revision.as_u64();
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
    Ok(ssh_cmd.spawn().bug_context("spawning ssh command")?)
}

async fn try_ssh_cmd_cleanup(mut cmd: Child) -> Result<()> {
    cmd.kill().await?;
    if let Some(status) = cmd.try_wait()? {
        match status.code() {
            // Possible to catch more error codes here, hence the use of a match.
            Some(255) => {
                tracing::warn!("SSH ret code: 255. Unexpected session termination.")
            }
            _ => tracing::error!("SSH exited with error code: {status}. "),
        }
    } else {
        tracing::error!("ssh child has not ended, trying one more time then ignoring it.");
        fuchsia_async::Timer::new(std::time::Duration::from_secs(2)).await;
        tracing::error!("ssh child status is {:?}", cmd.try_wait());
    }
    Ok(())
}

/// This config value must be set to true to use FDomain as a remoting protocol.
const FDOMAIN_CONFIG_KEY: &str = "ssh.allow_fdomain";

impl TargetConnector for SshConnector {
    const CONNECTION_TYPE: &'static str = "ssh";

    async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
        let allow_fdomain = self
            .env_context
            .get(FDOMAIN_CONFIG_KEY)
            .unwrap_or_else(|_| self.env_context.is_strict());
        let fdomain = if allow_fdomain {
            match self.connect_fdomain().await {
                Ok(f) => Some(f),
                Err(FDomainConnectionError::NotSupported) => None,
                Err(FDomainConnectionError::ConnectionError(other)) => {
                    // Eventually we should just return the error here, making
                    // FDomain authoritative about whether the device is
                    // connectable. For now we'll fall through because it's less
                    // likely to cause breakages prior to migration.
                    tracing::warn!("Connecting with FDomain encountered error {other:?}");
                    None
                }
            }
        } else {
            None
        };
        let overnet = self.connect_overnet().await;

        if let Some(fdomain) = fdomain {
            if let Some(overnet) = overnet.ok() {
                Ok(TargetConnection::Both(fdomain, overnet))
            } else {
                Ok(TargetConnection::FDomain(fdomain))
            }
        } else {
            overnet.map(TargetConnection::Overnet)
        }
    }

    fn device_address(&self) -> Option<SocketAddr> {
        Some(self.target.clone())
    }
}

impl Drop for SshConnector {
    fn drop(&mut self) {
        if let Some(mut cmd) = self.overnet_cmd.take() {
            let pid = Pid::from_raw(cmd.id().unwrap() as i32);
            match cmd.try_wait() {
                Ok(Some(result)) => {
                    tracing::info!("FidlPipe exited with {}", result);
                }
                Ok(None) => {
                    let _ = kill(pid, SIGKILL)
                        .map_err(|e| tracing::warn!("failed to kill FidlPipe command: {:?}", e));
                    let _ = waitpid(pid, None).map_err(|e| {
                        tracing::warn!("failed to clean up FidlPipe command: {:?}", e)
                    });
                }
                Err(e) => {
                    tracing::warn!("failed to soft-wait FidlPipe command: {:?}", e);
                    let _ = kill(pid, SIGKILL)
                        .map_err(|e| tracing::warn!("failed to kill FidlPipe command: {:?}", e));
                    let _ = waitpid(pid, None).map_err(|e| {
                        tracing::warn!("failed to clean up FidlPipe command: {:?}", e)
                    });
                }
            };
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_ssh_error_conversion() {
        use SshError::*;
        let err = Unknown("foobar".to_string());
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = PermissionDenied;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::Fatal(_)));
        let err = ConnectionRefused;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = UnknownNameOrService;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = KeyVerificationFailure;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::Fatal(_)));
        let err = NoRouteToHost;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = NetworkUnreachable;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = InvalidArgument;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::Fatal(_)));
        let err = TargetIncompatible;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::Fatal(_)));
        let err = Timeout;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::NonFatal(_)));
        let err = ConnectionClosedByRemoteHost;
        assert!(matches!(TargetConnectionError::from(err), TargetConnectionError::Fatal(_)));
    }
}
