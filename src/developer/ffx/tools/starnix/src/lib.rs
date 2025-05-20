// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_writer::VerifiedMachineWriter;
use fho::{FfxContext, FfxMain, FfxTool, Result};
use schemars::JsonSchema;
use serde::Serialize;
use target_connector::Connector;
use target_holders::{RemoteControlProxyHolder, TargetProxyHolder};

pub mod common;
use common::connect_to_rcs;

mod adb;
mod console;
mod vmo;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum StarnixSubCommand {
    Adb(adb::StarnixAdbCommand),
    #[cfg(feature = "enable_console_tool")]
    Console(console::StarnixConsoleCommand),
    Vmo(vmo::StarnixVmoCommand),
}

#[derive(Debug, JsonSchema, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StarnixToolOutput {
    Adb(adb::AdbCommandOutput),
    #[cfg(feature = "enable_console_tool")]
    Console(console::ConsoleCommandOutput),
    Vmo(vmo::VmoCommandOutput),
}

impl std::fmt::Display for StarnixToolOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adb(o) => write!(f, "{o}"),
            #[cfg(feature = "enable_console_tool")]
            Self::Console(o) => write!(f, "{o}"),
            Self::Vmo(o) => write!(f, "{o}"),
        }
    }
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
    rcs_connector: Connector<RemoteControlProxyHolder>,
    target_proxy: Result<TargetProxyHolder>,
    context: EnvironmentContext,
}

#[async_trait(?Send)]
impl FfxMain for StarnixTool {
    type Writer = VerifiedMachineWriter<StarnixToolOutput>;

    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        let output = match self.cmd.subcommand {
            StarnixSubCommand::Adb(command) => command
                .run(&self.context, &self.rcs_connector, self.target_proxy?)
                .await
                .map(StarnixToolOutput::Adb),
            #[cfg(feature = "enable_console_tool")]
            StarnixSubCommand::Console(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                console::starnix_console(command, &rcs).await.map(StarnixToolOutput::Console)
            }
            StarnixSubCommand::Vmo(command) => {
                let rcs = connect_to_rcs(&self.rcs_connector).await?;
                vmo::starnix_vmo(command, &rcs).await.map(StarnixToolOutput::Vmo)
            }
        }?;
        writer.machine_or_else(&output, || output.to_string()).bug_context("writing output")?;
        Ok(())
    }
}
