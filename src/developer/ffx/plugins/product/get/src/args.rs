// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use pbms::AuthFlowChoice;
use std::path::PathBuf;

/// Retrieve a Product Bundle directory of images and related data.
///
/// This command has three calling modes: with three inputs of <bucket>
/// <product.board> --version <version>, one positional arg as a product bundle
/// uri (pb:<context>:<product.board>:<version>), or one positional arg as a
/// full URL to the transfer.json
#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "get",
    example = "\
    All three of these commands get the same product bundle:\
    \n  $ ffx product get gs://fuchsia-public-artifacts-release/builds/8782862537611362513/transfer.json\
    \n  $ ffx product get pb:fuchsia:core.x64-dfv2:12.20230425.1.1\
    \n  $ ffx product get fuchsia core.x64-dfv2 --version 12.20230425.1.1",
    note = "\
    Auth flow choices for --auth include:\
    \n  `--auth oob` to use Out-of-Band auth (deprecated).\
    \n  `--auth pkce` to use PKCE auth flow (requires GUI browser).\
    \n  `--auth <path/to/exe>` run tool at given path which will print an \
    access token to stdout and exit 0."
)]
pub struct GetCommand {
    /// get the data again, even if it's already present locally.
    #[argh(switch)]
    pub force: bool,

    /// use specific auth flow for oauth2 (see examples; default: pkce).
    #[argh(option, default = "AuthFlowChoice::Default")]
    pub auth: AuthFlowChoice,

    /// local directory to download the product bundle into (default:
    /// "local_pb").
    #[argh(option, default = "PathBuf::from(\"local_pb\")")]
    pub out_dir: PathBuf,

    /// gcs bucket, pb uri, or url to the transfer manifest.
    #[argh(positional)]
    pub context_uri: String,

    /// name of the product bundle (used only if first arg is not a uri).
    /// if present, 'context_uri' is expected to be a gcs bucket.
    #[argh(positional)]
    pub product_name: Option<String>,

    /// look in the latest sdk build (does not find the most recent if the
    /// product is not in the 'latest' build). Overrides --version.
    #[argh(switch)]
    pub latest: bool,

    /// version of the product bundle (used only if first arg is not a uri).
    /// Default: current sdk version. Ignored if --latest is used.
    #[argh(option)]
    pub version: Option<String>,

    /// run experimental version of the tool
    #[argh(switch)]
    pub experimental: bool,
}
