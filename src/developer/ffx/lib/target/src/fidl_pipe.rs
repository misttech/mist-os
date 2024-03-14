// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_channel::Receiver;
use compat_info::CompatibilityInfo;
use ffx_config::EnvironmentContext;
use ffx_ssh::ssh::build_ssh_command_with_env;
use fuchsia_async::Task;
use futures::FutureExt;
use futures_lite::stream::StreamExt;
use nix::{
    sys::{
        signal::{kill, Signal::SIGKILL},
        wait::waitpid,
    },
    unistd::Pid,
};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

const BUFFER_SIZE: usize = 65536;

/// Represents a FIDL-piping connection to a Fuchsia target.
///
/// To connect a FIDL pipe, the user must know the IP address of the Fuchsia target being connected
/// to. This may or may not include a port number.
pub struct FidlPipe {
    process: Child,
    address: SocketAddr,
    task: Option<Task<()>>,
    error_queue: Receiver<Option<anyhow::Error>>,
    circuit_id: u64,
    compat: Option<CompatibilityInfo>,
}

#[derive(thiserror::Error, Debug)]
enum FidlPipeError {
    #[error("running target fidl pipe: {0}")]
    SpawnError(String),
    #[error("internal error running pipe cmd {1}: {0}")]
    InternalError(String, String),
}

/// creates the socket for overnet. IoError is possible from socket operations.
pub fn overnet_pipe(node: Arc<overnet_core::Router>) -> Result<fidl::AsyncSocket, io::Error> {
    let (local_socket, remote_socket) = fidl::Socket::create_stream();
    let local_socket = fidl::AsyncSocket::from_socket(local_socket);
    let (errors_sender, errors) = futures::channel::mpsc::unbounded();
    Task::spawn(
        futures::future::join(
            async move {
                if let Err(e) = async move {
                    let (mut rx, mut tx) = futures::AsyncReadExt::split(
                        fuchsia_async::Socket::from_socket(remote_socket),
                    );
                    circuit::multi_stream::multi_stream_node_connection_to_async(
                        node.circuit_node(),
                        &mut rx,
                        &mut tx,
                        false,
                        circuit::Quality::LOCAL_SOCKET,
                        errors_sender,
                        "remote_control_runner".to_owned(),
                    )
                    .await?;
                    Result::<(), anyhow::Error>::Ok(())
                }
                .await
                {
                    tracing::warn!("FIDL pipe circuit closed: {:?}", e);
                }
            },
            errors
                .map(|e| {
                    tracing::warn!("A FIDL pipe circuit stream failed: {e:?}");
                })
                .collect::<()>(),
        )
        .map(|((), ())| ()),
    )
    .detach();

    Ok(local_socket)
}

type PipeFut<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

struct PipeTasks<'a> {
    pipe_rx: PipeFut<'a>,
    pipe_tx: PipeFut<'a>,
    pipe_err: PipeFut<'a>,
    compat: Option<CompatibilityInfo>,
}

async fn create_pipe_tasks<'a>(
    cmd: &mut Child,
    node: Arc<overnet_core::Router>,
    error_sender: async_channel::Sender<Option<anyhow::Error>>,
) -> Result<PipeTasks<'a>> {
    let (pipe_rx, mut pipe_tx) = tokio::io::split(overnet_pipe(node).map_err(|e| {
        FidlPipeError::InternalError(
            format!("Unable to create overnet_pipe from {e:?}"),
            format!("{cmd:?}"),
        )
    })?);
    let mut stdout = BufReader::with_capacity(
        BUFFER_SIZE,
        cmd.stdout.take().expect("process should have stdout"),
    );
    let mut stderr = BufReader::with_capacity(
        BUFFER_SIZE,
        cmd.stderr.take().expect("process should have stderr"),
    );
    let (_addr, compat) = ffx_ssh::parse::parse_ssh_output(&mut stdout, &mut stderr, false).await?;
    let mut stdin = cmd.stdin.take().expect("process should have stdin");
    let err_clone = error_sender.clone();
    let copy_in = async move {
        if let Err(e) = tokio::io::copy_buf(&mut stdout, &mut pipe_tx).await {
            let _ = err_clone.send(Some(anyhow::anyhow!("SSH stdout read failure: {e:?}"))).await;
        };
    };
    let err_clone = error_sender.clone();
    let copy_out = async move {
        if let Err(e) =
            tokio::io::copy_buf(&mut BufReader::with_capacity(BUFFER_SIZE, pipe_rx), &mut stdin)
                .await
        {
            let _ = err_clone.send(Some(anyhow::anyhow!("SSH stdin write failure: {e:?}"))).await;
        }
    };
    let mut stderr = BufReader::new(stderr).lines();
    let stderr_reader = async move {
        while let Ok(Some(line)) = stderr.next_line().await {
            match error_sender.send(Some(anyhow::anyhow!("SSH stderr: {line}"))).await {
                Err(_e) => break,
                Ok(_) => {}
            }
        }
    };
    Ok(PipeTasks {
        pipe_rx: Box::pin(copy_in),
        pipe_tx: Box::pin(copy_out),
        pipe_err: Box::pin(stderr_reader),
        compat,
    })
}

// TODO(b/327683942): The error handling code needs better test coverage.
impl FidlPipe {
    /// Creates a new FIDL pipe to the Fuchsia target.
    ///
    /// Args:
    ///     env_context: the environment context for which configuration options are selected.
    ///     target: the address to the Fuchsia target.
    ///     overnet_node: The backing overnet node for this connection.
    pub async fn new(
        env_context: EnvironmentContext,
        target: SocketAddr,
        overnet_node: Arc<overnet_core::Router>,
    ) -> Result<Self> {
        Self::run_ssh(target, env_context, overnet_node).await
    }

    pub fn compatibility_info(&self) -> Option<CompatibilityInfo> {
        self.compat.clone()
    }

    pub fn circuit_id(&self) -> u64 {
        self.circuit_id
    }

    async fn run_ssh(
        target: SocketAddr,
        env_context: EnvironmentContext,
        overnet_node: Arc<overnet_core::Router>,
    ) -> Result<Self> {
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
        let (errors_sender, errors_receiver) = async_channel::unbounded();
        // Use ssh from the environment.
        let ssh_path = "ssh";
        let mut ssh = tokio::process::Command::from(
            build_ssh_command_with_env(ssh_path, target, &env_context, args).await?,
        );
        let ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());
        let mut ssh = ssh_cmd.spawn().map_err(|e| FidlPipeError::SpawnError(e.to_string()))?;
        let PipeTasks { pipe_rx, pipe_tx, pipe_err, compat } =
            create_pipe_tasks(&mut ssh, overnet_node, errors_sender.clone()).await?;
        let errors_clone = errors_sender.clone();
        let main_task = async move {
            let copy_fut = futures_lite::future::zip(pipe_rx, pipe_tx);
            let _ = futures_lite::future::zip(copy_fut, pipe_err).await;
            let _ = errors_clone.send(None).await;
        };
        Ok(Self {
            address: target,
            process: ssh,
            task: Some(Task::local(main_task)),
            error_queue: errors_receiver,
            circuit_id,
            compat,
        })
    }

    pub fn error_stream(&self) -> Receiver<Option<anyhow::Error>> {
        return self.error_queue.clone();
    }

    pub fn target_address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for FidlPipe {
    fn drop(&mut self) {
        let pid = Pid::from_raw(self.process.id().unwrap() as i32);
        match self.process.try_wait() {
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
        self.error_queue.close();
        drop(self.task.take());
    }
}
