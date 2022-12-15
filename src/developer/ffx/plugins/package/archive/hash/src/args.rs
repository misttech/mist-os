// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use std::path::PathBuf;

#[ffx_command()]
#[derive(Eq, FromArgs, PartialEq, Debug)]
#[argh(
    subcommand,
    name = "hash",
    description = "Fetch the merkleroot of a file which is typically a package archive .far file."
)]
pub struct HashCommand {
    #[argh(positional, description = "package archive path")]
    pub archive: PathBuf,
}
