// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "application-activity",
    description = "Controls the topology-test-daemon component to manipulate application_activity \
power element power levels in system_activity_governor.",
    example = "\
To change application_activity power level to 1:

    $ ffx power system-activity application-activity start

    To change application_activity power level to 0:

    $ ffx power system-activity application-activity stop",
    note = "\
If the topology-test-daemon component is not available to the target, then this command will not
work properly."
)]
/// Top-level command for "ffx power system-activity application-activity".
pub struct Command {
    #[argh(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum SubCommand {
    Start(StartCommand),
    Stop(StopCommand),
    Restart(RestartCommand),
}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Start application activity on the target
#[argh(subcommand, name = "start")]
pub struct StartCommand {}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Stop application activity on the target
#[argh(subcommand, name = "stop")]
pub struct StopCommand {}

#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
/// Stop application activity on the target and start it again.
#[argh(subcommand, name = "restart")]
pub struct RestartCommand {
    #[argh(option, default = "100000000")]
    /// the time the system waits before starting application activity again (in nanoseconds).
    /// The system is not guaranteed to start again after this time, but on the next wakeup
    /// this command will take a lease on application activity.
    /// Defaults to 100ms.
    pub wait_time_ns: u64,
}
