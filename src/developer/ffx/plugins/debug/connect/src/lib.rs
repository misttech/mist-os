// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_debug_connect_args::ConnectCommand;
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_debugger::DebugAgentProxy;
use fuchsia_async::unblock;
use signal_hook::consts::signal::SIGINT;
use std::{
    process::Command,
    sync::{atomic::AtomicBool, Arc},
};

pub use ffx_zxdb::{
    debug_agent::DebugAgentSocket, forward_to_agent, spawn_forward_task, CommandBuilder,
};

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

    let sdk = ffx_config::global_env_context()
        .context("loading global environment context")?
        .get_sdk()
        .await?;
    if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk).await {
        eprintln!("ensure_symbol_index_registered failed, error was: {:#?}", e);
    }

    let zxdb_path = sdk.get_host_tool("zxdb")?;

    let mut command_builder = CommandBuilder::new(zxdb_path.clone());
    command_builder.connect(&socket);

    if cmd.no_auto_attach_limbo {
        command_builder.push_str("--no-auto-attach-limbo");
    }

    command_builder.attach_each(&cmd.attach);
    command_builder.execute_each(&cmd.execute);
    command_builder.extend(&cmd.zxdb_args);

    let mut zxdb = match cmd.debugger {
        Some(debugger) => {
            if *sdk.get_version() != sdk::SdkVersion::InTree {
                // OOT doesn't provide symbols for zxdb.
                ffx_bail!("--debugger only works in-tree.");
            }
            let debugger_arg = if debugger == "gdb" {
                "--args"
            } else if debugger == "lldb" {
                "--"
            } else {
                ffx_bail!("--debugger must be gdb or lldb");
            };
            // lldb can find .build-id directory automatically but gdb has some trouble.
            // So we supply the unstripped version for them.
            let zxdb_unstripped_path = zxdb_path.parent().unwrap().join("exe.unstripped/zxdb");
            // Ignore SIGINT because Ctrl-C is used to interrupt zxdb and return to the debugger.
            signal_hook::flag::register(SIGINT, Arc::new(AtomicBool::new(false)))?;
            Command::new(debugger)
                .current_dir(sdk.get_path_prefix())
                .arg(debugger_arg)
                .arg(zxdb_unstripped_path)
                .args(command_builder.into_args())
                .spawn()?
        }
        None => command_builder.build().spawn()?,
    };

    // Spawn the task that doing the forwarding in the background.
    spawn_forward_task(socket);

    if let Some(exit_code) = unblock(move || zxdb.wait()).await?.code() {
        if exit_code == 0 {
            Ok(())
        } else {
            Err(ffx_error!("zxdb exited with code {}", exit_code).into())
        }
    } else {
        Err(ffx_error!("zxdb terminated by signal").into())
    }
}
