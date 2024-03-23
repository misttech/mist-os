// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    common::{
        cmd::{BootParams, Command, ManifestParams},
        prepare, Boot, Flash, Unlock,
    },
    file_resolver::{
        resolvers::{Resolver, ZipArchiveResolver},
        FileResolver,
    },
    manifest::{
        resolvers::{
            ArchiveResolver, FlashManifestResolver, FlashManifestTarResolver, ManifestResolver,
        },
        v1::FlashManifest as FlashManifestV1,
        v2::FlashManifest as FlashManifestV2,
        v3::FlashManifest as FlashManifestV3,
    },
};
use anyhow::{anyhow, bail, Context, Result};
use assembly_manifest::Image as AssemblyManifestImage;
use assembly_partitions_config::{Partition, Slot};
use async_trait::async_trait;
use camino::Utf8Path;
use chrono::Utc;
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use pbms::load_product_bundle;
use sdk_metadata::{ProductBundle, ProductBundleV2};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read, Write},
    path::PathBuf,
};
use termion::{color, style};

pub mod resolvers;
pub mod v1;
pub mod v2;
pub mod v3;

pub const UNKNOWN_VERSION: &str = "Unknown flash manifest version";

#[allow(dead_code)] // TODO(https://fxbug.dev/318827209)
#[derive(Default, Deserialize)]
pub struct Images(Vec<Image>);

#[derive(Default, Deserialize)]
pub struct Image {
    pub name: String,
    pub path: String,
    // Ignore the rest of the fields
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    manifest: Value,
    version: u64,
}

pub enum FlashManifestVersion {
    V1(FlashManifestV1),
    V2(FlashManifestV2),
    V3(FlashManifestV3),
}

/// The type of the image used in the below ImageMap.
#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
enum ImageType {
    ZBI,
    VBMeta,
    FVM,
    Fxfs,
}

/// A map from a slot to the image paths assigned to that slot.
/// This is used during the construction of a manifest from a product bundle.
type ImageMap = BTreeMap<Slot, BTreeMap<ImageType, String>>;

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
            1 => Ok(Self::V1(from_value(manifest.manifest.clone())?)),
            2 => Ok(Self::V2(from_value(manifest.manifest.clone())?)),
            3 => Ok(Self::V3(from_value(manifest.manifest.clone())?)),
            _ => ffx_bail!("{}", UNKNOWN_VERSION),
        }
    }

    pub fn from_product_bundle(product_bundle: &ProductBundle) -> Result<Self> {
        match product_bundle {
            ProductBundle::V2(product_bundle) => Self::from_product_bundle_v2(product_bundle),
        }
    }

    fn from_product_bundle_v2(product_bundle: &ProductBundleV2) -> Result<Self> {
        // Copy the unlock credentials from the partitions config to the flash manifest.
        let mut credentials = vec![];
        for c in &product_bundle.partitions.unlock_credentials {
            credentials.push(c.to_string());
        }

        // Copy the bootloader partitions from the partitions config to the flash manifest.
        let mut bootloader_partitions = vec![];
        for p in &product_bundle.partitions.bootloader_partitions {
            if let Some(name) = &p.name {
                bootloader_partitions.push(v3::Partition {
                    name: name.to_string(),
                    path: p.image.to_string(),
                    condition: None,
                });
            }
        }

        // Copy the bootstrap partitions from the partitions config to the flash manifest.
        let mut bootstrap_partitions = vec![];
        for p in &product_bundle.partitions.bootstrap_partitions {
            let condition = if let Some(c) = &p.condition {
                Some(v3::Condition { variable: c.variable.to_string(), value: c.value.to_string() })
            } else {
                None
            };
            bootstrap_partitions.push(v3::Partition {
                name: p.name.to_string(),
                path: p.image.to_string(),
                condition,
            });
        }
        // Append the bootloader partitions, bootstrapping a device means flashing any initial
        // bootstrap images plus a working bootloader. The bootstrap partitions should always come
        // first as the lowest-level items so that the higher-level bootloader images can depend on
        // bootstrapping being done.
        bootstrap_partitions.extend_from_slice(bootloader_partitions.as_slice());

        // Create a map from slot to available images by name (zbi, vbmeta, fvm).
        let mut image_map: ImageMap = BTreeMap::new();
        if let Some(manifest) = &product_bundle.system_a {
            add_images_to_map(&mut image_map, &manifest, Slot::A)?;
        }
        if let Some(manifest) = &product_bundle.system_b {
            add_images_to_map(&mut image_map, &manifest, Slot::B)?;
        }
        if let Some(manifest) = &product_bundle.system_r {
            add_images_to_map(&mut image_map, &manifest, Slot::R)?;
        }

        // Define the flashable "products".
        let mut products = vec![];
        products.push(v3::Product {
            name: "recovery".into(),
            bootloader_partitions: bootloader_partitions.clone(),
            partitions: get_mapped_partitions(
                &product_bundle.partitions.partitions,
                &image_map,
                /*is_recovery=*/ true,
            ),
            oem_files: vec![],
            requires_unlock: false,
        });
        products.push(v3::Product {
            name: "fuchsia_only".into(),
            bootloader_partitions: bootloader_partitions.clone(),
            partitions: get_mapped_partitions(
                &product_bundle.partitions.partitions,
                &image_map,
                /*is_recovery=*/ false,
            ),
            oem_files: vec![],
            requires_unlock: false,
        });
        products.push(v3::Product {
            name: "fuchsia".into(),
            bootloader_partitions: bootstrap_partitions.clone(),
            partitions: get_mapped_partitions(
                &product_bundle.partitions.partitions,
                &image_map,
                /*is_recovery=*/ false,
            ),
            oem_files: vec![],
            requires_unlock: !product_bundle.partitions.bootstrap_partitions.is_empty(),
        });
        if !product_bundle.partitions.bootstrap_partitions.is_empty() {
            products.push(v3::Product {
                name: "bootstrap".into(),
                bootloader_partitions: bootstrap_partitions.clone(),
                partitions: vec![],
                oem_files: vec![],
                requires_unlock: true,
            });
        }

        // Create the flash manifest.
        let ret = v3::FlashManifest {
            hw_revision: product_bundle.partitions.hardware_revision.clone(),
            credentials,
            products,
        };

        Ok(Self::V3(ret))
    }
}

/// Add a set of images from |manifest| to the |image_map|, assigning them to |slot|. This ignores
/// all images other than the ZBI, VBMeta, and fastboot FVM/Fxfs.
fn add_images_to_map(
    image_map: &mut ImageMap,
    manifest: &Vec<AssemblyManifestImage>,
    slot: Slot,
) -> Result<()> {
    let slot_entry = image_map.entry(slot).or_insert(BTreeMap::new());
    for image in manifest.iter() {
        match image {
            AssemblyManifestImage::ZBI { path, .. } => {
                slot_entry.insert(ImageType::ZBI, path.to_string())
            }
            AssemblyManifestImage::VBMeta(path) => {
                slot_entry.insert(ImageType::VBMeta, path.to_string())
            }
            AssemblyManifestImage::FVMFastboot(path) => {
                if let Slot::R = slot {
                    // Recovery should not include a separate FVM, because it is embedded into the
                    // ZBI as a ramdisk.
                    None
                } else {
                    slot_entry.insert(ImageType::FVM, path.to_string())
                }
            }
            assembly_manifest::Image::FxfsSparse { path, .. } => {
                if let Slot::R = slot {
                    // Recovery should not include fxfs, because it is embedded into the ZBI as a
                    // ramdisk.
                    None
                } else {
                    slot_entry.insert(ImageType::Fxfs, path.to_string())
                }
            }
            _ => None,
        };
    }
    Ok(())
}

/// Construct a list of partitions to add to the flash manifest by mapping the partitions to the
/// images. If |is_recovery|, then put the recovery images in every slot.
fn get_mapped_partitions(
    partitions: &Vec<Partition>,
    image_map: &ImageMap,
    is_recovery: bool,
) -> Vec<v3::Partition> {
    let mut mapped_partitions = vec![];

    // Assign the images to particular partitions. If |is_recovery|, then we use the recovery
    // images for all slots.
    for p in partitions {
        let (partition_name, image_type, slot) = match p {
            Partition::ZBI { name, slot, .. } => (name, ImageType::ZBI, slot),
            Partition::VBMeta { name, slot, .. } => (name, ImageType::VBMeta, slot),

            // Arbitrarily, take the fvm from the slot A system.
            Partition::FVM { name, .. } => (name, ImageType::FVM, &Slot::A),

            // Arbitrarily, take Fxfs from the slot A system.
            Partition::Fxfs { name, .. } => (name, ImageType::Fxfs, &Slot::A),
        };

        if let Some(slot) = match is_recovery {
            // If this is recovery mode, then fill every partition with images from the slot R
            // system.
            true => image_map.get(&Slot::R),
            false => image_map.get(slot),
        } {
            if let Some(image_path) = slot.get(&image_type) {
                mapped_partitions.push(v3::Partition {
                    name: partition_name.to_string(),
                    path: image_path.to_string(),
                    condition: None,
                });
            }
        }
    }

    mapped_partitions
}

#[async_trait(?Send)]
impl Flash for FlashManifestVersion {
    #[tracing::instrument(skip(writer, cmd, file_resolver, self))]
    async fn flash<W, F, T>(
        &self,
        writer: &mut W,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        W: Write,
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let total_time = Utc::now();
        prepare(writer, fastboot_interface).await?;
        match self {
            Self::V1(v) => v.flash(writer, file_resolver, fastboot_interface, cmd).await?,
            Self::V2(v) => v.flash(writer, file_resolver, fastboot_interface, cmd).await?,
            Self::V3(v) => v.flash(writer, file_resolver, fastboot_interface, cmd).await?,
        };
        let duration = Utc::now().signed_duration_since(total_time);
        writeln!(
            writer,
            "{}Done. Total Time{} [{}{:.2}s{}]",
            color::Fg(color::Green),
            style::Reset,
            color::Fg(color::Blue),
            (duration.num_milliseconds() as f32) / (1000 as f32),
            style::Reset
        )?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl Unlock for FlashManifestVersion {
    async fn unlock<W, F, T>(
        &self,
        writer: &mut W,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
    ) -> Result<()>
    where
        W: Write,
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let total_time = Utc::now();
        prepare(writer, fastboot_interface).await?;
        match self {
            Self::V1(v) => v.unlock(writer, file_resolver, fastboot_interface).await?,
            Self::V2(v) => v.unlock(writer, file_resolver, fastboot_interface).await?,
            Self::V3(v) => v.unlock(writer, file_resolver, fastboot_interface).await?,
        };
        let duration = Utc::now().signed_duration_since(total_time);
        writeln!(
            writer,
            "{}Done. Total Time{} [{}{:.2}s{}]",
            color::Fg(color::Green),
            style::Reset,
            color::Fg(color::Blue),
            (duration.num_milliseconds() as f32) / (1000 as f32),
            style::Reset
        )?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl Boot for FlashManifestVersion {
    async fn boot<W, F, T>(
        &self,
        writer: &mut W,
        file_resolver: &mut F,
        slot: String,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        W: Write,
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        let total_time = Utc::now();
        prepare(writer, fastboot_interface).await?;
        match self {
            Self::V1(v) => v.boot(writer, file_resolver, slot, fastboot_interface, cmd).await?,
            Self::V2(v) => v.boot(writer, file_resolver, slot, fastboot_interface, cmd).await?,
            Self::V3(v) => v.boot(writer, file_resolver, slot, fastboot_interface, cmd).await?,
        };
        let duration = Utc::now().signed_duration_since(total_time);
        writeln!(
            writer,
            "{}Done. Total Time{} [{}{:.2}s{}]",
            color::Fg(color::Green),
            style::Reset,
            color::Fg(color::Blue),
            (duration.num_milliseconds() as f32) / (1000 as f32),
            style::Reset
        )?;
        Ok(())
    }
}

pub async fn from_sdk<W: Write, F: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut F,
    cmd: ManifestParams,
) -> Result<()> {
    tracing::debug!("fastboot manifest from_sdk");
    match cmd.product_bundle.as_ref() {
        Some(b) => {
            let product_bundle = load_product_bundle(&Some(b.to_string())).await?.into();
            FlashManifest {
                resolver: Resolver::new(PathBuf::from(b))?,
                version: FlashManifestVersion::from_product_bundle(&product_bundle)?,
            }
            .flash(writer, fastboot_interface, cmd)
            .await
        }
        None => ffx_bail!(
            "Please supply the `--product-bundle` option to identify which product bundle to flash"
        ),
    }
}

#[tracing::instrument(skip(writer, cmd))]
pub async fn from_local_product_bundle<W: Write, F: FastbootInterface>(
    writer: &mut W,
    path: PathBuf,
    fastboot_interface: &mut F,
    cmd: ManifestParams,
) -> Result<()> {
    tracing::debug!("fastboot manifest from_local_product_bundle");
    let path = Utf8Path::from_path(&*path).ok_or_else(|| anyhow!("Error getting path"))?;
    let product_bundle = ProductBundle::try_load_from(path)?;

    let flash_manifest_version = FlashManifestVersion::from_product_bundle(&product_bundle)?;

    match (path.is_file(), path.extension()) {
        (true, Some("zip")) => {
            FlashManifest {
                resolver: ZipArchiveResolver::new(writer, path.into())?,
                version: flash_manifest_version,
            }
            .flash(writer, fastboot_interface, cmd)
            .await
        }
        (true, extension) => Err(anyhow!(
            "Attempting to flash using a Product Bundle file with unsupported extension: {:#?}",
            extension
        )),
        (false, _) => {
            FlashManifest { resolver: Resolver::new(path.into())?, version: flash_manifest_version }
                .flash(writer, fastboot_interface, cmd)
                .await
        }
    }
}

pub async fn from_in_tree<W: Write, T: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()> {
    tracing::debug!("fastboot manifest from_in_tree");
    if cmd.product_bundle.is_some() {
        tracing::debug!("in tree, but product bundle specified, use in-tree sdk");
        from_sdk(writer, fastboot_interface, cmd).await
    } else {
        bail!("manifest or product_bundle must be specified")
    }
}

pub async fn from_path<W: Write, T: FastbootInterface>(
    writer: &mut W,
    path: PathBuf,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()> {
    tracing::debug!("fastboot manifest from_path");
    match path.extension() {
        Some(ext) => {
            if ext == "zip" {
                let r = ArchiveResolver::new(writer, path)?;
                load_flash_manifest(r).await?.flash(writer, fastboot_interface, cmd).await
            } else if ext == "tgz" || ext == "tar.gz" || ext == "tar" {
                let r = FlashManifestTarResolver::new(writer, path)?;
                load_flash_manifest(r).await?.flash(writer, fastboot_interface, cmd).await
            } else {
                let r = FlashManifestResolver::new(path)?;
                load_flash_manifest(r).await?.flash(writer, fastboot_interface, cmd).await
            }
        }
        _ => {
            let r = FlashManifestResolver::new(path)?;
            load_flash_manifest(r).await?.flash(writer, fastboot_interface, cmd).await
        }
    }
}

async fn load_flash_manifest<F: ManifestResolver + FileResolver + Sync>(
    resolver: F,
) -> Result<FlashManifest<impl FileResolver + Sync>> {
    let reader = File::open(resolver.get_manifest_path().await).map(BufReader::new)?;
    Ok(FlashManifest { resolver, version: FlashManifestVersion::load(reader)? })
}

pub struct FlashManifest<F: FileResolver + Sync> {
    resolver: F,
    version: FlashManifestVersion,
}

impl<F: FileResolver + Sync> FlashManifest<F> {
    #[tracing::instrument(skip(self, writer, cmd))]
    pub async fn flash<W: Write, T: FastbootInterface>(
        &mut self,
        writer: &mut W,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()> {
        match &cmd.op {
            Command::Flash => {
                self.version.flash(writer, &mut self.resolver, fastboot_interface, cmd).await
            }
            Command::Unlock(_) => {
                // Using the manifest, don't need the unlock credential from the UnlockCommand
                // here.
                self.version.unlock(writer, &mut self.resolver, fastboot_interface).await
            }
            Command::Boot(BootParams { slot, .. }) => {
                self.version
                    .boot(writer, &mut self.resolver, slot.to_owned(), fastboot_interface, cmd)
                    .await
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use assembly_partitions_config::{BootloaderPartition, BootstrapPartition, PartitionsConfig};
    use camino::Utf8PathBuf;
    use maplit::btreemap;
    use serde_json::from_str;

    const UNKNOWN_VERSION: &'static str = r#"{
        "version": 99999,
        "manifest": "test"
    }"#;

    const MANIFEST: &'static str = r#"{
        "version": 1,
        "manifest": []
    }"#;

    const ARRAY_MANIFEST: &'static str = r#"[{
        "version": 1,
        "manifest": []
    }]"#;

    #[test]
    fn test_deserialization() -> Result<()> {
        let _manifest: ManifestFile = from_str(MANIFEST)?;
        Ok(())
    }

    #[test]
    fn test_serialization() -> Result<()> {
        let manifest = FlashManifestVersion::V3(FlashManifestV3 {
            hw_revision: "board".into(),
            credentials: vec![],
            products: vec![],
        });
        let mut buf = Vec::new();
        manifest.write(&mut buf).unwrap();
        let str = String::from_utf8(buf).unwrap();
        assert_eq!(
            str,
            r#"{
  "manifest": {
    "hw_revision": "board"
  },
  "version": 3
}"#
        );
        Ok(())
    }

    #[test]
    fn test_loading_unknown_version() {
        let manifest_contents = UNKNOWN_VERSION.to_string();
        let result = FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes()));
        assert!(result.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_loading_version_1() -> Result<()> {
        let manifest_contents = MANIFEST.to_string();
        FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes())).map(|_| ())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_loading_version_1_from_array() -> Result<()> {
        let manifest_contents = ARRAY_MANIFEST.to_string();
        FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes())).map(|_| ())
    }

    #[test]
    fn test_add_images_to_map() {
        let mut image_map: ImageMap = BTreeMap::new();
        let manifest = vec![
            AssemblyManifestImage::ZBI { path: "path/to/fuchsia.zbi".into(), signed: false },
            AssemblyManifestImage::VBMeta("path/to/fuchsia.vbmeta".into()),
            AssemblyManifestImage::FVMFastboot("path/to/fvm.fastboot.blk".into()),
            // These should be ignored.
            AssemblyManifestImage::FVM("path/to/fvm.blk".into()),
            AssemblyManifestImage::BasePackage("path/to/base".into()),
        ];
        add_images_to_map(&mut image_map, &manifest, Slot::A).unwrap();
        assert_eq!(image_map.len(), 1);
        assert_eq!(image_map[&Slot::A].len(), 3);
        assert_eq!(image_map[&Slot::A][&ImageType::ZBI], "path/to/fuchsia.zbi");
        assert_eq!(image_map[&Slot::A][&ImageType::VBMeta], "path/to/fuchsia.vbmeta");
        assert_eq!(image_map[&Slot::A][&ImageType::FVM], "path/to/fvm.fastboot.blk");

        add_images_to_map(&mut image_map, &manifest, Slot::B).unwrap();
        assert_eq!(image_map.len(), 2);
        assert_eq!(image_map[&Slot::B].len(), 3);
        assert_eq!(image_map[&Slot::B][&ImageType::ZBI], "path/to/fuchsia.zbi");
        assert_eq!(image_map[&Slot::B][&ImageType::VBMeta], "path/to/fuchsia.vbmeta");
        assert_eq!(image_map[&Slot::B][&ImageType::FVM], "path/to/fvm.fastboot.blk");

        add_images_to_map(&mut image_map, &manifest, Slot::R).unwrap();
        assert_eq!(image_map.len(), 3);
        assert_eq!(image_map[&Slot::R].len(), 2);
        assert_eq!(image_map[&Slot::R][&ImageType::ZBI], "path/to/fuchsia.zbi");
        assert_eq!(image_map[&Slot::R][&ImageType::VBMeta], "path/to/fuchsia.vbmeta");
    }

    #[test]
    fn test_get_mapped_partitions_no_slots() {
        let partitions = vec![];
        let image_map: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi".into(),
                ImageType::VBMeta => "vbmeta".into(),
                ImageType::FVM => "fvm".into(),
            },
        };
        let mapped = get_mapped_partitions(&partitions, &image_map, /*is_recovery=*/ false);
        assert!(mapped.is_empty());
    }

    #[test]
    fn test_get_mapped_partitions_slot_a_only() {
        let partitions = vec![
            Partition::ZBI { name: "part1".into(), slot: Slot::A, size: None },
            Partition::VBMeta { name: "part2".into(), slot: Slot::A, size: None },
            Partition::FVM { name: "part3".into(), size: None },
        ];
        let image_map: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::FVM => "fvm_a".into(),
            },
            Slot::B => btreemap!{
                ImageType::ZBI => "zbi_b".into(),
                ImageType::VBMeta => "vbmeta_b".into(),
                ImageType::FVM => "fvm_b".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
            },
        };
        let mapped = get_mapped_partitions(&partitions, &image_map, /*is_recovery=*/ false);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_a".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_a".into(), condition: None },
                v3::Partition { name: "part3".into(), path: "fvm_a".into(), condition: None },
            ]
        );
    }

    #[test]
    fn test_get_mapped_partitions_fvm_and_fxfs() {
        let partitions = vec![
            Partition::ZBI { name: "part1".into(), slot: Slot::A, size: None },
            Partition::VBMeta { name: "part2".into(), slot: Slot::A, size: None },
            Partition::FVM { name: "part3".into(), size: None },
            Partition::Fxfs { name: "part4".into(), size: None },
        ];
        let image_map_fvm: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::FVM => "fvm_a".into(),
            },
            Slot::B => btreemap!{
                ImageType::ZBI => "zbi_b".into(),
                ImageType::VBMeta => "vbmeta_b".into(),
                ImageType::FVM => "fvm_b".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
            },
        };
        let image_map_fxfs: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::Fxfs => "fxfs_a".into(),
            },
            Slot::B => btreemap!{
                ImageType::ZBI => "zbi_b".into(),
                ImageType::VBMeta => "vbmeta_b".into(),
                ImageType::Fxfs => "fxfs_b".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
            },
        };
        let mapped =
            get_mapped_partitions(&partitions, &image_map_fvm, /*is_recovery=*/ false);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_a".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_a".into(), condition: None },
                v3::Partition { name: "part3".into(), path: "fvm_a".into(), condition: None },
            ]
        );
        let mapped =
            get_mapped_partitions(&partitions, &image_map_fxfs, /*is_recovery=*/ false);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_a".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_a".into(), condition: None },
                v3::Partition { name: "part4".into(), path: "fxfs_a".into(), condition: None },
            ]
        );
    }

    #[test]
    fn test_get_mapped_partitions_all_slots() {
        let partitions = vec![
            Partition::ZBI { name: "part1".into(), slot: Slot::A, size: None },
            Partition::VBMeta { name: "part2".into(), slot: Slot::A, size: None },
            Partition::ZBI { name: "part3".into(), slot: Slot::B, size: None },
            Partition::VBMeta { name: "part4".into(), slot: Slot::B, size: None },
            Partition::ZBI { name: "part5".into(), slot: Slot::R, size: None },
            Partition::VBMeta { name: "part6".into(), slot: Slot::R, size: None },
            Partition::FVM { name: "part7".into(), size: None },
        ];
        let image_map: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::FVM => "fvm_a".into(),
            },
            Slot::B => btreemap!{
                ImageType::ZBI => "zbi_b".into(),
                ImageType::VBMeta => "vbmeta_b".into(),
                ImageType::FVM => "fvm_b".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
                ImageType::FVM => "fvm_r".into(),
            },
        };
        let mapped = get_mapped_partitions(&partitions, &image_map, /*is_recovery=*/ false);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_a".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_a".into(), condition: None },
                v3::Partition { name: "part3".into(), path: "zbi_b".into(), condition: None },
                v3::Partition { name: "part4".into(), path: "vbmeta_b".into(), condition: None },
                v3::Partition { name: "part5".into(), path: "zbi_r".into(), condition: None },
                v3::Partition { name: "part6".into(), path: "vbmeta_r".into(), condition: None },
                v3::Partition { name: "part7".into(), path: "fvm_a".into(), condition: None },
            ]
        );
    }

    #[test]
    fn test_get_mapped_partitions_missing_slot() {
        let partitions = vec![
            Partition::ZBI { name: "part1".into(), slot: Slot::A, size: None },
            Partition::VBMeta { name: "part2".into(), slot: Slot::A, size: None },
            Partition::ZBI { name: "part3".into(), slot: Slot::B, size: None },
            Partition::VBMeta { name: "part4".into(), slot: Slot::B, size: None },
            Partition::ZBI { name: "part5".into(), slot: Slot::R, size: None },
            Partition::VBMeta { name: "part6".into(), slot: Slot::R, size: None },
            Partition::FVM { name: "part7".into(), size: None },
        ];
        let image_map: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::FVM => "fvm_a".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
            },
        };
        let mapped = get_mapped_partitions(&partitions, &image_map, /*is_recovery=*/ false);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_a".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_a".into(), condition: None },
                v3::Partition { name: "part5".into(), path: "zbi_r".into(), condition: None },
                v3::Partition { name: "part6".into(), path: "vbmeta_r".into(), condition: None },
                v3::Partition { name: "part7".into(), path: "fvm_a".into(), condition: None },
            ]
        );
    }

    #[test]
    fn test_get_mapped_partitions_recovery() {
        let partitions = vec![
            Partition::ZBI { name: "part1".into(), slot: Slot::A, size: None },
            Partition::VBMeta { name: "part2".into(), slot: Slot::A, size: None },
            Partition::ZBI { name: "part3".into(), slot: Slot::B, size: None },
            Partition::VBMeta { name: "part4".into(), slot: Slot::B, size: None },
            Partition::ZBI { name: "part5".into(), slot: Slot::R, size: None },
            Partition::VBMeta { name: "part6".into(), slot: Slot::R, size: None },
            Partition::FVM { name: "part7".into(), size: None },
        ];
        let image_map: ImageMap = btreemap! {
            Slot::A => btreemap!{
                ImageType::ZBI => "zbi_a".into(),
                ImageType::VBMeta => "vbmeta_a".into(),
                ImageType::FVM => "fvm_a".into(),
            },
            Slot::B => btreemap!{
                ImageType::ZBI => "zbi_b".into(),
                ImageType::VBMeta => "vbmeta_b".into(),
                ImageType::FVM => "fvm_b".into(),
            },
            Slot::R => btreemap!{
                ImageType::ZBI => "zbi_r".into(),
                ImageType::VBMeta => "vbmeta_r".into(),
            },
        };
        let mapped = get_mapped_partitions(&partitions, &image_map, /*is_recovery=*/ true);
        assert_eq!(
            mapped,
            vec![
                v3::Partition { name: "part1".into(), path: "zbi_r".into(), condition: None },
                v3::Partition { name: "part2".into(), path: "vbmeta_r".into(), condition: None },
                v3::Partition { name: "part3".into(), path: "zbi_r".into(), condition: None },
                v3::Partition { name: "part4".into(), path: "vbmeta_r".into(), condition: None },
                v3::Partition { name: "part5".into(), path: "zbi_r".into(), condition: None },
                v3::Partition { name: "part6".into(), path: "vbmeta_r".into(), condition: None },
            ]
        );
    }

    #[test]
    fn test_from_product_bundle_bootstrap_partitions() {
        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![BootstrapPartition {
                    name: "bootstrap_part".into(),
                    condition: None,
                    image: Utf8PathBuf::from("bootstrap_image"),
                }],
                bootloader_partitions: vec![BootloaderPartition {
                    name: Some("bootloader_part".into()),
                    image: Utf8PathBuf::from("bootloader_image"),
                    partition_type: "".into(),
                }],
                partitions: vec![],
                hardware_revision: String::default(),
                unlock_credentials: vec![],
            },
            sdk_version: String::default(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let manifest = match FlashManifestVersion::from_product_bundle(&pb).unwrap() {
            FlashManifestVersion::V3(manifest) => manifest,
            _ => panic!("Expected a V3 FlashManifest"),
        };
        let bootstrap_product = manifest.products.iter().find(|&p| p.name == "bootstrap").unwrap();
        // The important piece here is that the bootstrap partition comes first.
        assert_eq!(
            bootstrap_product.bootloader_partitions,
            vec![
                v3::Partition {
                    name: "bootstrap_part".into(),
                    path: "bootstrap_image".into(),
                    condition: None
                },
                v3::Partition {
                    name: "bootloader_part".into(),
                    path: "bootloader_image".into(),
                    condition: None
                },
            ]
        )
    }
}
