// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;

pub use guest_cli_args::*;

#[derive(FromArgs, PartialEq, Debug)]
/// Top-level command.
pub struct GuestOptions {
    #[argh(subcommand)]
    pub nested: SubCommands,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum SubCommands {
    Attach(attach_args::AttachArgs),
    Launch(launch_args::LaunchArgs),
    Stop(stop_args::StopArgs),
    Balloon(BalloonArgs),
    BalloonStats(BalloonStatsArgs),
    List(list_args::ListArgs),
    Socat(SocatArgs),
    SocatListen(SocatListenArgs),
    Vsh(VshArgs),
    VsockPerf(vsockperf_args::VsockPerfArgs),
    Wipe(wipe_args::WipeArgs),
}
