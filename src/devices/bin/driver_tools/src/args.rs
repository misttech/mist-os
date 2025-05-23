// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::subcommands::disable::args::DisableCommand;
use super::subcommands::dump::args::DumpCommand;
use super::subcommands::list::args::ListCommand;
use super::subcommands::list_composite_node_specs::args::ListCompositeNodeSpecsCommand;
use super::subcommands::list_composites::args::ListCompositesCommand;
use super::subcommands::list_devices::args::ListDevicesCommand;
use super::subcommands::list_hosts::args::ListHostsCommand;
use super::subcommands::register::args::RegisterCommand;
use super::subcommands::restart::args::RestartCommand;
use super::subcommands::test_node::args::TestNodeCommand;
use argh::{ArgsInfo, FromArgs};

#[cfg(not(target_os = "fuchsia"))]
use static_checks_lib::args::StaticChecksCommand;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(name = "driver", description = "Support driver development workflows")]
pub struct DriverCommand {
    #[argh(subcommand)]
    pub subcommand: DriverSubCommand,
}

#[cfg(target_os = "fuchsia")]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum DriverSubCommand {
    Disable(DisableCommand),
    Dump(DumpCommand),
    List(ListCommand),
    ListComposites(ListCompositesCommand),
    ListDevices(ListDevicesCommand),
    ListHosts(ListHostsCommand),
    ListCompositeNodeSpecs(ListCompositeNodeSpecsCommand),
    Register(RegisterCommand),
    Restart(RestartCommand),
    TestNode(TestNodeCommand),
}

// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
#[cfg(not(target_os = "fuchsia"))]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum DriverSubCommand {
    Disable(DisableCommand),
    Dump(DumpCommand),
    List(ListCommand),
    ListComposites(ListCompositesCommand),
    ListDevices(ListDevicesCommand),
    ListHosts(ListHostsCommand),
    ListCompositeNodeSpecs(ListCompositeNodeSpecsCommand),
    Register(RegisterCommand),
    Restart(RestartCommand),
    StaticChecks(StaticChecksCommand),
    TestNode(TestNodeCommand),
}
