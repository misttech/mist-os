// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use async_trait::async_trait;
use fho::{Connector, Error, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;

pub mod common;
use common::connect_to_rcs;

mod adb;
mod console;
mod suspend;
mod vmo;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum StarnixSubCommand {
    Adb(adb::StarnixAdbCommand),
    #[cfg(feature = "enable_console_tool")]
    Console(console::StarnixConsoleCommand),
    Vmo(vmo::StarnixVmoCommand),
    Suspend(suspend::StarnixSuspendCommand),
    Resume(suspend::StarnixResumeCommand),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "starnix", description = "Control starnix containers")]
pub struct StarnixCommand {
    #[argh(subcommand)]
    subcommand: StarnixSubCommand,
}

#[derive(FfxTool)]
pub struct StarnixTool {
    #[command]
    cmd: StarnixCommand,

    rcs_connector: Connector<rc::RemoteControlProxy>,
}

#[async_trait(?Send)]
impl FfxMain for StarnixTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        match &self.cmd.subcommand {
            StarnixSubCommand::Adb(command) => {
                command.run(&self.rcs_connector).await.map_err(|e| Error::User(e))
            }
            #[cfg(feature = "enable_console_tool")]
            StarnixSubCommand::Console(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                console::starnix_console(command, &rcs, writer).await.map_err(|e| Error::User(e))
            }
            StarnixSubCommand::Vmo(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                vmo::starnix_vmo(command, &rcs, writer).await.map_err(|e| Error::User(e))
            }
            StarnixSubCommand::Suspend(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                suspend::starnix_suspend(command, &rcs, writer).await
            }
            StarnixSubCommand::Resume(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                suspend::starnix_resume(command, &rcs, writer).await
            }
        }
    }
}
