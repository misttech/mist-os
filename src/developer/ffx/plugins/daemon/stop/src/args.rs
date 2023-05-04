// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "stop", description = "stops a running daemon")]
pub struct StopCommand {
    #[argh(switch, short = 'w')]
    /// wait indefinitely for the daemon to stop before exiting (should not
    /// be used in automated systems)
    pub wait: bool,
    #[argh(switch)]
    /// do not wait for daemon to stop (default behavior -- eventually to be deprecated)
    pub no_wait: bool,
    #[argh(option, short = 't')]
    /// optional timeout (in milliseconds) to wait for the daemon to stop. Will try killing
    /// the daemon if it does not exit on its own.
    pub timeout_ms: Option<u32>,
}
