// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(unused)]

use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;

/**
Construct a product bundle using a platform, product, and board.
The inputs can be local paths or CIPD references.

Artifact formats:
  path/to/artifacts: Use local artifact with specified path
  cipd://path/to/artifact@1.2.3.4: Use artifact from CIPD with specified url

Additional formats for the platform:
  <omitted>: Use default local platform
  1.2.3.4: Use platform from CIPD with specified version
*/
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "create")]
pub struct CreateCommand {
    /// product.board combination to build when inside a fuchsia checkout.
    #[argh(positional)]
    pub product_board_combo: Option<String>,

    /// the platform artifacts to use.
    #[argh(option)]
    pub platform: Option<String>,

    /// the product config to use.
    #[argh(option)]
    pub product: Option<String>,

    /// the board config to use.
    #[argh(option)]
    pub board: Option<String>,

    /// prepare the assembly inputs, but do not run assembly yet.
    #[argh(switch)]
    pub stage: bool,

    /// the location to write the product bundle to.
    #[argh(option)]
    pub out: Option<Utf8PathBuf>,
}
