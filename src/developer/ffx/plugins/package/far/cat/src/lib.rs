// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_package_far_cat_args::CatCommand;
use fuchsia_archive as far;
use std::{
    fs::File,
    io::{self, Write as _},
};

#[ffx_plugin()]
pub async fn cmd_cat(cmd: CatCommand) -> Result<()> {
    let far_file = File::open(&cmd.far_file)
        .with_context(|| format!("failed to open file: {}", cmd.far_file.display()))?;
    let mut reader = far::Reader::new(far_file)
        .with_context(|| format!("failed to parse FAR file: {}", cmd.far_file.display()))?;

    let bytes = reader.read_file(cmd.path.as_str().as_bytes()).with_context(|| {
        format!("failed to read path {} from FAR file {}", cmd.path, cmd.far_file.display())
    })?;
    io::stdout().write_all(&bytes)?;

    Ok(())
}
