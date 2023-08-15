// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use camino::Utf8PathBuf;
use ffx_core::ffx_command;
use sdk_metadata::Type;

/// Get the paths of a group of artifacts inside a Product Bundle.
#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "get-artifacts")]
pub struct GetArtifactsCommand {
    /// path to product bundle directory.
    #[argh(positional)]
    pub product_bundle: Utf8PathBuf,

    /// select what group of artifacts to list
    #[argh(option, short = 'g')]
    pub artifacts_group: Type,

    /// return relative path or not
    #[argh(switch, short = 'r')]
    pub relative_path: bool,
}
