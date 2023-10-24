// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "debug",
    description = "Debug components with zxdb.",
    example = "To debug the `brightness_manager` component instance, all of the
following commands are valid:

    $ ffx component debug /core/brightness_manager
    $ ffx component debug fuchsia-pkg://fuchsia.com/brightness_manager#meta/brightness_manager.cm
    $ ffx component debug meta/brightness_manager.cm
    $ ffx component debug brightness_manager",
    note = "This command supports partial matches over the moniker, URL and instance ID"
)]

pub struct ComponentDebugCommand {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}
