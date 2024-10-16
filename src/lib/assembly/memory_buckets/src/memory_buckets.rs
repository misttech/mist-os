// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use assembly_util::{read_config, write_json_file};
use camino::Utf8PathBuf;
use serde::Serialize;
use std::path::Path;

/// A builder that produces a single memory buckets config file by merging
/// multiple memory buckets inputs from the platform, board, or product.
#[derive(Debug, Default, Serialize)]
pub struct MemoryBuckets {
    /// The collection of all buckets.
    buckets: Vec<serde_json::Value>,
}

impl MemoryBuckets {
    /// Add all the buckets from all the files in `buckets`.
    pub fn add_buckets(&mut self, buckets: &Vec<Utf8PathBuf>) -> Result<()> {
        for bucket_path in buckets {
            let mut buckets_values: Vec<serde_json::Value> = read_config(bucket_path)?;
            self.buckets.append(&mut buckets_values);
        }
        Ok(())
    }

    /// Write the final merged config into `output`.
    pub fn write(&self, output: impl AsRef<Path>) -> Result<()> {
        write_json_file(output, &self.buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use tempfile::NamedTempFile;

    #[test]
    fn test_add_buckets() {
        // Write the test inputs.
        let input1 = json!([
            {
                "name": "bucket name 1",
                "process": "",
                "vmo": "",
                "event_code": 1
            },
        ]);
        let input2 = json!([
            {
                "name": "bucket name 2",
                "process": "",
                "vmo": "",
                "event_code": 2
            },
        ]);
        let input3 = json!([
            {
                "name": "bucket name 3",
                "process": "",
                "vmo": "",
                "event_code": 3
            },
        ]);
        let buckets_input1 = NamedTempFile::new().unwrap();
        let buckets_input2 = NamedTempFile::new().unwrap();
        let buckets_input3 = NamedTempFile::new().unwrap();
        serde_json::ser::to_writer(&buckets_input1, &input1).unwrap();
        serde_json::ser::to_writer(&buckets_input2, &input2).unwrap();
        serde_json::ser::to_writer(&buckets_input3, &input3).unwrap();
        let bucket_group1 = vec![
            Utf8PathBuf::from_path_buf(buckets_input1.path().to_path_buf()).unwrap(),
            Utf8PathBuf::from_path_buf(buckets_input2.path().to_path_buf()).unwrap(),
        ];
        let bucket_group2 =
            vec![Utf8PathBuf::from_path_buf(buckets_input3.path().to_path_buf()).unwrap()];

        // Run the builder.
        let mut buckets = MemoryBuckets::default();
        buckets.add_buckets(&bucket_group1).unwrap();
        buckets.add_buckets(&bucket_group2).unwrap();
        let buckets_output = NamedTempFile::new().unwrap();
        buckets.write(&buckets_output.path()).unwrap();

        // Assert the result.
        let buckets_output = File::open(buckets_output).unwrap();
        let output: serde_json::Value = serde_json::from_reader(&buckets_output).unwrap();
        let expected_output = json!([
            {
                "name": "bucket name 1",
                "process": "",
                "vmo": "",
                "event_code": 1
            },
            {
                "name": "bucket name 2",
                "process": "",
                "vmo": "",
                "event_code": 2
            },
            {
                "name": "bucket name 3",
                "process": "",
                "vmo": "",
                "event_code": 3
            },
        ]);
        assert_eq!(output, expected_output);
    }
}
