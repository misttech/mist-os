// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use argh::{ArgsInfo, FromArgs, SubCommand};
use fho::subtool::{StandaloneFhoHandler, StandaloneToolCommand};
use fho::{FfxContext, Result};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;

/// Config element for the path of the socket we will use to communicate.
const USB_SOCKET_PATH_CONFIG: &str = "connectivity.usb_socket_path";

/// Default name for the control socket.
const USB_SOCKET_NAME: &str = "ffx_usb.sock";

// [START command_struct]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "usb-driver")]
/// Start a long-running process to drive USB-connected Fuchsia devices and
/// allow them to be connected to from FFX.
pub struct UsbDriverCommand {
    #[argh(switch)]
    /// whether to fork the driver process into the background rather than run
    background: bool,
}
// [END command_struct]

pub async fn run() {
    let result = implementation().await;
    let should_format = match fho::FfxCommandLine::from_env() {
        Ok(cli) => cli.global.machine.is_some(),
        Err(e) => {
            log::warn!("Received error getting command line: {}", e);
            match e {
                fho::Error::Help { .. } => false,
                _ => true,
            }
        }
    };
    ffx_command::exit(result, should_format).await;
}

async fn implementation() -> Result<ExitStatus> {
    let ffx_command::InitializedCmd { cmd: ffx, context: ctx, help_state } =
        ffx_command::init_cmd(ffx_config::environment::ExecutableKind::Subtool)?;

    // TODO(429272253): Set up logging

    match help_state {
        ffx_command::HelpState::ReturnArgsInfo => {
            let args_info = ffx_command::CliArgsInfo::from(UsbDriverCommand::get_args_info());
            let output = match ffx.global.machine.unwrap() {
                ffx_command::MachineFormat::Json => serde_json::to_string(&args_info),
                ffx_command::MachineFormat::JsonPretty => serde_json::to_string_pretty(&args_info),
                ffx_command::MachineFormat::Raw => Ok(format!("{args_info:#?}")),
            };
            println!("{}", output.bug_context("Error serializing args")?);
            return Ok(ExitStatus::from_raw(0));
        }
        ffx_command::HelpState::ReturnHelp { command, output, code } => {
            return Err(fho::Error::Help { command, output, code })
        }
        ffx_command::HelpState::None => (),
    }

    let args = Vec::from_iter(ffx.global.subcommand.iter().map(String::as_str));
    let command = StandaloneToolCommand::<UsbDriverCommand>::from_args(
        &Vec::from_iter(ffx.cmd_iter()),
        &args,
    )
    .map_err(|err| ffx_command::Error::from_early_exit(&ffx.command, err))?;

    let _command = match command.subcommand {
        StandaloneFhoHandler::Metadata(metadata_cmd) => {
            return metadata_cmd.run(UsbDriverCommand::COMMAND).await;
        }
        StandaloneFhoHandler::Standalone(cmd) => cmd,
    };

    if ffx.global.schema {
        todo!();
    }

    let (socket_path, found_config) = ffx_config::build()
        .context(Some(&ctx))
        .level(Some(ffx_config::ConfigLevel::Runtime))
        .name(Some(USB_SOCKET_PATH_CONFIG))
        .get::<PathBuf>()
        .map(|x| (x, true))
        .or_else(|_| -> fho::Result<_> {
            let runtime_dir =
                std::env::var("XDG_RUNTIME_DIR").ok().filter(|x| !x.is_empty()).ok_or_else(
                    || fho::Error::Unexpected(anyhow::anyhow!("$XDG_RUNTIME_DIR is not set")),
                )?;

            let mut ret = PathBuf::from(runtime_dir);
            ret.push(USB_SOCKET_NAME);
            Ok((ret, false))
        })?;

    if !found_config
        && ffx_config::build()
            .context(Some(&ctx))
            .name(Some(USB_SOCKET_PATH_CONFIG))
            .get::<String>()
            .is_ok()
    {
        return Err(fho::Error::User(anyhow::anyhow!(
            "{USB_SOCKET_PATH_CONFIG} must only be set on the command line"
        )));
    }

    // TODO(429272257): Daemonize if we're supposed to run in the background.
    usb_driver_impl::HostDriver::run(socket_path).await;
    Ok(ExitStatus::from_raw(0))
}
