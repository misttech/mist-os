// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use errors::ffx_error;
use ffx_power_debugcmd_args::PowerManagerDebugCommand;
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_power_manager_debug as fdebug;

#[derive(FfxTool)]
pub struct DebugCmdTool {
    #[command]
    cmd: PowerManagerDebugCommand,
    #[with(moniker("/bootstrap/power_manager"))]
    debug: fdebug::DebugProxy,
}

fho::embedded_plugin!(DebugCmdTool);

#[async_trait(?Send)]
impl FfxMain for DebugCmdTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        debugcmd(self.debug, self.cmd).await
    }
}

async fn debugcmd(proxy: fdebug::DebugProxy, cmd: PowerManagerDebugCommand) -> fho::Result<()> {
    proxy
        .message(&cmd.node_name, &cmd.command, &cmd.args)
        .await
        .map_err(|err| ffx_error!("Failed to call Debug/Message: {}", err))?
        .map_err(|e| match e {
            fdebug::MessageError::Generic => ffx_error!("Generic error occurred"),
            fdebug::MessageError::InvalidNodeName => {
                ffx_error!("Invalid node name '{}'", cmd.node_name)
            }
            fdebug::MessageError::UnsupportedCommand => {
                ffx_error!("Unsupported command '{}' for node '{}'", cmd.command, cmd.node_name)
            }
            fdebug::MessageError::InvalidCommandArgs => {
                ffx_error!("Invalid arguments for command '{}'", cmd.command)
            }
            e => ffx_error!("Unknown error: {:?}", e),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_debugcmd() {
        let command_request = PowerManagerDebugCommand {
            node_name: "test_node_name".to_string(),
            command: "test_command".to_string(),
            args: vec!["test_arg_1".to_string(), "test_arg_2".to_string()],
        };

        let debug_proxy = fho::testing::fake_proxy(move |req| match req {
            fdebug::DebugRequest::Message { node_name, command, args, responder, .. } => {
                assert_eq!(node_name, "test_node_name");
                assert_eq!(command, "test_command");
                assert_eq!(args, vec!["test_arg_1", "test_arg_2"]);
                let _ = responder.send(Ok(()));
            }
        });

        debugcmd(debug_proxy, command_request).await.unwrap();
    }
}
