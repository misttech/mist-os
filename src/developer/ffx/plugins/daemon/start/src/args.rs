// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use std::path::PathBuf;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "start",
    description = "run as daemon -- normally unnecessary, as the daemon is automatically started on demand. Used primarily for debugging"
)]
pub struct StartCommand {
    #[argh(option)]
    /// override the path the socket will be bound to
    pub path: Option<PathBuf>,

    #[argh(switch, short = 'b')]
    /// runs the daemon in the background. Returns after verifying daemon connection. No-op if the
    /// daemon is already running.
    pub background: bool,
}
