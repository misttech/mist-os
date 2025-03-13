// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_debug_connect_args::ConnectCommand;
use ffx_writer::SimpleWriter;
use ffx_zxdb::util::{self, Agent};
use ffx_zxdb::{forward_to_agent, Debugger};
use fho::{FfxMain, FfxTool};
use fidl_fuchsia_debugger as fdebugger;
use signal_hook::consts::signal::SIGINT;
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use target_holders::moniker;

pub use ffx_zxdb::debug_agent::{DebugAgentSocket, DebuggerProxy};

#[derive(FfxTool)]
pub struct ConnectTool {
    #[command]
    cmd: ConnectCommand,
    #[with(moniker("/core/debugger"))]
    launcher_proxy: fdebugger::LauncherProxy,
}

fho::embedded_plugin!(ConnectTool);

#[async_trait(?Send)]
impl FfxMain for ConnectTool {
    type Writer = SimpleWriter;

    async fn main(self, mut _writer: Self::Writer) -> fho::Result<()> {
        connect_tool_impl(self.cmd, self.launcher_proxy).await?;
        Ok(())
    }
}

async fn choose_debug_agent(launcher_proxy: &fdebugger::LauncherProxy) -> Result<Option<Agent>> {
    // Get the list of all currently running DebugAgents from the launcher.
    let mut agent_vec = util::get_all_debug_agents(&launcher_proxy).await?;

    if !agent_vec.is_empty() {
        println!("[0] Launch new DebugAgent");

        util::print_debug_agents(&agent_vec);

        println!(
            "Select a number from above to debug the attached process(es), \
             or 0 to start a new debugging session (ctrl-c to cancel)"
        );

        std::io::stdout().flush().unwrap();

        let input = std::io::stdin()
            .lock()
            .lines()
            .next()
            .map(|r| r.ok())
            .flatten()
            .map(|s| s.parse::<usize>().ok())
            .ok_or(ffx_error!("Failed to parse input!"))?
            .filter(|i| *i <= agent_vec.len())
            .ok_or(ffx_error!("Invalid input!"))?;

        return Ok(match input {
            0 => None,
            index => Some(agent_vec.remove(index - 1)),
        });
    }

    Ok(None)
}

async fn connect_tool_impl(
    cmd: ConnectCommand,
    launcher_proxy: fdebugger::LauncherProxy,
) -> Result<()> {
    let socket = if cmd.new_agent {
        DebugAgentSocket::create(DebuggerProxy::LauncherProxy(launcher_proxy))?
    } else {
        choose_debug_agent(&launcher_proxy).await?.map_or_else(
            || DebugAgentSocket::create(DebuggerProxy::LauncherProxy(launcher_proxy)),
            |agent| {
                println!("Connecting to {}", agent.name);
                DebugAgentSocket::create(DebuggerProxy::DebugAgentProxy(agent.debug_agent_proxy))
            },
        )?
    };

    if cmd.agent_only {
        println!("{}", socket.unix_socket_path().display());
        forward_to_agent(socket).await?;
        return Ok(());
    }

    let mut debugger = Debugger::from_socket(socket).await?;

    debugger.command.attach_each(&cmd.attach);
    debugger.command.execute_each(&cmd.execute);
    debugger.command.extend(&cmd.zxdb_args);

    let command = match cmd.debugger {
        Some(debugger_debugger) => {
            let sdk = ffx_config::global_env_context()
                .context("loading global environment context")?
                .get_sdk()?;
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
