// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use pbms::AuthFlowChoice;
use std::path::PathBuf;

/// Retrieve image data.
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq, Clone)]
#[argh(
    subcommand,
    name = "download",
    example = "\
    Auth flow choices for --auth include:\
    \n  `--auth no-auth` do not use auth.\
    \n  `--auth pkce` to use PKCE auth flow (requires GUI browser).\
    \n  `--auth device-experimental` to use device flow.\
    \n  `--auth <path/to/exe>` run tool at given path which will print an \
    access token to stdout and exit 0.
    \n  `--auth default` let the tool decide which auth flow to use."
)]
pub struct DownloadCommand {
    /// get the data again, even if it's already present locally.
    #[argh(switch)]
    pub force: bool,

    /// use specific auth flow for oauth2 (see examples; default: pkce).
    #[argh(option, default = "AuthFlowChoice::Default")]
    pub auth: AuthFlowChoice,

    /// url to the transfer manifest of the product bundle to download, or the
    /// product name to fetch.
    #[argh(positional)]
    pub manifest_url: String,

    /// local name of the product bundle directory.
    #[argh(positional)]
    pub product_dir: PathBuf,

    /// where to look for product bundles manifest.
    #[argh(option)]
    pub base_url: Option<String>,

    /// filter on products of <version>.
    #[argh(option)]
    pub version: Option<String>,

    /// filter on products of <branch>.
    #[argh(option)]
    pub branch: Option<String>,
}
