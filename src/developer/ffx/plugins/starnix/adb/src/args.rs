// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {argh::FromArgs, ffx_core::ffx_command};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "adb",
    example = "ffx starnix adb",
    description = "Bridge from host adb to adbd running inside starnix"
)]

pub struct AdbStarnixCommand {
    /// the moniker of the container running adbd
    /// (defaults to looking for a container in the current session)
    #[argh(option, short = 'm')]
    pub moniker: Option<String>,

    /// which port to serve the adb server on
    #[argh(option, short = 'p', default = "5556")]
    pub port: u16,
}
