// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use async_trait::async_trait;
use fho::{daemon_protocol, Error, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_ffx::TargetCollectionProxy;
use fidl_fuchsia_developer_remotecontrol as rc;

pub mod common;

mod adb;
mod console;
mod vmo;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum StarnixSubCommand {
    Adb(adb::StarnixAdbCommand),
    Console(console::StarnixConsoleCommand),
    Vmo(vmo::StarnixVmoCommand),
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

    #[with(daemon_protocol())]
    target_collection: TargetCollectionProxy,

    rcs_proxy: rc::RemoteControlProxy,
}

#[async_trait(?Send)]
impl FfxMain for StarnixTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        match &self.cmd.subcommand {
            StarnixSubCommand::Adb(command) => {
                adb::starnix_adb(command, &self.rcs_proxy, &self.target_collection, writer)
                    .await
                    .map_err(|e| Error::User(e))
            }
            StarnixSubCommand::Console(command) => {
                console::starnix_console(command, &self.rcs_proxy, writer)
                    .await
                    .map_err(|e| Error::User(e))
            }
            StarnixSubCommand::Vmo(command) => {
                vmo::starnix_vmo(command, &self.rcs_proxy, writer).await.map_err(|e| Error::User(e))
            }
        }
    }
}
