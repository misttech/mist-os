// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_package_far_create_args::CreateCommand;
use fuchsia_archive as far;
use std::{collections::BTreeMap, fs::File, io::Read};
use walkdir::WalkDir;

#[ffx_plugin()]
pub async fn cmd_create(cmd: CreateCommand) -> Result<()> {
    let mut entries = BTreeMap::new();

    for file in WalkDir::new(&cmd.input_directory).follow_links(true) {
        let file = file?;
        if file.file_type().is_dir() {
            continue;
        }
        if !file.file_type().is_file() {
            eprintln!("Not a regular file; ignoring: {}", file.path().display());
            continue;
        }

        let len = file.metadata()?.len();
        let reader = File::open(file.path())
            .with_context(|| format!("failed to open file: {}", file.path().display()))?;
        let reader: Box<dyn Read> = Box::new(reader);

        // Omit the base directory (which is common to all paths).
        let path = file.path().strip_prefix(&cmd.input_directory).unwrap();
        let path =
            path.to_str().with_context(|| format!("non-unicode file path: {}", path.display()))?;

        entries.insert(String::from(path), (len, reader));
    }

    let output_file = File::create(&cmd.output_file)
        .with_context(|| format!("failed to create file: {}", cmd.output_file.display()))?;
    far::write(output_file, entries)
        .with_context(|| format!("failed to write FAR file: {}", cmd.output_file.display()))?;

    Ok(())
}
