// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use fidl_fuchsia_developer_ffx::{RepositoryRegistrationAliasConflictMode, RepositoryStorageType};

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "register",
    description = "Make the target aware of a specific repository"
)]
pub struct RegisterCommand {
    #[argh(option, short = 'r')]
    /// register this repository, rather than the default.
    pub repository: Option<String>,

    #[argh(option, short = 'p')]
    /// repository server port number.
    /// Required to disambiguate multiple repositories with the same name.
    pub port: Option<u16>,

    /// enable persisting this repository across reboots.
    #[argh(option, from_str_fn(parse_storage_type))]
    pub storage_type: Option<RepositoryStorageType>,

    /// set up a rewrite rule mapping each `alias` host to
    /// to the repository identified by `name`.
    #[argh(option)]
    pub alias: Vec<String>,

    /// resolution mechanism when alias registrations conflict. Must be either
    /// `error-out` or `replace`. Default is `replace`.
    #[argh(
        option,
        default = "default_alias_conflict_mode()",
        from_str_fn(parse_alias_conflict_mode)
    )]
    pub alias_conflict_mode: RepositoryRegistrationAliasConflictMode,
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
