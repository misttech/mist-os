// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Argument-parsing specification for the `components` subcommand.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

/// Components.
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "components")]
pub struct ComponentsCommand {
    #[argh(switch, description = "loads the unprocessed memory information as json from stdin.")]
    pub stdin_input: bool,

    /// Uses the same format as the FIDL table Snapshot in
    /// //sdk/fidl/fuchsia.memory.attribution.plugin/plugin.fidl.
    #[argh(
        switch,
        description = "outputs the unprocessed memory information from the device as json."
    )]
    pub debug_json: bool,

    #[argh(switch, description = "outputs data in csv format.")]
    pub csv: bool,
}
