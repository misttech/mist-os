// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::package_reader::{PackageReader, PackagesFromUpdateReader};
use crate::package_types::PartialPackageDefinition;
use anyhow::{anyhow, bail, format_err, Context, Result};
use fuchsia_hash::Hash;
use fuchsia_url::{AbsolutePackageUrl, PackageName, PackageVariant};
use log::{info, warn};
use scrutiny_collection::model::DataModel;
use scrutiny_collection::zbi::Zbi;
use scrutiny_utils::artifact::{ArtifactReader, FileArtifactReader};
use scrutiny_utils::bootfs::{BootfsFileIndex, BootfsPackageIndex, BootfsReader};
use scrutiny_utils::key_value::parse_key_value;
use scrutiny_utils::package::PackageIndexContents;
use scrutiny_utils::url::from_package_name_variant_path;
use scrutiny_utils::zbi::{ZbiReader, ZbiType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use update_package::parse_image_packages_json;

/// The path of the file in bootfs that lists all the bootfs packages.
const BOOT_PACKAGE_INDEX: &str = "data/bootfs_packages";
/// The path of the file in bootfs that lists all the bootfs packages.
const IMAGES_JSON_PATH: &str = "images.json";
const IMAGES_JSON_ORIG_PATH: &str = "images.json.orig";

/// A collector that returns the zbi contents in a product.
#[derive(Default)]
pub struct ZbiCollector;

impl ZbiCollector {
    pub fn collect(&self, model: Arc<DataModel>) -> Result<()> {
        let model_config = model.config();
        let blobs_directory = &model_config.blobs_directory();
        let artifact_reader = FileArtifactReader::new(&PathBuf::new(), blobs_directory);
        let mut package_reader: Box<dyn PackageReader> = Box::new(PackagesFromUpdateReader::new(
            &model_config.update_package_path(),
            Box::new(artifact_reader.clone()),
        ));
        let mut artifact_reader: Box<dyn ArtifactReader> = Box::new(artifact_reader);

        let update_package = package_reader
            .read_update_package_definition()
            .context("Failed to read update package definition for package data collector")?;
        let zbi = extract_zbi_from_update_package(
            &mut artifact_reader,
            &mut package_reader,
            &update_package,
            model.config().is_recovery(),
        )?;
        model.set(zbi)?;

        Ok(())
    }
}

/// Extracts the ZBI from the update package and parses it into the ZBI
/// model.
fn extract_zbi_from_update_package(
    artifact_reader: &mut Box<dyn ArtifactReader>,
    package_reader: &mut Box<dyn PackageReader>,
    update_package: &PartialPackageDefinition,
    recovery: bool,
) -> Result<Zbi> {
    info!("Extracting the ZBI from update package");

    let zbi_hash =
        lookup_zbi_hash_in_images_json(artifact_reader, package_reader, update_package, recovery)?;

    let zbi_data = artifact_reader.read_bytes(&Path::new(&zbi_hash.to_string()))?;
    let mut zbi_reader = ZbiReader::new(zbi_data);
    let sections = zbi_reader.parse()?;
    let mut bootfs_files = HashMap::new();
    let mut cmdline = vec![];
    info!(total = sections.len(); "Extracted sections from the ZBI");
    for section in sections.iter() {
        info!(section_type:? = section.section_type; "Extracted sections");
        if section.section_type == ZbiType::StorageBootfs {
            let mut bootfs_reader = BootfsReader::new(section.buffer.clone());
            let bootfs_result = bootfs_reader.parse();
            if let Err(err) = bootfs_result {
                warn!(err:%; "Bootfs parse failed");
            } else {
                bootfs_files = bootfs_result.unwrap();
                info!(total = bootfs_files.len(); "Bootfs found files");
            }
        } else if section.section_type == ZbiType::Cmdline {
            let mut cmd_buffer = section.buffer.clone();
            // The cmdline.blk contains a trailing 0.
            cmd_buffer.truncate(cmd_buffer.len() - 1);
            let cmd_str = std::str::from_utf8(&cmd_buffer)
                .context("Failed to convert kernel arguments to utf-8")?;
            let mut cmd = cmd_str.split(' ').map(ToString::to_string).collect::<Vec<String>>();
            cmd.sort();
            cmdline.extend(cmd);
        }
    }

    // Find the bootfs package index
    let bootfs_pkg_contents = bootfs_files.iter().find_map(|(file_name, data)| {
        if file_name == BOOT_PACKAGE_INDEX {
            Some(data)
        } else {
            None
        }
    });
    let bootfs_packages: Option<Result<PackageIndexContents>> = bootfs_pkg_contents.map(|data| {
        let bootfs_pkg_contents = std::str::from_utf8(&data)?;
        let bootfs_pkgs = parse_key_value(bootfs_pkg_contents)?;
        let bootfs_pkgs = bootfs_pkgs
            .into_iter()
            .map(|(name_and_variant, merkle)| {
                let url = from_package_name_variant_path(name_and_variant)?;
                let merkle = Hash::from_str(&merkle)?;
                Ok(((url.name().clone(), url.variant().map(|v| v.clone())), merkle))
            })
            // Handle errors via collect
            // Iter<Result<_, __>> into Result<Vec<_>, __>.
            .collect::<Result<Vec<((PackageName, Option<PackageVariant>), Hash)>>>()
            .map_err(|err| {
                format_err!("Failed to parse bootfs package index name/variant=merkle: {:?}", err)
            })?
            // Collect Vec<(_, __)> into HashMap<_, __>.
            .into_iter()
            .collect::<PackageIndexContents>();
        Ok(bootfs_pkgs)
    });
    let bootfs_files = BootfsFileIndex { bootfs_files };
    let bootfs_packages = BootfsPackageIndex { bootfs_pkgs: bootfs_packages.transpose()? };
    let mut deps = artifact_reader.get_deps();
    deps.extend(package_reader.get_deps());
    Ok(Zbi { deps, sections, bootfs_files, bootfs_packages, cmdline })
}

fn lookup_zbi_hash_in_images_json(
    artifact_reader: &mut Box<dyn ArtifactReader>,
    package_reader: &mut Box<dyn PackageReader>,
    update_package: &PartialPackageDefinition,
    recovery: bool,
) -> Result<Hash> {
    let images_json_hash = update_package
        .contents
        .get(&PathBuf::from(IMAGES_JSON_PATH))
        .or_else(|| update_package.contents.get(&PathBuf::from(IMAGES_JSON_ORIG_PATH)))
        .ok_or_else(|| anyhow!("Update package contains no images manifest entry"))?;
    let images_json_contents = artifact_reader
        .read_bytes(&Path::new(&images_json_hash.to_string()))
        .context("Failed to open images manifest blob designated in update package")?;
    let image_packages_manifest = parse_image_packages_json(images_json_contents.as_slice())
        .context("Failed to parse images manifest in update package")?;
    let metadata = if recovery {
        image_packages_manifest.recovery().ok_or_else(|| {
            anyhow!("Update package images manifest contains no recovery boot slot images")
        })
    } else {
        image_packages_manifest.fuchsia().ok_or_else(|| {
            anyhow!("Update package images manifest contains no fuchsia boot slot images")
        })
    }?;

    let images_component_url = metadata.zbi().url();
    let images_package_url = match metadata.zbi().url().package_url() {
        AbsolutePackageUrl::Unpinned(_) => bail!("Images package is not pinned"),
        AbsolutePackageUrl::Pinned(pinned) => pinned,
    };
    let images_package =
        package_reader.read_package_definition(&images_package_url).with_context(|| {
            format!(
                "Failed to located update package images package with URL {}",
                images_package_url
            )
        })?;

    let zbi_path = PathBuf::from(images_component_url.resource());
    images_package.contents.get(&zbi_path).map(Hash::clone).ok_or_else(|| {
        anyhow!(
            "Update package images package contains no {} zbi entry {:?}",
            if recovery { "recovery" } else { "fuchsia" },
            zbi_path
        )
    })
}

#[cfg(test)]
mod tests {
    use super::ZbiCollector;
    use scrutiny_collection::model::DataModel;
    use scrutiny_collection::model_config::ModelConfig;
    use scrutiny_collection::zbi::Zbi;
    use std::sync::Arc;

    const PRODUCT_BUNDLE_PATH: &str = env!("PRODUCT_BUNDLE_PATH");

    #[test]
    fn bootfs() {
        let model = ModelConfig::from_product_bundle(PRODUCT_BUNDLE_PATH).unwrap();
        let data_model = Arc::new(DataModel::new(model).unwrap());
        let collector = ZbiCollector {};
        collector.collect(data_model.clone()).unwrap();
        let collection = data_model.get::<Zbi>().unwrap();
        assert!(collection.bootfs_files.bootfs_files.contains_key(&"path/to/version".to_string()));
    }

    #[test]
    fn cmdline() {
        let model = ModelConfig::from_product_bundle(PRODUCT_BUNDLE_PATH).unwrap();
        let data_model = Arc::new(DataModel::new(model).unwrap());
        let collector = ZbiCollector {};
        collector.collect(data_model.clone()).unwrap();
        let collection = data_model.get::<Zbi>().unwrap();
        assert_eq!(collection.cmdline, vec!["abc".to_string(), "def".to_string(),]);
    }
}
