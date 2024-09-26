// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "remove",
    description = "Remove repository from deamon server configuration."
)]
pub struct RemoveCommand {
    /// name of the repository to remove.
    #[argh(positional)]
    pub name: Option<String>,

    /// remove all repositories
    #[argh(switch)]
    pub all: bool,
}
