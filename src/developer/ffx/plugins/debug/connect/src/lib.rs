// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use errors::ffx_bail;
use ffx_debug_connect_args::ConnectCommand;
use ffx_zxdb::{forward_to_agent, Debugger};
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_debugger::DebugAgentProxy;
use signal_hook::consts::signal::SIGINT;
use std::{
    process::Command,
    sync::{atomic::AtomicBool, Arc},
};

pub use ffx_zxdb::debug_agent::DebugAgentSocket;

#[derive(FfxTool)]
pub struct ConnectTool {
    #[command]
    cmd: ConnectCommand,
    #[with(moniker("/core/debug_agent"))]
    debugger_proxy: fidl_fuchsia_debugger::DebugAgentProxy,
}

fho::embedded_plugin!(ConnectTool);

#[async_trait(?Send)]
impl FfxMain for ConnectTool {
    type Writer = SimpleWriter;

    async fn main(self, mut _writer: Self::Writer) -> fho::Result<()> {
        connect_tool_impl(self.cmd, self.debugger_proxy).await?;
        Ok(())
    }
}

async fn connect_tool_impl(cmd: ConnectCommand, debugger_proxy: DebugAgentProxy) -> Result<()> {
    let socket = DebugAgentSocket::create(debugger_proxy)?;

    if cmd.agent_only {
        println!("{}", socket.unix_socket_path().display());
        forward_to_agent(socket).await?;
        return Ok(());
    }

    let mut debugger = Debugger::from_socket(socket).await?;

    if cmd.no_auto_attach_limbo {
        debugger.command.push_str("--no-auto-attach-limbo");
    }

    debugger.command.attach_each(&cmd.attach);
    debugger.command.execute_each(&cmd.execute);
    debugger.command.extend(&cmd.zxdb_args);

    let command = match cmd.debugger {
        Some(debugger_debugger) => {
            let sdk = ffx_config::global_env_context()
                .context("loading global environment context")?
                .get_sdk()
                .await?;
            if *sdk.get_version() != sdk::SdkVersion::InTree {
                // OOT doesn't provide symbols for zxdb.
                ffx_bail!("--debugger only works in-tree.");
            }
            let debugger_arg = if debugger_debugger == "lldb" {
                "--"
            } else {
                ffx_bail!("--debugger must be lldb");
            };
            // Ignore SIGINT because Ctrl-C is used to interrupt zxdb and return to the debugger.
            signal_hook::flag::register(SIGINT, Arc::new(AtomicBool::new(false)))?;
            let mut command = Command::new(debugger_debugger);
            command
                .current_dir(sdk.get_path_prefix())
                .arg(debugger_arg)
                .arg(debugger.path())
                .args(debugger.command.args());

            command
        }
        None => debugger.command.build(),
    };

    debugger.run_with_command(command).await
}
