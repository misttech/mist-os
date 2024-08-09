// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "stop", description = "Stops the repository server")]
pub struct StopCommand {
    /// stop all repository servers.
    #[argh(switch)]
    pub all: bool,

    /// stop the repository server with the given name.
    #[argh(positional)]
    pub name: Option<String>,
}
