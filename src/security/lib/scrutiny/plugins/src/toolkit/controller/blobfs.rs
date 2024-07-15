// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use scrutiny_utils::blobfs::*;
use scrutiny_utils::fs::tempdir;
use scrutiny_utils::io::TryClonableBufReaderFile;
use serde_json::json;
use serde_json::value::Value;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

pub struct BlobFsExtractController {}

impl BlobFsExtractController {
    pub fn extract(input: PathBuf, output: PathBuf) -> Result<Value> {
        let tmp_dir = tempdir::<PathBuf>(None)
            .context("Failed to create temporary directory for blobfs extract controller")?;

        let blobfs_file = File::open(&input)
            .map_err(|err| anyhow!("Failed to open blbofs archive {:?}: {}", input, err))?;
        let reader: TryClonableBufReaderFile = BufReader::new(blobfs_file).into();
        let mut reader =
            BlobFsReaderBuilder::new().archive(reader)?.tmp_dir(Arc::new(tmp_dir))?.build()?;

        fs::create_dir_all(&output)?;

        // Clone paths out of `reader` to avoid simultaneous immutable borrow from
        // `reader.blob_paths()` and mutable borrow from `reader.read_blob()`.
        let blob_paths: Vec<PathBuf> = reader.blob_paths().map(PathBuf::clone).collect();
        for blob_path in blob_paths.into_iter() {
            let path = output.join(&blob_path);
            let mut file = File::create(path)?;
            let mut blob = reader.open(&blob_path)?;
            std::io::copy(&mut blob, &mut file)?;
        }
        Ok(json!({"status": "ok"}))
    }
}
