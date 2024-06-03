// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    argh::{ArgsInfo, FromArgs},
    ffx_core::ffx_command,
    ffx_power_system_activity_sub_command::SubCommand,
};

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "system-activity", description = "Manipulate SAG power elements")]
/// Top-level command for "ffx power system-activity".
pub struct SystemActivityCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}
