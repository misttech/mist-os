// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::overnet_connector::{
    OvernetConnection, OvernetConnectionError, OvernetConnector, BUFFER_SIZE,
};
use anyhow::Result;
use ffx_config::EnvironmentContext;
use ffx_ssh::ssh::{build_ssh_command_with_env, SshError};
use fuchsia_async::Task;
use nix::sys::signal::{kill, Signal::SIGKILL};
use nix::sys::wait::waitpid;
use nix::unistd::Pid;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

impl From<SshError> for OvernetConnectionError {
    fn from(ssh_err: SshError) -> Self {
        use SshError::*;
        match &ssh_err {
            Unknown(_)
            | Timeout
            | ConnectionRefused
            | UnknownNameOrService
            | KeyVerificationFailure
            | NoRouteToHost
            | NetworkUnreachable
            | InvalidArgument
            | TargetIncompatible => OvernetConnectionError::NonFatal(ssh_err.into()),
            PermissionDenied => OvernetConnectionError::Fatal(ssh_err.into()),
        }
    }
}

#[derive(Debug)]
pub struct SshConnector {
    pub(crate) cmd: Option<Child>,
    target: SocketAddr,
    env_context: EnvironmentContext,
}

impl SshConnector {
    pub async fn new(target: SocketAddr, env_context: &EnvironmentContext) -> Result<Self> {
        Ok(Self { cmd: None, target, env_context: env_context.clone() })
    }
}

async fn start_ssh_command(target: SocketAddr, env_context: &EnvironmentContext) -> Result<Child> {
    let rev: u64 = version_history::HISTORY.get_misleading_version_for_ffx().abi_revision.as_u64();
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
    Ok(ssh_cmd.spawn()?)
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

impl OvernetConnector for SshConnector {
    async fn connect(&mut self) -> Result<OvernetConnection, OvernetConnectionError> {
        self.cmd = Some(start_ssh_command(self.target, &self.env_context).await?);
        let cmd = self.cmd.as_mut().unwrap();
        let mut stdout = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stdout.take().expect("process should have stdout"),
        );
        let mut stderr = BufReader::with_capacity(
            BUFFER_SIZE,
            cmd.stderr.take().expect("process should have stderr"),
        );
        let (_addr, compat) =
            // This function returns a PipeError on error, which is then ignored and followed by an
            // extraction of a actual SSH error parsing. All PipeError's require terminating the
            // ssh command.
            match ffx_ssh::parse::parse_ssh_output(&mut stdout, &mut stderr, false).await {
                Ok(res) => res,
                Err(e) => {
                    tracing::warn!("SSH pipe error encountered {e:?}");
                    try_ssh_cmd_cleanup(
                        self.cmd.take().expect("ssh command must have started")
                    )
                    .await?;
                    // TODO(b/341372082): This is _not_ logging to a file to avoid using the global env context.
                    // This function should determine which file to log to from the env context.
                    return Err(ffx_ssh::ssh::extract_ssh_error(&mut stderr, false).await.into());
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
        })
    }
}

impl Drop for SshConnector {
    fn drop(&mut self) {
        if let Some(mut cmd) = self.cmd.take() {
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
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = PermissionDenied;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::Fatal(_)));
        let err = ConnectionRefused;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = UnknownNameOrService;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = KeyVerificationFailure;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = NoRouteToHost;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = NetworkUnreachable;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = InvalidArgument;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = TargetIncompatible;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
        let err = Timeout;
        assert!(matches!(OvernetConnectionError::from(err), OvernetConnectionError::NonFatal(_)));
    }
}
