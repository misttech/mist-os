// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/111242): Exercise production update package implementations to eliminate dead code
// warnings.
#![allow(dead_code)]

use super::api;
use super::api::Package as _;
use super::blob::BlobOpenError;
use super::blob::BlobSet;
use super::data_source::DataSource;
use super::package::Error as PackageError;
use super::package::Package;
use fuchsia_url::AbsolutePackageUrl;
use std::io;
use std::io::Read as _;
use thiserror::Error;

/// Errors that may occur constructing an [`UpdatePackage`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to locate update package blob: {0}")]
    BlobOpen(#[from] BlobOpenError),
    #[error("failed to instantiate update package as package object: {0}")]
    Package(#[from] PackageError),
    #[error("failed to locate packages.json file at {packages_json_path:?} in update package {update_package_hash}")]
    MissingPackagesJson {
        packages_json_path: Box<dyn api::Path>,
        update_package_hash: Box<dyn api::Hash>,
    },
    #[error("failed to open packages.json blob: {0}")]
    PackagesJsonBlob(#[from] api::BlobError),
    #[error("failed to read packages.json blob: {0}")]
    PackagesJsonRead(#[from] io::Error),
    #[error("failed to parse packages.json in update package: {0}")]
    PackagesJsonParse(#[from] update_package::ParsePackageError),
}

/// An internal model of a Fuchsia update package. See
/// https://fuchsia.dev/fuchsia-src/concepts/packages/update_pkg for details.
#[derive(Clone)]
pub(crate) struct UpdatePackage {
    package: Box<dyn api::Package>,
    packages_json: Vec<AbsolutePackageUrl>,
}

impl UpdatePackage {
    /// Constructs a new [`UpdatePackage`] assumed to be loaded from `parent_data_source`. The
    /// update package data is loaded by:
    ///
    /// 1. Obtaining the update package `meta.far` file with hash `update_package_hash` from
    ///    `blob_set`;
    /// 2. Loading update package-specific metadata from blobs in `blob_set`.
    pub fn new(
        parent_data_source: Option<DataSource>,
        update_package_hash: Box<dyn api::Hash>,
        blob_set: Box<dyn BlobSet>,
    ) -> Result<Self, Error> {
        let update_package_blob = blob_set.blob(update_package_hash.clone())?;

        let package = Package::new(
            parent_data_source,
            api::PackageResolverUrl::Url,
            update_package_blob,
            blob_set,
        )?;
        let packages_json: Box<dyn api::Path> = Box::new("packages.json");
        let (_, packages_json_blob) = package
            .content_blobs()
            .find(|(package_path, _)| package_path.as_ref() == packages_json.as_ref())
            .ok_or_else(|| Error::MissingPackagesJson {
                packages_json_path: packages_json,
                update_package_hash,
            })?;
        let mut packages_json_reader = packages_json_blob.reader_seeker()?;
        let mut packages_json_contents = vec![];
        packages_json_reader.read_to_end(&mut packages_json_contents)?;
        Ok(Self {
            package: Box::new(package),
            packages_json: update_package::parse_packages_json(packages_json_contents.as_slice())?,
        })
    }
}

impl api::Package for UpdatePackage {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.package.hash()
    }

    fn meta_package(&self) -> Box<dyn api::MetaPackage> {
        self.package.meta_package()
    }

    fn meta_contents(&self) -> Box<dyn api::MetaContents> {
        self.package.meta_contents()
    }

    fn content_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        self.package.content_blobs()
    }

    fn meta_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        self.package.meta_blobs()
    }

    fn components(
        &self,
    ) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Component>)>> {
        self.package.components()
    }
}

impl api::UpdatePackage for UpdatePackage {
    fn packages(&self) -> &Vec<AbsolutePackageUrl> {
        &self.packages_json
    }
}

#[cfg(test)]
pub mod test {
    use assembly_manifest::AssemblyManifest;
    use assembly_manifest::Image;
    use assembly_partitions_config::BootloaderPartition;
    use assembly_partitions_config::Partition;
    use assembly_partitions_config::PartitionsConfig;
    use assembly_partitions_config::Slot as PartitionSlot;
    use assembly_update_package::Slot;
    use assembly_update_package::UpdatePackageBuilder;
    use camino::Utf8Path;
    use epoch::EpochFile;
    use fuchsia_merkle::Hash;
    use fuchsia_pkg::PackageManifest;
    use once_cell::sync::Lazy;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    pub(crate) struct FakeUpdatePackage {
        pub hash: Hash,
        pub blobs: Vec<Vec<u8>>,
    }

    pub(crate) static FAKE_UPDATE_PACKAGE: Lazy<FakeUpdatePackage> = Lazy::new(|| {
        let pkg_dir = tempfile::tempdir().expect("create update package tempdir");
        let pkg_path = Utf8Path::from_path(pkg_dir.path()).expect("get update package path");
        let bootloader_file = NamedTempFile::new().expect("create bootloader tempfile");
        let bootloader_path =
            Utf8Path::from_path(bootloader_file.path()).expect("get bootloader path");

        let partitions_cfg = PartitionsConfig {
            bootstrap_partitions: vec![],
            unlock_credentials: vec![],
            bootloader_partitions: vec![BootloaderPartition {
                partition_type: "tpl".into(),
                name: Some("firmware_tpl".into()),
                image: bootloader_path.to_path_buf(),
            }],
            partitions: vec![Partition::ZBI { name: "zircon_a".into(), slot: PartitionSlot::A }],
            hardware_revision: "hw".into(),
        };

        let epoch = EpochFile::Version1 { epoch: 0 };
        let mut version_file = NamedTempFile::new().expect("create version file");
        writeln!(version_file, "1.2.3.4").expect("write version file");

        let zbi_file = NamedTempFile::new().expect("create zbi file");
        let zbi_path = Utf8Path::from_path(zbi_file.path()).expect("get zbi path");

        let mut builder = UpdatePackageBuilder::new(
            partitions_cfg,
            "board",
            version_file.path().to_path_buf(),
            epoch.clone(),
            Some(0xECDB841C251A8CB9),
            &pkg_path,
        );
        builder.add_slot_images(Slot::Primary(AssemblyManifest {
            images: vec![Image::ZBI { path: zbi_path.to_path_buf(), signed: true }],
        }));
        builder.build().expect("build update package");

        let pkg_manifest = PackageManifest::try_load_from(
            Utf8Path::from_path(&pkg_dir.path().join("update_package_manifest.json"))
                .expect("create update package manifest path"),
        )
        .expect("load update package manifest");

        let hash = pkg_manifest.hash();
        let blobs = pkg_manifest
            .blobs()
            .into_iter()
            .map(|blob_info| {
                std::fs::read(&blob_info.source_path).expect("read blob from package manifest")
            })
            .collect::<Vec<_>>();
        FakeUpdatePackage { hash, blobs }
    });
}
