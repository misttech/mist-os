// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use ffx_package_far_create_args::CreateCommand;
use ffx_writer::{SimpleWriter, ToolIO as _};
use fho::{bug, user_error, FfxMain, FfxTool, Result};
use fuchsia_archive as far;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use walkdir::WalkDir;

#[derive(FfxTool)]
pub struct FarCreateTool {
    #[command]
    pub cmd: CreateCommand,
}

fho::embedded_plugin!(FarCreateTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for FarCreateTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        let mut entries = BTreeMap::new();

        for file in WalkDir::new(&self.cmd.input_directory).follow_links(true) {
            let file = file.map_err(|e| bug!(e))?;
            if file.file_type().is_dir() {
                continue;
            }
            if !file.file_type().is_file() {
                writeln!(
                    writer.stderr(),
                    "Not a regular file; ignoring: {}",
                    file.path().display()
                )
                .map_err(|e| bug!(e))?;
                continue;
            }

            let len = file.metadata().map_err(|e| bug!(e))?.len();
            let reader = File::open(file.path())
                .map_err(|e| user_error!("failed to open file {}: {e}", file.path().display()))?;
            let reader: Box<dyn Read> = Box::new(reader);

            // Omit the base directory (which is common to all paths).
            let path = file.path().strip_prefix(&self.cmd.input_directory).unwrap();
            let path = path
                .to_str()
                .with_context(|| format!("non-unicode file path: {}", path.display()))?;

            entries.insert(String::from(path), (len, reader));
        }

        let output_file = File::create(&self.cmd.output_file).with_context(|| {
            format!("failed to create file: {}", self.cmd.output_file.display())
        })?;
        far::write(output_file, entries).with_context(|| {
            format!("failed to write FAR file: {}", self.cmd.output_file.display())
        })?;

        Ok(())
    }
}
