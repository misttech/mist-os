// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use pbms::AuthFlowChoice;
use std::path::PathBuf;

/// Retrieve image data.
#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "get",
    example = "\
    Auth flow choices for --auth include:\
    \n  `--auth oob` to use Out-of-Band auth (deprecated).\
    \n  `--auth pkce` to use PKCE auth flow (requires GUI browser).\
    \n  `--auth <path/to/exe>` run tool at given path which will print an \
    access token to stdout and exit 0.
    "
)]
pub struct GetCommand {
    /// get the data again, even if it's already present locally.
    #[argh(switch)]
    pub force: bool,

    /// use specific auth flow for oauth2 (see examples; default: pkce).
    #[argh(option, default = "AuthFlowChoice::Default")]
    pub auth: AuthFlowChoice,

    /// url to the transfer manifest of the product bundle to download.
    #[argh(positional)]
    pub manifest_url: String,

    /// local directory to download the product bundle into (default:
    /// "local_pb").
    #[argh(positional, default = "PathBuf::from(\"local_pb\")")]
    pub out_dir: PathBuf,
}
