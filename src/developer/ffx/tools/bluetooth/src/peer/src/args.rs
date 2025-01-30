// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;

// ffx bluetooth peer
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "peer",
    description = "Show details for a known peer.",
    example = "ffx bluetooth peer"
)]
pub struct PeerCommand {
    /// list, show, connect, or disconnect
    #[argh(subcommand)]
    pub subcommand: PeerSubCommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(subcommand)]
pub enum PeerSubCommand {
    List(ListCommand),
    Show(ShowCommand),
    Connect(ConnectCommand),
    Disconnect(DisconnectCommand),
}

/// ffx bluetooth peer list
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "list",
    description = "Show all known peers in a summarized view (optionally filtered).",
    example = "ffx bluetooth peer list <filter>"
)]
pub struct ListCommand {
    /// filter all known peers by id, address, or name (case-insensitive)
    #[argh(positional)]
    pub filter: Option<String>,

    /// show details for all known peers
    #[argh(switch)]
    pub details: bool,
}

/// ffx bluetooth peer show
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "show",
    description = "Show details for a known peer.",
    example = "ffx bluetooth peer show <id|addr>"
)]
pub struct ShowCommand {
    /// specify peer by id or address
    #[argh(positional)]
    pub id_or_addr: ffx_bluetooth_common::PeerIdOrAddr,
}

/// ffx bluetooth peer connect
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "connect",
    description = "Connect to a peer.",
    example = "ffx bluetooth peer connect <id|addr>"
)]
pub struct ConnectCommand {
    /// specify peer by id or address
    #[argh(positional)]
    pub id_or_addr: ffx_bluetooth_common::PeerIdOrAddr,
}

/// ffx bluetooth peer disconnect
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "disconnect",
    description = "Disconnect from a peer.",
    example = "ffx bluetooth peer disconnect <id|addr>"
)]
pub struct DisconnectCommand {
    /// specify peer by id or address
    #[argh(positional)]
    pub id_or_addr: ffx_bluetooth_common::PeerIdOrAddr,
}
