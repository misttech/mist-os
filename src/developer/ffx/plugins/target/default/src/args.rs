// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "default",
    description = "Manage the default target",
    example = "For one-off overrides for the default use `--target` option:

    $ ffx --target <target name> <subcommand>

Or use the `--config` option:

    $ ffx --config target.default=<target name> <subcommand>",
    note = "Manages the default configured target for all operations. The default
target is designated by a `*` next to the name. This is an alias for the
`target.default` configuration key."
)]
pub struct TargetDefaultCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    Get(TargetDefaultGetCommand),
    Set(TargetDefaultSetCommand),
    Unset(TargetDefaultUnsetCommand),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "get",
    description = "Get the default configured target",
    note = "Returns the effective default configured target from configuration,
$FUCHSIA_NODENAME, and $FUCHSIA_DEVICE_ADDR.
Returns an empty string if no default is configured."
)]
pub struct TargetDefaultGetCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "set",
    description = "Set the default target",
    example = "To set the default target:

   $ ffx target default set <target name>",
    note = "Sets the `target.default` configuration key in 'User Configuration'
scope.

After setting the default target, `ffx target list` will mark the default
with a `*` in the output list."
)]
pub struct TargetDefaultSetCommand {
    #[argh(positional)]
    /// node name of the target
    pub nodename: String,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "unset",
    description = "Clears the configured default target",
    example = "To clear the default target:

    $ ffx target default unset",
    note = "Clears the `target.default` configuration key on all configuration
levels. Returns a warning if it's already unset. Returns an error if it's not
possible to clear it."
)]
pub struct TargetDefaultUnsetCommand {}
