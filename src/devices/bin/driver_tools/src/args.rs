// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::subcommands::device::args::DeviceCommand;
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
use {
    super::subcommands::{
        i2c::args::I2cCommand, lspci::args::LspciCommand, lsusb::args::LsusbCommand,
        print_input_report::args::PrintInputReportCommand, runtool::args::RunToolCommand,
    },
    // Driver conformance testing is run on the host against a target device's driver.
    // So, this subcommand is only relevant on the host side.
    // If we are host-side, then we will import the conformance library.
    // Otherwise, we will use the placeholder subcommand declared above.
    conformance_lib::args::ConformanceCommand,
    static_checks_lib::args::StaticChecksCommand,
};

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
    Device(DeviceCommand),
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
    Conformance(ConformanceCommand),
    Device(DeviceCommand),
    Disable(DisableCommand),
    Dump(DumpCommand),
    I2c(I2cCommand),
    List(ListCommand),
    ListComposites(ListCompositesCommand),
    ListDevices(ListDevicesCommand),
    ListHosts(ListHostsCommand),
    ListCompositeNodeSpecs(ListCompositeNodeSpecsCommand),
    Lspci(LspciCommand),
    Lsusb(LsusbCommand),
    PrintInputReport(PrintInputReportCommand),
    Register(RegisterCommand),
    Restart(RestartCommand),
    RunTool(RunToolCommand),
    StaticChecks(StaticChecksCommand),
    TestNode(TestNodeCommand),
}
