// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {argh::FromArgs, ffx_core::ffx_command};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "snapshot", description = "Snapshot current heap memory usage")]
pub struct SnapshotCommand {
    #[argh(option, description = "select process by name")]
    pub by_name: Option<String>,
    #[argh(option, description = "select process by koid")]
    pub by_koid: Option<u64>,
    #[argh(option, description = "output protobuf file")]
    pub output_file: String,
}
