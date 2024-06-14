// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::subcommands::ping::args::PingCommand;
use super::subcommands::read::args::ReadCommand;
use super::subcommands::transact::args::TransactCommand;
use super::subcommands::write::args::WriteCommand;
use argh::{ArgsInfo, FromArgs};

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "i2c", description = "Perform reads and writes on an I2C device")]
pub struct I2cCommand {
    #[argh(subcommand)]
    pub subcommand: I2cSubCommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum I2cSubCommand {
    Ping(PingCommand),
    Read(ReadCommand),
    Transact(TransactCommand),
    Write(WriteCommand),
}
