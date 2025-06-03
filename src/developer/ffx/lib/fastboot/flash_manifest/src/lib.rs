// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::v1::FlashManifest as FlashManifestV1;
use crate::v2::FlashManifest as FlashManifestV2;
use crate::v3::{Condition, FlashManifest as FlashManifestV3, Partition, Product};
use anyhow::{bail, Context, Error, Result};
use assembly_partitions_config::{PartitionAndImage, PartitionImageMapper, Slot};
use errors::ffx_bail;
use product_bundle::{ProductBundle, ProductBundleV2};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value, Value};
use std::default::Default;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub mod v1;
pub mod v2;
pub mod v3;

pub const UNKNOWN_VERSION: &str = "Unknown flash manifest version";

pub(crate) const OEM_FILE_ERROR_MSG: &str =
    "Unrecognized OEM staged file. Expected comma-separated pair: \"<OEM_COMMAND>,<PATH_TO_FILE>\"";

#[derive()]
pub struct ManifestParams {
    pub manifest: Option<PathBuf>,
    pub product: String,
    pub product_bundle: Option<String>,
    pub oem_stage: Vec<OemFile>,
    pub no_bootloader_reboot: bool,
    pub skip_verify: bool,
    pub op: Command,
    pub flash_timeout_rate_mb_per_second: u64,
    pub flash_min_timeout_seconds: u64,
}

impl Default for ManifestParams {
    fn default() -> Self {
        Self {
            manifest: None,
            product: "fuchsia".to_string(),
            product_bundle: None,
            oem_stage: vec![],
            no_bootloader_reboot: false,
            skip_verify: false,
            op: Command::Flash,
            flash_timeout_rate_mb_per_second: 5000,
            flash_min_timeout_seconds: 200,
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Flash,
    Unlock(UnlockParams),
    Boot(BootParams),
}

#[derive(Debug)]
pub struct UnlockParams {
    pub cred: Option<String>,
    pub force: bool,
}

#[derive(Debug)]
pub struct BootParams {
    pub zbi: Option<String>,
    pub vbmeta: Option<String>,
    pub slot: String,
}

#[derive(Default, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OemFile(String, String);

impl OemFile {
    pub fn new(command: String, path: String) -> Self {
        Self(command, path)
    }

    pub fn command(&self) -> &str {
        self.0.as_str()
    }

    pub fn file(&self) -> &str {
        self.1.as_str()
    }
}

impl std::str::FromStr for OemFile {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.len() == 0 {
            bail!(OEM_FILE_ERROR_MSG);
        }

        let splits: Vec<&str> = s.split(",").collect();

        if splits.len() != 2 {
            bail!(OEM_FILE_ERROR_MSG);
        }

        let file = Path::new(splits[1]);
        if !file.exists() {
            bail!("File does not exist: {}", splits[1]);
        }

        Ok(Self(splits[0].to_string(), file.to_string_lossy().to_string()))
    }
}

pub enum FlashManifestVersion {
    V1(FlashManifestV1),
    V2(FlashManifestV2),
    V3(FlashManifestV3),
}

impl FlashManifestVersion {
    pub fn write<W: Write>(&self, writer: W) -> Result<()> {
        let manifest = match &self {
            FlashManifestVersion::V1(manifest) => {
                ManifestFile { version: 1, manifest: to_value(manifest)? }
            }
            FlashManifestVersion::V2(manifest) => {
                ManifestFile { version: 2, manifest: to_value(manifest)? }
            }
            FlashManifestVersion::V3(manifest) => {
                ManifestFile { version: 3, manifest: to_value(manifest)? }
            }
        };
        serde_json::to_writer_pretty(writer, &manifest).context("writing flash manifest")
    }

    pub fn load<R: Read>(reader: R) -> Result<Self> {
        let value: Value = serde_json::from_reader::<R, Value>(reader)
            .context("reading flash manifest from disk")?;
        // GN generated JSON always comes from a list
        let manifest: ManifestFile = match value {
            Value::Array(v) => from_value(v[0].clone())?,
            Value::Object(_) => from_value(value)?,
            _ => ffx_bail!("Could not parse flash manifest."),
        };
        match manifest.version {
            1 => Ok(Self::V1(from_value(manifest.manifest)?)),
            2 => Ok(Self::V2(from_value(manifest.manifest)?)),
            3 => Ok(Self::V3(from_value(manifest.manifest)?)),
            _ => ffx_bail!("{}", UNKNOWN_VERSION),
        }
    }

    pub fn from_product_bundle(product_bundle: &ProductBundle) -> Result<Self> {
        match product_bundle {
            ProductBundle::V2(product_bundle) => Self::from_product_bundle_v2(product_bundle),
        }
    }

    fn from_product_bundle_v2(product_bundle: &ProductBundleV2) -> Result<Self> {
        log::debug!("Begin loading flash manifest from ProductBundleV2: {:#?}", product_bundle);
        // Copy the unlock credentials from the partitions config to the flash manifest.
        let mut credentials = vec![];
        for c in &product_bundle.partitions.unlock_credentials {
            log::debug!("Adding unlock credential: {}", c.to_string());
            credentials.push(c.to_string());
        }

        // Copy the bootloader partitions from the partitions config to the flash manifest.
        let mut bootloader_partitions = vec![];
        for p in &product_bundle.partitions.bootloader_partitions {
            if let Some(name) = &p.name {
                let partition = Partition {
                    name: name.to_string(),
                    path: p.image.to_string(),
                    condition: None,
                };
                log::debug!("Adding bootloader partition: {:#?}", partition);
                bootloader_partitions.push(partition);
            }
        }

        // Copy the bootstrap partitions from the partitions config to the flash manifest.
        let mut bootstrap_partitions = vec![];
        for p in &product_bundle.partitions.bootstrap_partitions {
            let condition = if let Some(c) = &p.condition {
                Some(Condition { variable: c.variable.to_string(), value: c.value.to_string() })
            } else {
                None
            };
            let partition =
                Partition { name: p.name.to_string(), path: p.image.to_string(), condition };
            log::debug!("Adding bootstrap partition: {:#?}", partition);
            bootstrap_partitions.push(partition);
        }
        // Append the bootloader partitions, bootstrapping a device means flashing any initial
        // bootstrap images plus a working bootloader. The bootstrap partitions should always come
        // first as the lowest-level items so that the higher-level bootloader images can depend on
        // bootstrapping being done.
        bootstrap_partitions.extend_from_slice(bootloader_partitions.as_slice());

        // Create a map from slot to available images by name (zbi, vbmeta, fvm).
        let mut image_map = PartitionImageMapper::new(product_bundle.partitions.clone())?;
        if let Some(manifest) = &product_bundle.system_a {
            let slot = Slot::A;
            log::debug!("Mapping images: {:?} to slot: {}", manifest, slot);
            image_map.map_images_to_slot(&manifest, slot)?;
        }
        if let Some(manifest) = &product_bundle.system_b {
            let slot = Slot::B;
            log::debug!("Mapping images: {:?} to slot: {}", manifest, slot);
            image_map.map_images_to_slot(&manifest, slot)?;
        }
        if let Some(manifest) = &product_bundle.system_r {
            let slot = Slot::R;
            log::debug!("Mapping images: {:?} to slot: {}", manifest, slot);
            image_map.map_images_to_slot(&manifest, slot)?;
        }

        // Define the flashable "products".
        let mut products = vec![];
        products.push(Product {
            name: "recovery".into(),
            bootloader_partitions: bootloader_partitions.clone(),
            partitions: get_mapped_partitions(&image_map, /*is_recovery=*/ true),
            oem_files: vec![],
            requires_unlock: false,
        });
        products.push(Product {
            name: "fuchsia_only".into(),
            bootloader_partitions: bootloader_partitions.clone(),
            partitions: get_mapped_partitions(&image_map, /*is_recovery=*/ false),
            oem_files: vec![],
            requires_unlock: false,
        });
        products.push(Product {
            name: "fuchsia".into(),
            bootloader_partitions: bootstrap_partitions.clone(),
            partitions: get_mapped_partitions(&image_map, /*is_recovery=*/ false),
            oem_files: vec![],
            requires_unlock: !product_bundle.partitions.bootstrap_partitions.is_empty(),
        });
        if !product_bundle.partitions.bootstrap_partitions.is_empty() {
            products.push(Product {
                name: "bootstrap".into(),
                bootloader_partitions: bootstrap_partitions.clone(),
                partitions: vec![],
                oem_files: vec![],
                requires_unlock: true,
            });
        }

        // Create the flash manifest.
        let ret = FlashManifestV3 {
            hw_revision: product_bundle.partitions.hardware_revision.clone(),
            credentials,
            products,
        };

        log::debug!("Created FlashManifest: {:#?}", ret);

        Ok(Self::V3(ret))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    manifest: Value,
    version: u64,
}

/// Construct a list of partitions to add to the flash manifest by mapping the partitions to the
/// images. If |is_recovery|, then put the recovery images in every slot.
fn get_mapped_partitions(image_map: &PartitionImageMapper, is_recovery: bool) -> Vec<Partition> {
    let partition_map =
        if is_recovery { image_map.map_recovery_on_all_slots() } else { image_map.map() };
    partition_map
        .iter()
        .map(|PartitionAndImage { partition, path }| Partition {
            name: partition.name().clone(),
            path: path.to_string(),
            condition: None,
        })
        .collect()
}
