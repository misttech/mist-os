// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;
use fidl_fuchsia_developer_ffx::{RepositoryRegistrationAliasConflictMode, RepositoryStorageType};
use std::{
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
};

// TODO(b/295560556): Expand to handle multiple repositories.
#[ffx_command()]
#[derive(ArgsInfo, Clone, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "serve",
    description = "(EXPERIMENTAL) serve the repository server, registering repositories to device"
)]
pub struct ServeCommand {
    #[argh(option, short = 'r')]
    /// register this repository.
    /// Default is `devhost`.
    pub repository: Option<String>,

    /// path to the root metadata that was used to sign the
    /// repository TUF metadata. This establishes the root of
    /// trust for this repository. If the TUF metadata was not
    /// signed by this root metadata, running this command
    /// will result in an error.
    /// Default is to use 1.root.json from the repository.
    #[argh(option)]
    pub trusted_root: Option<Utf8PathBuf>,

    /// address on which to serve the repository.
    /// Note that this can be either IPV4 or IPV6.
    /// For example, [::]:8083 or 127.0.0.1:8083
    /// Default is `[::]:8083`.
    #[argh(option, default = "default_address()")]
    pub address: SocketAddr,

    /// location of the package repo.
    /// Default is given by the build directory
    /// obtained from the ffx context.
    #[argh(option)]
    pub repo_path: Option<Utf8PathBuf>,

    /// location of product bundle.
    #[argh(option)]
    pub product_bundle: Option<Utf8PathBuf>,

    /// set up a rewrite rule mapping each `alias` host to
    /// the repository identified by `name`.
    #[argh(option)]
    pub alias: Vec<String>,

    /// enable persisting this repository across reboots.
    /// Default is `Ephemeral`.
    #[argh(option, from_str_fn(parse_storage_type))]
    pub storage_type: Option<RepositoryStorageType>,

    /// resolution mechanism when alias registrations conflict. Must be either
    /// `error-out` or `replace`.
    /// Default is `replace`.
    #[argh(
        option,
        default = "default_alias_conflict_mode()",
        from_str_fn(parse_alias_conflict_mode)
    )]
    pub alias_conflict_mode: RepositoryRegistrationAliasConflictMode,

    /// location to write server port information to, in case port dynamically
    /// instantiated.
    #[argh(option)]
    pub port_path: Option<PathBuf>,

    /// if true, will not register repositories to device.
    /// Default is `false`.
    #[argh(switch)]
    pub no_device: bool,
}

fn default_address() -> SocketAddr {
    (Ipv6Addr::UNSPECIFIED, 8083).into()
}

fn parse_storage_type(arg: &str) -> Result<RepositoryStorageType, String> {
    match arg {
        "ephemeral" => Ok(RepositoryStorageType::Ephemeral),
        "persistent" => Ok(RepositoryStorageType::Persistent),
        _ => Err(format!("unknown storage type {}", arg)),
    }
}

fn default_alias_conflict_mode() -> RepositoryRegistrationAliasConflictMode {
    RepositoryRegistrationAliasConflictMode::Replace
}

fn parse_alias_conflict_mode(arg: &str) -> Result<RepositoryRegistrationAliasConflictMode, String> {
    match arg {
        "error-out" => Ok(RepositoryRegistrationAliasConflictMode::ErrorOut),
        "replace" => Ok(RepositoryRegistrationAliasConflictMode::Replace),
        _ => Err(format!("unknown alias conflict mode {}", arg)),
    }
}
