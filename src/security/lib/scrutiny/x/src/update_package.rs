// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::api::Blob as _;
use crate::api::Package as _;
use crate::blob;
use crate::package;
use crate::package::Package;
use crate::package::PackageInitializationError;
use fuchsia_url::AbsolutePackageUrl;
use std::io;
use std::io::Read as _;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UpdatePackageError<
    BlobSetError: api::Error,
    UpdatePackageBlobError: api::Error,
    PackagesJsonBlobError: api::Error,
> {
    #[error("update package not found: {0:?}")]
    FindUpdatePackage(BlobSetError),
    #[error("failed to open update package: {0:?}")]
    OpenUpdatePackage(UpdatePackageBlobError),
    #[error("failed to open update package as fuchsia package: {0}")]
    InitializePackage(#[from] PackageInitializationError),
    #[error("failed to find packages.json in update package: {0}")]
    FindPackagesJson(#[from] anyhow::Error),
    #[error("failed to open packages.json in update package: {0:?}")]
    OpenPackagesJson(PackagesJsonBlobError),
    #[error("failed to open packages.json in update package: {0}")]
    ReadPackagesJson(#[from] io::Error),
    #[error("parse packages.json in update package: {0}")]
    ParsePackagesJson(#[from] update_package::ParsePackageError),
}

type Error<BS> = UpdatePackageError<
    <BS as blob::BlobSet>::Error,
    <<BS as blob::BlobSet>::Blob as api::Blob>::Error,
    package::BlobError<
        <BS as blob::BlobSet>::Error,
        <<BS as blob::BlobSet>::Blob as api::Blob>::Error,
    >,
>;

pub struct UpdatePackage {
    packages_json: Vec<AbsolutePackageUrl>,
}

impl UpdatePackage {
    pub fn from_hash<
        Hash: api::Hash + From<fuchsia_hash::Hash>,
        Blobs: blob::BlobSet<Hash = Hash> + 'static,
        BlobSource: api::DataSource + Clone + 'static,
    >(
        update_package_hash: &Hash,
        blobs: Blobs,
        blob_source: BlobSource,
    ) -> Result<Self, Error<Blobs>> {
        let update_package_blob =
            blobs.blob(update_package_hash.clone()).map_err(Error::<Blobs>::FindUpdatePackage)?;
        let update_package_reader =
            update_package_blob.reader_seeker().map_err(Error::<Blobs>::OpenUpdatePackage)?;
        let update_package = Package::new(blob_source, update_package_reader, blobs)?;
        let packages_json = PathBuf::from("packages.json");
        let (_, packages_json_blob) = update_package
            .content_blobs()
            .find(|(package_path, _)| package_path == &packages_json)
            .ok_or_else(|| {
                Error::<Blobs>::from(anyhow::anyhow!(
                    "no content blob at path {:?} update package with hash {}",
                    packages_json,
                    update_package_hash
                ))
            })?;
        let mut packages_json_reader =
            packages_json_blob.reader_seeker().map_err(Error::<Blobs>::OpenPackagesJson)?;
        let mut packages_json_contents = vec![];
        packages_json_reader.read_to_end(&mut packages_json_contents)?;
        Ok(Self {
            packages_json: update_package::parse_packages_json(packages_json_contents.as_slice())?,
        })
    }

    pub fn packages(&self) -> &Vec<AbsolutePackageUrl> {
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

    pub struct FakeUpdatePackage {
        pub hash: Hash,
        pub blobs: Vec<Vec<u8>>,
    }

    pub static FAKE_UPDATE_PACKAGE: Lazy<FakeUpdatePackage> = Lazy::new(|| {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let output_directory_path =
            Utf8Path::from_path(temporary_directory.path()).expect("output directory path");

        let fake_bootloader_temporary_file =
            tempfile::NamedTempFile::new().expect("named temporary file");
        let fake_bootloader_path =
            Utf8Path::from_path(fake_bootloader_temporary_file.path()).expect("bootloader path");

        let partitions_config = PartitionsConfig {
            bootstrap_partitions: vec![],
            unlock_credentials: vec![],
            bootloader_partitions: vec![BootloaderPartition {
                partition_type: "tpl".into(),
                name: Some("firmware_tpl".into()),
                image: fake_bootloader_path.to_path_buf(),
            }],
            partitions: vec![Partition::ZBI { name: "zircon_a".into(), slot: PartitionSlot::A }],
            hardware_revision: "hw".into(),
        };
        let epoch = EpochFile::Version1 { epoch: 0 };
        let mut fake_version = NamedTempFile::new().expect("create fake version file");
        writeln!(fake_version, "1.2.3.4").expect("write fake version file");
        let mut builder = UpdatePackageBuilder::new(
            partitions_config,
            "board",
            fake_version.path().to_path_buf(),
            epoch.clone(),
            Some(0xECDB841C251A8CB9),
            &output_directory_path,
        );
        let fake_zbi_file = NamedTempFile::new().expect("create fake zbi file");
        let fake_zbi_path =
            Utf8Path::from_path(fake_zbi_file.path()).expect("encode fake zbi path");

        builder.add_slot_images(Slot::Primary(AssemblyManifest {
            images: vec![Image::ZBI { path: fake_zbi_path.to_path_buf(), signed: true }],
        }));

        builder.build().expect("build update package");

        let package_manifest = PackageManifest::try_load_from(
            Utf8Path::from_path(&temporary_directory.path().join("update_package_manifest.json"))
                .expect("update package manifest path"),
        )
        .expect("load package manifest");

        let hash = package_manifest.hash();
        let blobs = package_manifest
            .blobs()
            .into_iter()
            .map(|blob_info| {
                std::fs::read(&blob_info.source_path).expect("read blob from package manifest")
            })
            .collect::<Vec<_>>();
        FakeUpdatePackage { hash, blobs }
    });
}
