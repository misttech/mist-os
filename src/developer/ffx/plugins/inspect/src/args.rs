// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use ffx_inspect_sub_command::SubCommand;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "inspect",
    description = "Query component nodes exposed via the Inspect API. \n\n\
    If you wish to see the JSON format of Inspect, you must pass `--machine json` to the `ffx` \
    command. \n\
    For example to see the Inspect JSON of all components running in the system, run: \n\n\
    ```\n\
    ffx --machine json inspect show\n\
    ```"
)]
pub struct InspectCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}
