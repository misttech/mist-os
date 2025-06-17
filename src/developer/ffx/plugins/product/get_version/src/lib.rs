// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! FFX plugin for the product version of a Product Bundle.

mod unique_release_info;

use crate::unique_release_info::{from_release_info, UniqueReleaseInfo};

use anyhow::{Context, Result};
use assembly_partitions_config::Slot;
use assembly_release_info::SystemReleaseInfo;
use async_trait::async_trait;
use ffx_writer::{MachineWriter, ToolIO as _};
use fho::{FfxContext, FfxMain, FfxTool};
use product_bundle::{ProductBundle, ProductBundleV2};
use std::collections::BTreeMap;

mod args;
pub use args::GetVersionCommand;

/// This plugin will get the the product version of a Product Bundle.
#[derive(FfxTool)]
pub struct PbGetVersionTool {
    #[command]
    cmd: GetVersionCommand,
}

#[async_trait(?Send)]
impl FfxMain for PbGetVersionTool {
    type Writer = MachineWriter<Vec<UniqueReleaseInfo>>;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let product_bundle = ProductBundle::try_load_from(self.cmd.product_bundle)
            .context("Failed to load product bundle")?;

        let (version, release_info) = match product_bundle {
            ProductBundle::V2(pb) => extract_release_info_v2(&pb),
        };

        writer.machine(&release_info).bug()?;
        writer.line(&version).bug()?;

        Ok(())
    }
}

fn extract_release_info_v2(pb: &ProductBundleV2) -> (String, Vec<UniqueReleaseInfo>) {
    let pb_info = pb.release_info.clone().unwrap();
    let mut btree: BTreeMap<UniqueReleaseInfo, Vec<Slot>> = BTreeMap::new();

    // If an existing UniqueReleaseInfo exists in the BTreeMap with the same
    // fields (excluding the Slot list), merge the two slot vectors into the
    // "value" for that entry in the BTreeMap.
    //
    // The Slot field in each UniqueReleaseInfo entry will be overwritten with
    // the contents of the BTreeMap values down below.
    let mut push_or_merge = |new_info: UniqueReleaseInfo| {
        let new_info_slot = new_info.slot.clone();
        btree
            .entry(new_info)
            .and_modify(|slot_vec| slot_vec.extend(&new_info_slot))
            .or_insert(new_info_slot);
    };

    let mut add_flat_system_info = |info: SystemReleaseInfo, slot: Slot| {
        push_or_merge(from_release_info(&info.platform, &slot));

        let product = info.product;
        push_or_merge(from_release_info(&product.info, &slot));
        product.pibs.iter().for_each(|pib| push_or_merge(from_release_info(pib, &slot)));

        let board = info.board;
        push_or_merge(from_release_info(&board.info, &slot));
        board.bib_sets.iter().for_each(|bib_set| push_or_merge(from_release_info(bib_set, &slot)));
    };

    // Push release information for the systems inside the PB.
    pb_info.system_a.map(|system| add_flat_system_info(system, Slot::A));
    pb_info.system_b.map(|system| add_flat_system_info(system, Slot::B));
    pb_info.system_r.map(|system| add_flat_system_info(system, Slot::R));

    // Convert the btreemap to a vector of keys, and set each UniqueReleaseInfo
    // slot field equal to the corresponding "value" in the BTreeMap.
    let mut flat: Vec<UniqueReleaseInfo> = btree.clone().into_keys().collect();
    for info in flat.iter_mut() {
        info.slot = btree[info].clone();
    }

    (pb.product_version.clone(), flat)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::unique_release_info::UniqueReleaseInfo;

    use assembly_partitions_config::{PartitionsConfig, Slot};
    use assembly_release_info::{
        BoardReleaseInfo, ProductBundleReleaseInfo, ProductReleaseInfo, ReleaseInfo,
        SystemReleaseInfo,
    };
    use ffx_writer::{Format, MachineWriter, TestBuffers};

    fn generate_test_product_bundle() -> ProductBundleV2 {
        ProductBundleV2 {
            product_name: "fake_name".to_string(),
            product_version: "fake_version".to_string(),
            sdk_version: "fake_sdk_version".to_string(),
            partitions: PartitionsConfig::default(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
            release_info: Some(ProductBundleReleaseInfo {
                name: "fake_name".to_string(),
                version: "fake_version".to_string(),
                sdk_version: "fake_version".to_string(),
                system_a: Some(SystemReleaseInfo {
                    platform: ReleaseInfo {
                        name: "fake_platform".to_string(),
                        repository: "fake_repository_for_platform".to_string(),
                        version: "fake_version_for_platform".to_string(),
                    },
                    product: ProductReleaseInfo {
                        info: ReleaseInfo {
                            name: "fake_product".to_string(),
                            repository: "fake_repository_for_product".to_string(),
                            version: "fake_version_for_product".to_string(),
                        },
                        pibs: vec![
                            ReleaseInfo {
                                name: "fake_example_pib_1".to_string(),
                                repository: "fake_repository_for_product".to_string(),
                                version: "fake_version_for_product".to_string(),
                            },
                            ReleaseInfo {
                                name: "fake_example_pib_2".to_string(),
                                repository: "fake_repository_for_product".to_string(),
                                version: "fake_version_for_product".to_string(),
                            },
                        ],
                    },
                    board: BoardReleaseInfo {
                        info: ReleaseInfo {
                            name: "fake_board".to_string(),
                            repository: "fake_repository_for_board".to_string(),
                            version: "fake_version_for_board".to_string(),
                        },
                        bib_sets: vec![
                            ReleaseInfo {
                                name: "fake_example_bib_set_1".to_string(),
                                repository: "fake_repository_for_board".to_string(),
                                version: "fake_version_for_board".to_string(),
                            },
                            ReleaseInfo {
                                name: "fake_example_bib_set_2".to_string(),
                                repository: "fake_repository_for_board".to_string(),
                                version: "fake_version_for_board".to_string(),
                            },
                            ReleaseInfo {
                                name: "fake_example_bib_set_3".to_string(),
                                repository: "fake_repository_for_board".to_string(),
                                version: "fake_version_for_board".to_string(),
                            },
                        ],
                    },
                }),
                system_b: None,
                system_r: Some(SystemReleaseInfo {
                    platform: ReleaseInfo {
                        name: "fake_platform".to_string(),
                        repository: "fake_repository_for_platform".to_string(),
                        version: "fake_version_for_platform".to_string(),
                    },
                    // This is the same product as "fake_product" in Slot A (system_a) defined
                    // above. We expect both product entries to be combined into a single
                    // UniqueReleaseInfo entry in the result.
                    product: ProductReleaseInfo {
                        info: ReleaseInfo {
                            name: "fake_product".to_string(),
                            repository: "fake_repository_for_product".to_string(),
                            version: "fake_version_for_product".to_string(),
                        },
                        pibs: vec![],
                    },
                    // This is the same board as "fake_board" in Slot A (system_a) defined
                    // above. We expect both board entries to be combined into a single
                    // UniqueReleaseInfo entry in the result.
                    board: BoardReleaseInfo {
                        info: ReleaseInfo {
                            name: "fake_board".to_string(),
                            repository: "fake_repository_for_board".to_string(),
                            version: "fake_version_for_board".to_string(),
                        },
                        bib_sets: vec![],
                    },
                }),
            }),
        }
    }

    fn generate_test_pb_flat_unique_release_info_vector() -> Vec<UniqueReleaseInfo> {
        vec![
            UniqueReleaseInfo {
                name: "fake_platform".to_string(),
                version: "fake_version_for_platform".to_string(),
                repository: "fake_repository_for_platform".to_string(),
                slot: vec![Slot::A, Slot::R],
            },
            UniqueReleaseInfo {
                name: "fake_product".to_string(),
                version: "fake_version_for_product".to_string(),
                repository: "fake_repository_for_product".to_string(),
                slot: vec![Slot::A, Slot::R],
            },
            UniqueReleaseInfo {
                name: "fake_example_pib_1".to_string(),
                version: "fake_version_for_product".to_string(),
                repository: "fake_repository_for_product".to_string(),
                slot: vec![Slot::A],
            },
            UniqueReleaseInfo {
                name: "fake_example_pib_2".to_string(),
                version: "fake_version_for_product".to_string(),
                repository: "fake_repository_for_product".to_string(),
                slot: vec![Slot::A],
            },
            UniqueReleaseInfo {
                name: "fake_board".to_string(),
                version: "fake_version_for_board".to_string(),
                repository: "fake_repository_for_board".to_string(),
                slot: vec![Slot::A, Slot::R],
            },
            UniqueReleaseInfo {
                name: "fake_example_bib_set_1".to_string(),
                version: "fake_version_for_board".to_string(),
                repository: "fake_repository_for_board".to_string(),
                slot: vec![Slot::A],
            },
            UniqueReleaseInfo {
                name: "fake_example_bib_set_2".to_string(),
                version: "fake_version_for_board".to_string(),
                repository: "fake_repository_for_board".to_string(),
                slot: vec![Slot::A],
            },
            UniqueReleaseInfo {
                name: "fake_example_bib_set_3".to_string(),
                version: "fake_version_for_board".to_string(),
                repository: "fake_repository_for_board".to_string(),
                slot: vec![Slot::A],
            },
        ]
    }

    #[test]
    fn test_version_string_writer() {
        let pb = generate_test_product_bundle();
        let (version, release_info) = extract_release_info_v2(&pb);
        assert_eq!("fake_version", version);

        let test_buffers = TestBuffers::default();
        let mut writer: MachineWriter<&str> = MachineWriter::new_test(None, &test_buffers);

        // Write the version string.
        let res = writer.line(version);
        assert!(res.is_ok());

        // Write the json structure. This should be suppressed.
        let machine = serde_json::to_string(&release_info).unwrap();
        let res = writer.machine(&&machine.as_str());
        assert!(res.is_ok());

        let (stdout, stderr) = test_buffers.into_strings();
        assert_eq!("fake_version\n", stdout);
        assert_eq!("", stderr);
    }

    #[test]
    fn test_machine_json_writer() {
        let mut expected = generate_test_pb_flat_unique_release_info_vector();
        let pb = generate_test_product_bundle();
        let (version, mut release_info) = extract_release_info_v2(&pb);

        expected.sort();
        release_info.sort();
        assert_eq!(expected, release_info);

        let test_buffers = TestBuffers::default();
        let mut writer: MachineWriter<&str> =
            MachineWriter::new_test(Some(Format::Json), &test_buffers);

        // Write the version string. This should be suppressed.
        let res = writer.line(version);
        assert!(res.is_ok());

        // Write the json structure.
        let machine = serde_json::to_string(&release_info).unwrap();
        let res = writer.machine(&&machine.as_str());
        assert!(res.is_ok());

        let (stdout, stderr) = test_buffers.into_strings();

        // The stdout string needs to be un-escaped, which can be done using serde.
        let stdout: String = serde_json::from_str(&stdout).unwrap();
        let mut actual: Vec<UniqueReleaseInfo> = serde_json::from_str(&stdout).unwrap();

        actual.sort();
        assert_eq!(expected, actual);
        assert_eq!(stderr, "");
    }
}
