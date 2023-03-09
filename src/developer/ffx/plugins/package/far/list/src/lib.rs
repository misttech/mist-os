// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_core::ffx_plugin;
use ffx_package_far_list_args::ListCommand;
use fuchsia_archive as far;
use humansize::{file_size_opts, FileSize};
use prettytable::{cell, row, Table};
use std::fs::File;

#[ffx_plugin()]
pub async fn cmd_list(cmd: ListCommand) -> Result<()> {
    let file = File::open(&cmd.far_file)
        .with_context(|| format!("failed to open file: {}", cmd.far_file.display()))?;
    let reader = far::Reader::new(file)
        .with_context(|| format!("failed to parse FAR file: {}", cmd.far_file.display()))?;

    let mut entries: Vec<_> = reader.list().collect();
    entries.sort_by_key(far::Entry::path);

    if entries.is_empty() {
        println!("FAR file contains no entries.");
    } else {
        print!("{}", format_table(&entries, cmd.long_format));
    }

    Ok(())
}

fn format_table(entries: &[far::Entry<'_>], display_lengths: bool) -> Table {
    let mut table = Table::new();

    if display_lengths {
        table.set_titles(row!["path", "length"]);

        for entry in entries {
            let path = String::from_utf8_lossy(entry.path());
            let length = entry
                .length()
                .file_size(file_size_opts::CONVENTIONAL)
                .expect("length is non-negative");

            table.add_row(row![path, length]);
        }
    } else {
        table.set_titles(row!["path"]);

        for entry in entries {
            let path = String::from_utf8_lossy(entry.path());
            table.add_row(row![path]);
        }
    }

    table
}
