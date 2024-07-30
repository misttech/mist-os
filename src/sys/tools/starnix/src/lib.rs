// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use argh::{ArgsInfo, FromArgs};

pub mod common;
mod console;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum StarnixSubCommand {
    #[cfg(feature = "enable_console_tool")]
    Console(console::StarnixConsoleCommand),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(name = "starnix", description = "Control starnix containers")]
pub struct StarnixCommand {
    #[argh(subcommand)]
    subcommand: StarnixSubCommand,
}

pub async fn exec() -> Result<()> {
    let args: StarnixCommand = argh::from_env();

    match &args.subcommand {
        StarnixSubCommand::Console(args) => console::starnix_console(args).await,
    }
}
