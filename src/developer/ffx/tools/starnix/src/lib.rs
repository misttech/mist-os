// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use async_trait::async_trait;
use fho::{Error, FfxMain, FfxTool, Result, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;

pub mod common;

mod adb;
mod console;

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum StarnixSubCommand {
    Adb(adb::StarnixAdbCommand),
    Console(console::StarnixConsoleCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "starnix", description = "Control starnix containers")]
pub struct StarnixCommand {
    #[argh(subcommand)]
    subcommand: StarnixSubCommand,
}

#[derive(FfxTool)]
pub struct StarnixTool {
    #[command]
    cmd: StarnixCommand,

    rcs_proxy: rc::RemoteControlProxy,
}

#[async_trait(?Send)]
impl FfxMain for StarnixTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        match &self.cmd.subcommand {
            StarnixSubCommand::Adb(command) => {
                adb::starnix_adb(command, &self.rcs_proxy, writer).await.map_err(|e| Error::User(e))
            }
            StarnixSubCommand::Console(command) => {
                console::starnix_console(command, &self.rcs_proxy, writer)
                    .await
                    .map_err(|e| Error::User(e))
            }
        }
    }
}
