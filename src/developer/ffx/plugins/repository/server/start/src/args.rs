// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use std::net::SocketAddr;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "start",
    description = "Starts the repository server. Note that all \
    repositories listed from `ffx repository list` will be started as subpaths."
)]
pub struct StartCommand {
    /// address on which to start the repository.
    /// Note that this can be either IPV4 or IPV6.
    /// For example, [::]:8083 or 127.0.0.1:8083
    #[argh(option)]
    pub address: Option<SocketAddr>,
}
