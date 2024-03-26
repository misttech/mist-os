// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Partition, PartitionsConfig, Slot};
use anyhow::{Context, Result};
use assembly_manifest::Image;
use assembly_util::write_json_file;
use camino::Utf8PathBuf;
use serde_json::json;
use std::collections::BTreeMap;
use url::Url;

/// The type of the image used to correlate a partition with an image in the
/// assembly manifest.
#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub enum ImageType {
    /// Zircon Boot Image.
    ZBI,
    /// Verified Boot Metadata.
    VBMeta,
    /// Fuchsia Volume Manager.
    FVM,
    /// Fuchsia Filesystem.
    Fxfs,
}

/// A pair of an image path mapped to a specific partition.
#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct PartitionAndImage {
    /// The partition on hardware.
    pub partition: Partition,
    /// The path to the image to place in the partition.
    pub path: Utf8PathBuf,
}

/// A tool that can map sets of images into hardware partitions.
pub struct PartitionImageMapper {
    partitions: PartitionsConfig,
    images: BTreeMap<Slot, BTreeMap<ImageType, Utf8PathBuf>>,
}

impl PartitionImageMapper {
    /// Construct a new mapper that targets the `partitions`.
    pub fn new(partitions: PartitionsConfig) -> Self {
        Self { partitions, images: BTreeMap::new() }
    }

    /// Map a set images that are intended for a specific slot to partitions.
    pub fn map_images_to_slot(&mut self, images: &Vec<Image>, slot: Slot) {
        let slot_entry = self.images.entry(slot).or_insert(BTreeMap::new());
        for image in images.iter() {
            match image {
                Image::ZBI { path, .. } => {
                    slot_entry.insert(ImageType::ZBI, path.clone());
                }
                Image::VBMeta(path) => {
                    slot_entry.insert(ImageType::VBMeta, path.clone());
                }
                Image::FVMFastboot(path) => {
                    if let Slot::R = slot {
                        // Recovery should not include a separate FVM, because it is embedded into the
                        // ZBI as a ramdisk.
                        continue;
                    } else {
                        slot_entry.insert(ImageType::FVM, path.clone());
                    }
                }
                Image::FxfsSparse { path, .. } => {
                    if let Slot::R = slot {
                        // Recovery should not include a separate FVM, because it is embedded into the
                        // ZBI as a ramdisk.
                        continue;
                    } else {
                        slot_entry.insert(ImageType::Fxfs, path.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// Return the mappings of images to partitions.
    pub fn map(&self) -> Vec<PartitionAndImage> {
        self.map_internal(false)
    }

    /// Return the mappings of images to partitions.
    /// Use the R slot images for all partitions.
    pub fn map_recovery_on_all_slots(&self) -> Vec<PartitionAndImage> {
        self.map_internal(true)
    }

    fn map_internal(&self, recovery_on_all_slots: bool) -> Vec<PartitionAndImage> {
        let mut mapped_partitions = vec![];

        // Assign the images to particular partitions.
        for p in &self.partitions.partitions {
            let (image_type, slot) = match &p {
                Partition::ZBI { slot, .. } => (ImageType::ZBI, slot),
                Partition::VBMeta { slot, .. } => (ImageType::VBMeta, slot),

                // Arbitrarily, take the fvm from the slot A system.
                Partition::FVM { .. } => (ImageType::FVM, &Slot::A),

                // Arbitrarily, take Fxfs from the slot A system.
                Partition::Fxfs { .. } => (ImageType::Fxfs, &Slot::A),
            };

            if let Some(slot) = match recovery_on_all_slots {
                // If this is recovery mode, then fill every partition with images from the slot R
                // system.
                true => self.images.get(&Slot::R),
                false => self.images.get(slot),
            } {
                if let Some(path) = slot.get(&image_type) {
                    mapped_partitions
                        .push(PartitionAndImage { partition: p.clone(), path: path.clone() });
                }
            }
        }

        mapped_partitions
    }

    /// Generate a size report that indicates the partition size and the size of the image mapped
    /// to it.
    pub fn generate_gerrit_size_report(
        &self,
        report_path: &Utf8PathBuf,
        prefix: &String,
    ) -> Result<()> {
        let mut report = BTreeMap::new();

        let mappings = self.map();
        for mapping in mappings {
            let PartitionAndImage { partition, path } = mapping;
            if let (Some(size), name) = (partition.size(), partition.name()) {
                let metadata = std::fs::metadata(path).context("Getting image metadata")?;
                let measured_size = metadata.len();
                report.insert(format!("{}-{}", prefix, name), json!(measured_size));
                report.insert(format!("{}-{}.budget", prefix, name), json!(size));
                report.insert(format!("{}-{}.creepBudget", prefix, name), json!(200 * 1024));
                let url = Url::parse_with_params(
                    "http://go/fuchsia-size-stats/single_component/",
                    &[("f", format!("component:in:{}-{}", prefix, name))],
                )?;
                report.insert(format!("{}-{}.owner", prefix, name), json!(url.as_str()));
            }
        }

        write_json_file(&report_path, &report).context("Writing gerrit size report")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_manifest::{AssemblyManifest, BlobfsContents};
    use assembly_util::read_config;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn test_map_fvm() {
        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                Partition::FVM { name: "fvm".into(), size: None },
                Partition::Fxfs { name: "fxfs".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let images_b = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/b/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/b/fuchsia.vbmeta".into()),
                Image::FVM("path/to/b/fvm.blk".into()),
                Image::FVMFastboot("path/to/b/fvm.fastboot.blk".into()),
            ],
        };
        let images_r = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/r/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/r/fuchsia.vbmeta".into()),
                Image::FVM("path/to/r/fvm.blk".into()),
                Image::FVMFastboot("path/to/r/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.map_images_to_slot(&images_b.images, Slot::B);
        mapper.map_images_to_slot(&images_r.images, Slot::R);

        let expected = vec![
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                path: "path/to/b/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                path: "path/to/b/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::FVM { name: "fvm".into(), size: None },
                path: "path/to/a/fvm.fastboot.blk".into(),
            },
        ];
        assert_eq!(expected, mapper.map());
    }

    #[test]
    fn test_map_recovery() {
        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                Partition::FVM { name: "fvm".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let images_b = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/b/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/b/fuchsia.vbmeta".into()),
                Image::FVM("path/to/b/fvm.blk".into()),
                Image::FVMFastboot("path/to/b/fvm.fastboot.blk".into()),
            ],
        };
        let images_r = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/r/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/r/fuchsia.vbmeta".into()),
                Image::FVM("path/to/r/fvm.blk".into()),
                Image::FVMFastboot("path/to/r/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.map_images_to_slot(&images_b.images, Slot::B);
        mapper.map_images_to_slot(&images_r.images, Slot::R);

        let expected = vec![
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
        ];
        assert_eq!(expected, mapper.map_recovery_on_all_slots());
    }

    #[test]
    fn test_map_fxfs() {
        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                Partition::FVM { name: "fvm".into(), size: None },
                Partition::Fxfs { name: "fxfs".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FxfsSparse {
                    path: "path/to/a/fxfs.blk".into(),
                    contents: BlobfsContents::default(),
                },
            ],
        };
        let images_b = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/b/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/b/fuchsia.vbmeta".into()),
                Image::FxfsSparse {
                    path: "path/to/b/fxfs.blk".into(),
                    contents: BlobfsContents::default(),
                },
            ],
        };
        let images_r = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/r/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/r/fuchsia.vbmeta".into()),
                Image::FxfsSparse {
                    path: "path/to/r/fxfs.blk".into(),
                    contents: BlobfsContents::default(),
                },
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.map_images_to_slot(&images_b.images, Slot::B);
        mapper.map_images_to_slot(&images_r.images, Slot::R);

        let expected = vec![
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                path: "path/to/b/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                path: "path/to/b/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::Fxfs { name: "fxfs".into(), size: None },
                path: "path/to/a/fxfs.blk".into(),
            },
        ];
        assert_eq!(expected, mapper.map());
    }

    #[test]
    fn test_no_slots() {
        let partitions = PartitionsConfig { partitions: vec![], ..Default::default() };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        assert!(mapper.map().is_empty());
    }

    #[test]
    fn test_slot_a_only() {
        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                Partition::FVM { name: "fvm".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let images_b = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/b/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/b/fuchsia.vbmeta".into()),
                Image::FVM("path/to/b/fvm.blk".into()),
                Image::FVMFastboot("path/to/b/fvm.fastboot.blk".into()),
            ],
        };
        let images_r = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/r/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/r/fuchsia.vbmeta".into()),
                Image::FVM("path/to/r/fvm.blk".into()),
                Image::FVMFastboot("path/to/r/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.map_images_to_slot(&images_b.images, Slot::B);
        mapper.map_images_to_slot(&images_r.images, Slot::R);

        let expected = vec![
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::FVM { name: "fvm".into(), size: None },
                path: "path/to/a/fvm.fastboot.blk".into(),
            },
        ];
        assert_eq!(expected, mapper.map());
    }

    #[test]
    fn test_missing_slot() {
        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                Partition::ZBI { name: "zbi_b".into(), slot: Slot::B, size: None },
                Partition::VBMeta { name: "vbmeta_b".into(), slot: Slot::B, size: None },
                Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                Partition::FVM { name: "fvm".into(), size: None },
                Partition::Fxfs { name: "fxfs".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/a/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/a/fuchsia.vbmeta".into()),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let images_r = AssemblyManifest {
            images: vec![
                Image::ZBI { path: "path/to/r/fuchsia.zbi".into(), signed: false },
                Image::VBMeta("path/to/r/fuchsia.vbmeta".into()),
                Image::FVM("path/to/r/fvm.blk".into()),
                Image::FVMFastboot("path/to/r/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.map_images_to_slot(&images_r.images, Slot::R);

        let expected = vec![
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: None },
                path: "path/to/a/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::ZBI { name: "zbi_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.zbi".into(),
            },
            PartitionAndImage {
                partition: Partition::VBMeta { name: "vbmeta_r".into(), slot: Slot::R, size: None },
                path: "path/to/r/fuchsia.vbmeta".into(),
            },
            PartitionAndImage {
                partition: Partition::FVM { name: "fvm".into(), size: None },
                path: "path/to/a/fvm.fastboot.blk".into(),
            },
        ];
        assert_eq!(expected, mapper.map());
    }

    #[test]
    fn test_size_report() {
        let temp_dir = TempDir::new().unwrap();
        let temp_dir_path = Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf()).unwrap();
        let zbi_path = temp_dir_path.join("zbi");
        let vbmeta_path = temp_dir_path.join("vbmeta");
        let size_report_path = temp_dir_path.join("report.json");

        std::fs::write(&zbi_path, "zbi").unwrap();
        std::fs::write(&vbmeta_path, "vbmeta").unwrap();

        let partitions = PartitionsConfig {
            partitions: vec![
                Partition::ZBI { name: "zbi_a".into(), slot: Slot::A, size: Some(100) },
                Partition::VBMeta { name: "vbmeta_a".into(), slot: Slot::A, size: Some(200) },
                Partition::FVM { name: "fvm".into(), size: None },
            ],
            ..Default::default()
        };
        let images_a = AssemblyManifest {
            images: vec![
                Image::ZBI { path: zbi_path, signed: false },
                Image::VBMeta(vbmeta_path),
                Image::FVM("path/to/a/fvm.blk".into()),
                Image::FVMFastboot("path/to/a/fvm.fastboot.blk".into()),
            ],
        };
        let mut mapper = PartitionImageMapper::new(partitions);
        mapper.map_images_to_slot(&images_a.images, Slot::A);
        mapper.generate_gerrit_size_report(&size_report_path, &"prefix".to_string()).unwrap();

        let result: serde_json::Value = read_config(&size_report_path).unwrap();
        let expected = serde_json::json!({
            "prefix-vbmeta_a": 6,
            "prefix-vbmeta_a.budget": 200,
            "prefix-vbmeta_a.creepBudget": 200 * 1024,
            "prefix-vbmeta_a.owner": "http://go/fuchsia-size-stats/single_component/?f=component%3Ain%3Aprefix-vbmeta_a",
            "prefix-zbi_a": 3,
            "prefix-zbi_a.budget": 100,
            "prefix-zbi_a.creepBudget": 200 * 1024,
            "prefix-zbi_a.owner": "http://go/fuchsia-size-stats/single_component/?f=component%3Ain%3Aprefix-zbi_a",
        });
        assert_eq!(expected, result);
    }
}
