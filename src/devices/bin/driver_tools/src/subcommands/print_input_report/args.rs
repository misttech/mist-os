// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::subcommands::descriptor::args::DescriptorCommand;
use super::subcommands::feature::args::FeatureCommand;
use super::subcommands::get::args::GetCommand;
use super::subcommands::read::args::ReadCommand;
use argh::{ArgsInfo, FromArgs};

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "print-input-report",
    description = "Prints input reports and other information of input devices"
)]
pub struct PrintInputReportCommand {
    #[argh(subcommand)]
    pub subcommand: PrintInputReportSubcommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum PrintInputReportSubcommand {
    Descriptor(DescriptorCommand),
    Feature(FeatureCommand),
    Get(GetCommand),
    Read(ReadCommand),
}
