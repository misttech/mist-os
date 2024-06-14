// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "add",
    description = "Make the daemon aware of specific product bundle repositories"
)]
pub struct AddCommand {
    /// repositories will have the prefix `NAME`. Defaults to `devhost`.
    #[argh(option, short = 'p', default = "default_prefix()")]
    pub prefix: String,

    /// path to the product bundle directory.
    #[argh(positional)]
    pub product_bundle_dir: Utf8PathBuf,
}

fn default_prefix() -> String {
    "devhost".to_string()
}
