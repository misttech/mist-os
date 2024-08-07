// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;
use fidl_fuchsia_developer_ffx::{RepositoryRegistrationAliasConflictMode, RepositoryStorageType};
use std::net::SocketAddr;
use std::path::PathBuf;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
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
    /// Default is read from config `repository.server.listen` or
    /// `[::]:8083` if not set.
    #[argh(option)]
    pub address: Option<SocketAddr>,

    /// run server in as part of the ffx daemon. This is the
    /// default mode. This switch is mutually exclusive with
    /// --foreground.
    #[argh(switch)]
    pub daemon: bool,

    /// run server in as a foreground process.
    #[argh(switch)]
    pub foreground: bool,

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
    #[argh(option, from_str_fn(ffx_repository_serve_args::parse_storage_type))]
    pub storage_type: Option<RepositoryStorageType>,

    /// resolution mechanism when alias registrations conflict. Must be either
    /// `error-out` or `replace`.
    /// Default is `replace`.
    #[argh(
        option,
        default = "ffx_repository_serve_args::default_alias_conflict_mode()",
        from_str_fn(ffx_repository_serve_args::parse_alias_conflict_mode)
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

    /// refresh repository metadata during startup.
    /// Note that this is not necessary if package-tool runs in the background
    /// taking care of it, e.g. as part of `fx serve`.
    /// Default is `false`.
    #[argh(switch)]
    pub refresh_metadata: bool,
}
