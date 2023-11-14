// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;

/// Get the product version of a Product Bundle.
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "get-version")]
pub struct GetVersionCommand {
    /// path to product bundle directory.
    #[argh(positional)]
    pub product_bundle: Utf8PathBuf,
}
