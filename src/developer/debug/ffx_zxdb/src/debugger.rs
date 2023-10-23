// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use errors::ffx_error;
use fidl_fuchsia_debugger::DebugAgentProxy;
use fuchsia_async::unblock;
use std::{path::PathBuf, process::Command};

use crate::{debug_agent::DebugAgentSocket, spawn_forward_task, CommandBuilder};

pub struct Debugger {
    socket: DebugAgentSocket,
    path: PathBuf,
    pub command: CommandBuilder,
}

impl Debugger {
    /// Create a debugger that uses the given debug agent.
    pub async fn new(debugger_proxy: DebugAgentProxy) -> Result<Self> {
        let socket = DebugAgentSocket::create(debugger_proxy)?;
        Debugger::from_socket(socket).await
    }

    /// Create a debugger from an existing socket connection to a debug agent.
    pub async fn from_socket(socket: DebugAgentSocket) -> Result<Self> {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await?;
        if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk).await {
            eprintln!("ensure_symbol_index_registered failed, error was: {:#?}", e);
        }

        let path = sdk.get_host_tool("zxdb")?;

        let mut command = CommandBuilder::new(path.clone());
        command.connect(&socket);

        Ok(Self { socket, path, command })
    }

    /// The path of the zxdb binary on the local system.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Run the debugger with the given `command` instead of `self.command`.
    pub async fn run_with_command(self, mut command: Command) -> Result<()> {
        let mut child = command.spawn()?;
        let _task = spawn_forward_task(self.socket);

        if let Some(exit_code) = unblock(move || child.wait()).await?.code() {
            if exit_code == 0 {
                Ok(())
            } else {
                Err(ffx_error!("zxdb exited with code {}", exit_code).into())
            }
        } else {
            Err(ffx_error!("zxdb terminated by signal").into())
        }
    }

    /// Run the debugger.
    ///
    /// The future returned by this function resolves when the user's interactive session with the
    /// debugger is complete.
    pub async fn run(self) -> Result<()> {
        let command = self.command.build();
        self.run_with_command(command).await
    }
}
