// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use ffx_package_far_extract_args::ExtractCommand;
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool};
use fuchsia_archive as far;
use std::fs::{self, File};

#[derive(FfxTool)]
pub struct ExtractTool {
    #[command]
    pub cmd: ExtractCommand,
}

fho::embedded_plugin!(ExtractTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for ExtractTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: <Self as fho::FfxMain>::Writer) -> fho::Result<()> {
        self.cmd_extract().await.map_err(Into::into)
    }
}

impl ExtractTool {
    pub async fn cmd_extract(&self) -> Result<()> {
        let far_file = File::open(&self.cmd.far_file)
            .with_context(|| format!("failed to open file: {}", self.cmd.far_file.display()))?;
        let mut reader = far::Utf8Reader::new(far_file).with_context(|| {
            format!("failed to parse FAR file: {}", self.cmd.far_file.display())
        })?;

        // If no paths are given on the command line, extract everything.
        let paths = if self.cmd.paths.is_empty() {
            reader.list().map(|entry| Utf8PathBuf::from(entry.path())).collect()
        } else {
            self.cmd.paths.clone()
        };

        for path in paths {
            // Note that this implicitly does some validation on `path`.
            //
            // E.g., it can't be:
            // * empty,
            // * start or end with "/",
            // * contain "." or ".." as a segment.
            let bytes = reader.read_file(path.as_str()).with_context(|| {
                format!("failed to read {path} from {}", self.cmd.far_file.display())
            })?;

            let out_path = self.cmd.output_dir.join(path);
            let parent = out_path.parent().expect("`path` must be non-empty");
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
            fs::write(&out_path, &bytes)?;

            if self.cmd.verbose {
                println!("{}", out_path.display());
            }
        }

        Ok(())
    }
}
