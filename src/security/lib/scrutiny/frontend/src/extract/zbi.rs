// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use log::info;
use scrutiny_utils::blobfs::*;
use scrutiny_utils::bootfs::*;
use scrutiny_utils::fs::tempdir;
use scrutiny_utils::fvm::*;
use scrutiny_utils::zbi::*;
use serde_json::json;
use serde_json::value::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::prelude::*;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ZbiExtractController {}

impl ZbiExtractController {
    pub fn extract(input: PathBuf, output: PathBuf) -> Result<Value> {
        let mut zbi_file = File::open(input)?;
        let mut zbi_buffer = Vec::new();
        zbi_file.read_to_end(&mut zbi_buffer)?;
        let mut reader = ZbiReader::new(zbi_buffer);
        let zbi_sections = reader.parse()?;

        fs::create_dir_all(&output)?;
        let mut sections_dir = output.clone();
        sections_dir.push("sections");
        fs::create_dir_all(&sections_dir)?;
        let mut section_count = HashMap::new();
        for section in zbi_sections.iter() {
            let section_str = format!("{:?}", section.section_type).to_lowercase();
            let section_name = if let Some(count) = section_count.get_mut(&section.section_type) {
                *count += 1;
                format!("{}.{}.blk", section_str, count)
            } else {
                section_count.insert(section.section_type, 0);
                format!("{}.blk", section_str)
            };
            let mut path = sections_dir.clone();
            path.push(section_name);
            let mut file = File::create(path)?;
            file.write_all(&section.buffer)?;

            // Expand bootfs into its own folder as well.
            if section.section_type == ZbiType::StorageBootfs {
                let mut bootfs_dir = output.clone();
                bootfs_dir.push("bootfs");
                fs::create_dir_all(bootfs_dir.clone())?;
                let mut bootfs_reader = BootfsReader::new(section.buffer.clone());
                let bootfs_files = bootfs_reader.parse()?;
                for (file_name, data) in bootfs_files.iter() {
                    let mut bootfs_file_path = bootfs_dir.clone();
                    bootfs_file_path.push(file_name);
                    if let Some(parent_dir) = bootfs_file_path.as_path().parent() {
                        fs::create_dir_all(parent_dir)?;
                    }
                    let mut bootfs_file = File::create(bootfs_file_path)?;
                    bootfs_file.write_all(&data)?;
                }
            } else if section.section_type == ZbiType::StorageRamdisk {
                info!("Attempting to load FvmPartitions");
                let mut fvm_reader = FvmReader::new(section.buffer.clone());
                if let Ok(fvm_partitions) = fvm_reader.parse() {
                    info!(total = fvm_partitions.len(); "Extracting Partitions in StorageRamdisk");
                    let mut fvm_dir = output.clone();
                    fvm_dir.push("fvm");
                    fs::create_dir_all(fvm_dir.clone())?;

                    let mut partition_count = HashMap::<FvmPartitionType, u64>::new();
                    for partition in fvm_partitions.iter() {
                        let file_name = if let Some(count) =
                            partition_count.get_mut(&partition.partition_type)
                        {
                            *count += 1;
                            format!("{}.{}.blk", partition.partition_type, count)
                        } else {
                            section_count.insert(section.section_type, 0);
                            format!("{}.blk", partition.partition_type)
                        };
                        let mut fvm_partition_path = fvm_dir.clone();
                        fvm_partition_path.push(file_name);
                        let mut fvm_file = File::create(fvm_partition_path)?;
                        fvm_file.write_all(&partition.buffer)?;

                        // Write out the blobfs data.
                        if partition.partition_type == FvmPartitionType::BlobFs {
                            info!("Extracting BlobFs FVM partiion");
                            let blobfs_dir = fvm_dir.join("blobfs");
                            fs::create_dir_all(&blobfs_dir)?;

                            let tmp_dir = tempdir::<PathBuf>(None).context(
                                "Failed to create temporary directory for zbi extract controller",
                            )?;
                            let mut reader = BlobFsReaderBuilder::new()
                                .archive(Cursor::new(partition.buffer.clone()))?
                                .tmp_dir(Arc::new(tmp_dir))?
                                .build()?;

                            // Clone paths out of `reader` to avoid simultaneous immutable borrow
                            // from `reader.blob_paths()` and mutable borrow from
                            // `reader.read_blob()`.
                            #[allow(clippy::needless_collect)] // collecting avoids lifetime issues
                            let blob_paths: Vec<PathBuf> =
                                reader.blob_paths().map(PathBuf::clone).collect();
                            for blob_path in blob_paths.into_iter() {
                                let path = blobfs_dir.join(&blob_path);
                                let mut file = File::create(path)?;
                                let mut blob = reader.open(&blob_path)?;
                                std::io::copy(&mut blob, &mut file)?;
                            }
                        }
                    }
                } else {
                    info!("No FvmPartitions found in StorageRamdisk");
                }
            }
        }

        Ok(json!({"status": "ok"}))
    }
}
