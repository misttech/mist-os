// Copyright 2023 The Fuchsia Authors. All rights reserved.
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
    name = "download",
    example = "\
    Auth flow choices for --auth include:\
    \n  `--auth no-auth` do not use auth.\
    \n  `--auth oob` to use Out-of-Band auth (deprecated).\
    \n  `--auth pkce` to use PKCE auth flow (requires GUI browser).\
    \n  `--auth device-experimental` to use device flow.\
    \n  `--auth <path/to/exe>` run tool at given path which will print an \
    access token to stdout and exit 0.
    \n  `--auth default` let the tool decide which auth flow to use.\
    \n\n\
    Legacy mode is used to download legacy, PBv1 product bundles:\
    \n   ffx product download --legacy-release 11 core.x64"
)]
pub struct DownloadCommand {
    /// get the data again, even if it's already present locally.
    #[argh(switch)]
    pub force: bool,

    /// use specific auth flow for oauth2 (see examples; default: pkce).
    #[argh(option, default = "AuthFlowChoice::Default")]
    pub auth: AuthFlowChoice,

    /// url to the transfer manifest of the product bundle to download.
    /// If using legacy mode, this is the product name.
    #[argh(positional)]
    pub manifest_url: String,

    /// local name of the product bundle directory. If downloading a legacy
    /// product bundle, this argument is not used. The PBv1 product bundles
    /// are stored in the internal product bundle index.
    #[argh(positional)]
    pub product_dir: Option<PathBuf>,

    /// legacy release version to get. This is the "F" release number, and
    /// gets the latest version of the product bundle for that version.
    /// Required when using the use-legacy flag. This value can be the
    /// integer part, "11", which results in latest F11 build,
    /// or a full version number such as "11.20230306.3.112"
    #[argh(option)]
    pub legacy_release: Option<String>,
}
