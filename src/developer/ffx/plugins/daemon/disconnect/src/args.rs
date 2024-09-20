// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "disconnect",
    description = "Disconnect from a target",
    note = "Tells the daemon to disconnect from the specified target.
Useful when the client knows the connection is no longer valid, e.g.
in a test in which the target has been rebooted."
)]
pub struct DisconnectCommand {}
