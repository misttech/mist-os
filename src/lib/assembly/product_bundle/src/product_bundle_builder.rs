// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::product_bundle::ProductBundle;
use crate::v2::{ProductBundleV2, Repository};

use anyhow::{anyhow, ensure, Context, Result};
use assembled_system::{AssembledSystem, BlobfsContents, Image, PackagesMetadata};
use assembly_container::AssemblyContainer;
use assembly_partitions_config::{PartitionImageMapper, PartitionsConfig, Slot as PartitionSlot};
use assembly_release_info::ProductBundleReleaseInfo;
use assembly_tool::ToolProvider;
use assembly_update_package::{Slot, UpdatePackage, UpdatePackageBuilder};
use assembly_update_packages_manifest::UpdatePackagesManifest;
use camino::{Utf8Path, Utf8PathBuf};
use delivery_blob::DeliveryBlobType;
use epoch::EpochFile;
use fuchsia_pkg::PackageManifest;
use fuchsia_repo::repo_builder::RepoBuilder;
use fuchsia_repo::repo_keys::RepoKeys;
use fuchsia_repo::repository::FileSystemRepository;
use sdk_metadata::{VirtualDevice, VirtualDeviceManifest};
use std::collections::BTreeMap;
use std::fs::File;
use tempfile::TempDir;

/// Build a ProductBundle.
pub struct ProductBundleBuilder {
    product_bundle_name: String,
    product_bundle_version: String,
    sdk_version: String,
    partitions: Option<PartitionsConfig>,
    system_a: Option<AssembledSystem>,
    system_b: Option<AssembledSystem>,
    system_r: Option<AssembledSystem>,
    virtual_devices: BTreeMap<String, VirtualDevice>,
    recommended_virtual_device: Option<String>,
    update_details: Option<UpdateDetails>,
    repository_details: Option<RepositoryDetails>,
    gerrit_size_report: Option<Utf8PathBuf>,
}

/// The details needed to build an update package.
struct UpdateDetails {
    epoch: EpochFile,
    version_file: Utf8PathBuf,
}

/// The details needed to build a TUF repository.
struct RepositoryDetails {
    delivery_blob_type: DeliveryBlobType,
    tuf_keys: Utf8PathBuf,
}

impl ProductBundleBuilder {
    /// Construct a new ProductBundleBuilder.
    pub fn new(
        product_bundle_name: impl AsRef<str>,
        product_bundle_version: impl AsRef<str>,
    ) -> Self {
        let product_bundle_name = product_bundle_name.as_ref().into();
        let product_bundle_version = product_bundle_version.as_ref().into();
        Self {
            product_bundle_name,
            product_bundle_version,
            sdk_version: "not_built_from_sdk".into(),
            partitions: None,
            system_a: None,
            system_b: None,
            system_r: None,
            virtual_devices: BTreeMap::new(),
            recommended_virtual_device: None,
            update_details: None,
            repository_details: None,
            gerrit_size_report: None,
        }
    }

    /// Set the SDK version if built from the SDK.
    pub fn sdk_version(mut self, version: String) -> Self {
        self.sdk_version = version;
        self
    }

    /// Set a partitions config directly into the PB instead of fetching from
    /// the board.
    pub fn partitions(mut self, partitions: PartitionsConfig) -> Self {
        self.partitions = Some(partitions);
        self
    }

    /// Assign an assembled system to a particular slot.
    pub fn system(mut self, system: AssembledSystem, slot: PartitionSlot) -> Self {
        match slot {
            PartitionSlot::A => self.system_a = Some(system),
            PartitionSlot::B => self.system_b = Some(system),
            PartitionSlot::R => self.system_r = Some(system),
        }
        self
    }

    /// Add a virtual device with a desired `file_name`.
    /// The `file_name` is necessary so that GN can depend on a consistent
    /// output file.
    pub fn virtual_device(mut self, file_name: impl AsRef<str>, device: VirtualDevice) -> Self {
        self.virtual_devices.insert(file_name.as_ref().into(), device);
        self
    }

    /// Set a virtual device to use by default.
    pub fn recommended_virtual_device(mut self, name: impl AsRef<str>) -> Self {
        self.recommended_virtual_device = Some(name.as_ref().into());
        self
    }

    /// Add an update package.
    pub fn update_package(mut self, version_file: impl AsRef<Utf8Path>, epoch: u64) -> Self {
        let epoch: EpochFile = EpochFile::Version1 { epoch };
        let version_file = version_file.as_ref().to_path_buf();
        self.update_details = Some(UpdateDetails { epoch, version_file });
        self
    }

    /// Write an image size report for gerrit.
    pub fn gerrit_size_report(mut self, output: impl AsRef<Utf8Path>) -> Self {
        self.gerrit_size_report = Some(output.as_ref().to_path_buf());
        self
    }

    /// Add a TUF repository.
    pub fn repository(
        mut self,
        delivery_blob_type: DeliveryBlobType,
        tuf_keys: impl AsRef<Utf8Path>,
    ) -> Self {
        let tuf_keys = tuf_keys.as_ref().to_path_buf();
        self.repository_details = Some(RepositoryDetails { delivery_blob_type, tuf_keys });
        self
    }

    /// Build the ProductBundle and write to `out_dir`.
    pub async fn build(
        self,
        tools: Box<dyn ToolProvider>,
        out_dir: impl AsRef<Utf8Path>,
    ) -> Result<ProductBundle> {
        let ProductBundleBuilder {
            product_bundle_name,
            product_bundle_version,
            sdk_version,
            partitions,
            system_a,
            system_b,
            system_r,
            virtual_devices,
            recommended_virtual_device,
            update_details,
            repository_details,
            gerrit_size_report,
        } = self;

        // Make sure `out_dir` is created and empty.
        let out_dir = out_dir.as_ref();
        if out_dir.exists() {
            if out_dir == "" || out_dir == "/" {
                anyhow::bail!("Avoiding deletion of an unsafe out directory: {}", &out_dir);
            }
            std::fs::remove_dir_all(&out_dir).context("Deleting the out_dir")?;
        }
        std::fs::create_dir_all(&out_dir).context("Creating the out_dir")?;

        // Write the systems to the `out_dir`, and extract the packages.
        let (system_a, packages_a) = write_assembled_system(system_a, out_dir.join("system_a"))?;
        let (system_b, _packages_b) = write_assembled_system(system_b, out_dir.join("system_b"))?;
        let (system_r, packages_r) = write_assembled_system(system_r, out_dir.join("system_r"))?;

        // Write the partitions config to `out_dir`.
        let partitions = write_partitions(partitions, &system_a, &system_b, &system_r, &out_dir)?;

        // Write the gerrit image size report.
        if let Some(gerrit_size_report) = gerrit_size_report {
            write_size_report(
                &partitions,
                &system_a,
                &system_b,
                &system_r,
                &product_bundle_name,
                gerrit_size_report,
            )?;
        }

        // Write the update package.
        let gen_dir = TempDir::new().context("creating temporary directory")?;
        let gen_dir_path = Utf8Path::from_path(gen_dir.path())
            .context("checking if temporary directory is UTF-8")?;
        let update_package = if let Some(update_details) = update_details {
            Some(write_update_package(
                update_details,
                &packages_a,
                &system_a,
                &system_r,
                &partitions,
                tools,
                gen_dir_path,
            )?)
        } else {
            None
        };
        let update_package_hash = update_package.as_ref().map(|u| u.merkle.clone());

        // We always create a blobs directory even if there is no repository, because tools that read
        // the product bundle inadvertently creates the blobs directory, which dirties the product
        // bundle, causing hermeticity errors.
        let blobs_path = out_dir.join("blobs");
        std::fs::create_dir(&blobs_path).context("Creating blobs directory")?;

        // When RBE is enabled, Bazel will skip empty directory. This will ensure
        // blobs directory still appear in the output dir.
        let ensure_file_path = blobs_path.join(".ensure-one-file");
        std::fs::File::create(&ensure_file_path).context("Creating ensure file")?;

        // Write the repositories.
        let repositories = if let Some(repository_details) = repository_details {
            write_repositories(
                repository_details,
                update_package,
                packages_a,
                packages_r,
                blobs_path,
                out_dir,
            )
            .await?
        } else {
            vec![]
        };

        // Write the virtual devices.
        let virtual_devices_path = if !virtual_devices.is_empty() {
            Some(write_virtual_devices(
                virtual_devices,
                out_dir.join("virtual_devices"),
                recommended_virtual_device,
            )?)
        } else {
            None
        };

        // Collect the release information.
        let release_info = Some(ProductBundleReleaseInfo {
            name: product_bundle_name.clone(),
            version: product_bundle_version.clone(),
            sdk_version: sdk_version.clone(),
            system_a: system_a.as_ref().and_then(|s| s.system_release_info.clone()),
            system_b: system_b.as_ref().and_then(|s| s.system_release_info.clone()),
            system_r: system_r.as_ref().and_then(|s| s.system_release_info.clone()),
        });

        // Construct the product bundle.
        let product_bundle = ProductBundle::V2(ProductBundleV2 {
            product_name: product_bundle_name,
            product_version: product_bundle_version,
            partitions,
            sdk_version,
            system_a: system_a.map(|s| s.images),
            system_b: system_b.map(|s| s.images),
            system_r: system_r.map(|s| s.images),
            repositories,
            update_package_hash,
            virtual_devices_path,
            release_info,
        });
        product_bundle
            .write(out_dir)
            .with_context(|| format!("Writing product bundle: {}", out_dir))?;
        Ok(product_bundle)
    }
}

/// Find the partitions config, complete some checks, and write it to `out_dir`.
fn write_partitions(
    partitions: Option<PartitionsConfig>,
    system_a: &Option<AssembledSystem>,
    system_b: &Option<AssembledSystem>,
    system_r: &Option<AssembledSystem>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<PartitionsConfig> {
    let out_dir = out_dir.as_ref();

    // Load the partitions config from the boards and product bundle, and
    // ensure they are all identical.
    let mut chosen_partitions: Option<(PartitionsConfig, bool)> = partitions.map(|p| (p, true));
    let mut maybe_add_partitions_config = |path: Option<&Utf8PathBuf>| -> Result<()> {
        if let Some(path) = path {
            let another_config = PartitionsConfig::from_dir(&path)
                .with_context(|| format!("Parsing partitions config: {}", &path))?;

            match &chosen_partitions {
                // No chosen partitions yet, so just save it.
                None => chosen_partitions = Some((another_config, false)),

                // Chosen partitions was from a PB.
                // Always clobber it.
                Some((_, true)) => chosen_partitions = Some((another_config, false)),

                // Chosen and new partitions are from boards.
                // Assert they are equal.
                Some((current_config, false)) => {
                    ensure!(
                        current_config.contents_eq(&another_config)?,
                        "The partitions config ({}) does not match the partitions config ({})",
                        another_config.hardware_revision,
                        current_config.hardware_revision
                    );
                }
            }
        }
        Ok(())
    };
    maybe_add_partitions_config(partitions_from_system(system_a.as_ref()))?;
    maybe_add_partitions_config(partitions_from_system(system_b.as_ref()))?;
    maybe_add_partitions_config(partitions_from_system(system_r.as_ref()))?;

    let partitions = chosen_partitions.ok_or_else(|| anyhow!("Missing a partitions config"))?.0;
    let partitions = partitions.write_to_dir(out_dir.join("partitions"), None::<Utf8PathBuf>)?;
    Ok(partitions)
}

/// Write the update package to `out_dir`.
fn write_update_package(
    update_details: UpdateDetails,
    packages: &Vec<(Option<Utf8PathBuf>, PackageManifest)>,
    system_a: &Option<AssembledSystem>,
    system_r: &Option<AssembledSystem>,
    partitions: &PartitionsConfig,
    tools: Box<dyn ToolProvider>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<UpdatePackage> {
    let out_dir = out_dir.as_ref();

    let mut builder = UpdatePackageBuilder::new(
        partitions.clone(),
        partitions.hardware_revision.clone(),
        update_details.version_file,
        update_details.epoch,
        out_dir,
    );
    let mut all_packages = UpdatePackagesManifest::default();
    for (_path, package) in packages {
        all_packages.add_by_manifest(&package)?;
    }
    builder.add_packages(all_packages);
    if let Some(manifest) = &system_a {
        builder.add_slot_images(Slot::Primary(manifest.clone()));
    }
    if let Some(manifest) = &system_r {
        builder.add_slot_images(Slot::Recovery(manifest.clone()));
    }
    builder.build(tools)
}

/// Write the TUF repositories to `out_dir` and the blobs to `blobs_path`.
async fn write_repositories(
    repository_details: RepositoryDetails,
    update_package: Option<UpdatePackage>,
    packages_a: Vec<(Option<Utf8PathBuf>, PackageManifest)>,
    packages_r: Vec<(Option<Utf8PathBuf>, PackageManifest)>,
    blobs_path: impl AsRef<Utf8Path>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<Vec<Repository>> {
    let tuf_keys = repository_details.tuf_keys;
    let blobs_path = blobs_path.as_ref();
    let out_dir = out_dir.as_ref();

    let main_metadata_path = out_dir.join("repository");
    let recovery_metadata_path = out_dir.join("recovery_repository");
    let keys_path = out_dir.join("keys");

    let repo_keys = RepoKeys::from_dir(tuf_keys.as_std_path()).context("Gathering repo keys")?;

    // Main slot.
    let repo =
        FileSystemRepository::builder(main_metadata_path.to_path_buf(), blobs_path.to_path_buf())
            .delivery_blob_type(repository_details.delivery_blob_type)
            .build();
    let mut repo_builder = RepoBuilder::create(&repo, &repo_keys)
        .add_package_manifests(packages_a.into_iter())
        .await?;
    if let Some(update_package) = update_package {
        repo_builder = repo_builder
            .add_package_manifests(
                update_package.package_manifests.into_iter().map(|manifest| (None, manifest)),
            )
            .await?;
    }
    repo_builder.commit().await.context("Building the repo")?;

    // Recovery slot.
    // We currently need this for scrutiny to find the recovery blobs.
    let recovery_repo = FileSystemRepository::builder(
        recovery_metadata_path.to_path_buf(),
        blobs_path.to_path_buf(),
    )
    .delivery_blob_type(repository_details.delivery_blob_type)
    .build();
    RepoBuilder::create(&recovery_repo, &repo_keys)
        .add_package_manifests(packages_r.into_iter())
        .await?
        .commit()
        .await
        .context("Building the recovery repo")?;

    std::fs::create_dir_all(&keys_path).context("Creating keys directory")?;

    // We intentionally do not add the recovery repository, because no tools currently need
    // it. Scrutiny needs the recovery blobs to be accessible, but that's it.
    Ok(vec![Repository {
        name: "fuchsia.com".into(),
        metadata_path: main_metadata_path,
        blobs_path: blobs_path.into(),
        delivery_blob_type: repository_details.delivery_blob_type.into(),
        root_private_key_path: copy_file(tuf_keys.join("root.json"), &keys_path).ok(),
        targets_private_key_path: copy_file(tuf_keys.join("targets.json"), &keys_path).ok(),
        snapshot_private_key_path: copy_file(tuf_keys.join("snapshot.json"), &keys_path).ok(),
        timestamp_private_key_path: copy_file(tuf_keys.join("timestamp.json"), &keys_path).ok(),
    }])
}

/// Collect the partitions config from an AssembledSystem.
fn partitions_from_system<'a>(system: Option<&'a AssembledSystem>) -> Option<&'a Utf8PathBuf> {
    system.map(|a| a.partitions_config.as_ref().map(|p| p.as_utf8_path_buf())).flatten()
}

/// Copy the images from an AssembledSystem to `out_dir`, and return a new
/// AssembledSystem with the new paths and the `contents` removed.
fn write_assembled_system(
    system: Option<AssembledSystem>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<(Option<AssembledSystem>, Vec<(Option<Utf8PathBuf>, PackageManifest)>)> {
    let out_dir = out_dir.as_ref();
    if let Some(system) = system {
        // Make sure `out_dir` is created.
        std::fs::create_dir_all(&out_dir).context("Creating the out_dir")?;

        // Filter out the base package, and the blobfs contents.
        let mut images = Vec::new();
        let mut packages = Vec::new();
        let mut extract_packages = |packages_metadata| -> Result<()> {
            let PackagesMetadata { base, cache } = packages_metadata;
            let all_packages = [base.metadata, cache.metadata].concat();
            for package in all_packages {
                let manifest = PackageManifest::try_load_from(&package.manifest)
                    .with_context(|| format!("reading package manifest: {}", package.manifest))?;
                packages.push((Some(package.manifest), manifest));
            }
            Ok(())
        };
        let mut has_zbi = false;
        let mut has_vbmeta = false;
        let mut has_dtbo = false;
        for image in system.images.into_iter() {
            match image {
                Image::BasePackage(..) => {}
                Image::FxfsSparse { path, contents } => {
                    extract_packages(contents.packages)?;
                    images.push(Image::FxfsSparse { path, contents: BlobfsContents::default() });
                }
                Image::BlobFS { path, contents } => {
                    extract_packages(contents.packages)?;
                    images.push(Image::BlobFS { path, contents: BlobfsContents::default() });
                }
                Image::ZBI { .. } => {
                    if has_zbi {
                        anyhow::bail!("Found more than one ZBI");
                    }
                    images.push(image);
                    has_zbi = true;
                }
                Image::VBMeta(_) => {
                    if has_vbmeta {
                        anyhow::bail!("Found more than one VBMeta");
                    }
                    images.push(image);
                    has_vbmeta = true;
                }
                Image::Dtbo(_) => {
                    if has_dtbo {
                        anyhow::bail!("Found more than one Dtbo");
                    }
                    images.push(image);
                    has_dtbo = true;
                }

                Image::Fxfs(_)
                | Image::FVM(_)
                | Image::FVMSparse(_)
                | Image::FVMFastboot(_)
                | Image::QemuKernel(_) => {
                    images.push(image);
                }
            }
        }

        // Copy the images to the `out_dir`.
        let mut new_images = Vec::<Image>::new();
        for mut image in images.into_iter() {
            let dest = copy_file(image.source(), &out_dir)?;
            image.set_source(dest);
            new_images.push(image);
        }

        Ok((Some(AssembledSystem { images: new_images, ..system }), packages))
    } else {
        Ok((None, vec![]))
    }
}

/// Copy a file from `source` to `out_dir` preserving the filename.
/// Returns the destination, which is equal to {out_dir}{filename}.
fn copy_file(source: impl AsRef<Utf8Path>, out_dir: impl AsRef<Utf8Path>) -> Result<Utf8PathBuf> {
    let source = source.as_ref();
    let out_dir = out_dir.as_ref();
    let filename = source.file_name().context("getting file name")?;
    let destination = out_dir.join(filename);

    // Attempt to hardlink, if that fails, fall back to copying.
    if let Err(_) = std::fs::hard_link(source, &destination) {
        // falling back to copying.
        std::fs::copy(source, &destination)
            .with_context(|| format!("copying file '{}'", source))?;
    }
    Ok(destination)
}

/// Generate and write an image size report to `output`.
fn write_size_report(
    partitions: &PartitionsConfig,
    system_a: &Option<AssembledSystem>,
    system_b: &Option<AssembledSystem>,
    system_r: &Option<AssembledSystem>,
    product_bundle_name: &String,
    output: impl AsRef<Utf8Path>,
) -> Result<()> {
    let output = output.as_ref().to_path_buf();
    let mut mapper = PartitionImageMapper::new(partitions.clone())?;
    if let Some(system) = system_a {
        mapper.map_images_to_slot(&system.images, PartitionSlot::A)?;
    }
    if let Some(system) = system_b {
        mapper.map_images_to_slot(&system.images, PartitionSlot::B)?;
    }
    if let Some(system) = system_r {
        mapper.map_images_to_slot(&system.images, PartitionSlot::R)?;
    }
    mapper
        .generate_gerrit_size_report(&output, product_bundle_name)
        .context("Generating image size report")?;
    Ok(())
}

/// Writes the virtual devices to `out_dir`, and returns the path to the manifest.
fn write_virtual_devices(
    virtual_devices: BTreeMap<String, VirtualDevice>,
    out_dir: impl AsRef<Utf8Path>,
    recommended: Option<String>,
) -> Result<Utf8PathBuf> {
    let out_dir = out_dir.as_ref();
    let mut manifest = VirtualDeviceManifest::default();
    manifest.recommended = recommended;

    // Create the virtual_devices directory.
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Creating the virtual_devices directory: {}", out_dir))?;

    for (file_name, virtual_device) in virtual_devices {
        // Write the virtual device to the directory.
        let name = virtual_device.name().to_string();
        let device_file_name = Utf8PathBuf::from(&file_name);
        let device_file_path = out_dir.join(&device_file_name);
        virtual_device
            .write(&device_file_path)
            .with_context(|| format!("Writing virtual device: {}", device_file_path))?;

        // Add the virtual device to the manifest.
        manifest.device_paths.insert(name, device_file_name);
    }

    // Write the manifest into the directory.
    let manifest_path = out_dir.join("manifest.json");
    let manifest_file = File::create(&manifest_path)
        .with_context(|| format!("Creating virtual device manifest: {}", &manifest_path))?;
    serde_json::to_writer(manifest_file, &manifest)
        .with_context(|| format!("Writing virtual device manifest: {}", &manifest_path))?;

    Ok(manifest_path)
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use super::{ProductBundleBuilder, Repository};
    use crate::product_bundle::ProductBundle;
    use crate::v2::ProductBundleV2;

    use assembled_system::{AssembledSystem, Image};
    use assembly_container::{AssemblyContainer, DirectoryPathBuf};
    use assembly_partitions_config::{Partition, PartitionsConfig, Slot};
    use assembly_release_info::ProductBundleReleaseInfo;
    use assembly_tool::testing::{blobfs_side_effect, FakeToolProvider};
    use camino::{Utf8Path, Utf8PathBuf};
    use fuchsia_repo::test_utils;
    use pretty_assertions::assert_eq;
    use sdk_metadata::virtual_device::Hardware;
    use sdk_metadata::{VirtualDevice, VirtualDeviceV1};
    use tempfile::TempDir;

    #[fuchsia::test]
    async fn test_minimum() {
        let tools = FakeToolProvider::default();
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();

        let partitions = PartitionsConfig::default();
        let partitions_path = tempdir.join("partitions");
        partitions.write_to_dir(&partitions_path, None::<Utf8PathBuf>).unwrap();

        let system = AssembledSystem {
            images: vec![],
            board_name: "board_name".into(),
            partitions_config: Some(DirectoryPathBuf::new(partitions_path)),
            system_release_info: None,
        };

        let product_bundle_path = tempdir.join("pb");
        let product_bundle = ProductBundleBuilder::new("name", "version")
            .system(system, Slot::A)
            .build(Box::new(tools), &product_bundle_path)
            .await
            .unwrap();

        let expected = ProductBundle::V2(ProductBundleV2 {
            product_name: "name".into(),
            product_version: "version".into(),
            partitions: PartitionsConfig::default(),
            sdk_version: "not_built_from_sdk".into(),
            system_a: Some(vec![]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
            release_info: Some(ProductBundleReleaseInfo {
                name: "name".to_string(),
                version: "version".to_string(),
                sdk_version: "not_built_from_sdk".to_string(),
                system_a: None,
                system_b: None,
                system_r: None,
            }),
        });
        assert_eq!(expected, product_bundle);
    }

    #[fuchsia::test]
    async fn test_full() {
        let tools = FakeToolProvider::new_with_side_effect(blobfs_side_effect);
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();

        // Write a test zbi.
        let zbi_path = tempdir.join("fuchsia.zbi");
        let mut zbi_file = std::fs::File::create(&zbi_path).unwrap();
        zbi_file.write_all(b"zbi contents").unwrap();

        // Write a test version file for the update package.
        let version_path = tempdir.join("version.txt");
        let mut version_file = std::fs::File::create(&version_path).unwrap();
        version_file.write_all(b"1.2.3.4").unwrap();

        // Write the test key for the repository.
        let tuf_keys = tempdir.join("keys");
        test_utils::make_repo_keys_dir(&tuf_keys);

        // Write the partitions config.
        let partitions = PartitionsConfig {
            hardware_revision: "hw".into(),
            partitions: vec![Partition::ZBI {
                name: "zbi_a".into(),
                slot: Slot::A,
                size: Some(60),
            }],
            ..Default::default()
        };
        let partitions_path = tempdir.join("partitions");
        partitions.write_to_dir(&partitions_path, None::<Utf8PathBuf>).unwrap();

        // Construct the system.
        let system = AssembledSystem {
            images: vec![Image::ZBI { path: zbi_path, signed: false }],
            board_name: "board_name".into(),
            partitions_config: Some(DirectoryPathBuf::new(partitions_path)),
            system_release_info: None,
        };

        // Construct the PB.
        let size_report_path = tempdir.join("size_report.json");
        let product_bundle_path = tempdir.join("pb");
        let product_bundle = ProductBundleBuilder::new("name", "version")
            .sdk_version("custom_sdk_version".into())
            .system(system, Slot::A)
            .virtual_device(
                "vd_file_name",
                VirtualDevice::V1(VirtualDeviceV1::new("my_virtual_device", Hardware::default())),
            )
            .recommended_virtual_device("my_virtual_device")
            .update_package(version_path, 42)
            .repository(delivery_blob::DeliveryBlobType::Type1, tuf_keys)
            .gerrit_size_report(&size_report_path)
            .build(Box::new(tools), &product_bundle_path)
            .await
            .unwrap();

        // Ensure the PB is correct.
        let expected = ProductBundle::V2(ProductBundleV2 {
            product_name: "name".into(),
            product_version: "version".into(),
            partitions: PartitionsConfig {
                hardware_revision: "hw".into(),
                partitions: vec![Partition::ZBI {
                    name: "zbi_a".into(),
                    slot: Slot::A,
                    size: Some(60),
                }],
                ..Default::default()
            },
            sdk_version: "custom_sdk_version".into(),
            system_a: Some(vec![Image::ZBI {
                path: product_bundle_path.join("system_a/fuchsia.zbi"),
                signed: false,
            }]),
            system_b: None,
            system_r: None,
            repositories: vec![Repository {
                name: "fuchsia.com".into(),
                metadata_path: product_bundle_path.join("repository"),
                blobs_path: product_bundle_path.join("blobs"),
                delivery_blob_type: 1,
                root_private_key_path: Some(product_bundle_path.join("keys/root.json")),
                targets_private_key_path: Some(product_bundle_path.join("keys/targets.json")),
                snapshot_private_key_path: Some(product_bundle_path.join("keys/snapshot.json")),
                timestamp_private_key_path: Some(product_bundle_path.join("keys/timestamp.json")),
            }],
            update_package_hash: Some(
                "4198e7b88cc98aa87b16afa134e1f1ec8580fd9105f7db399adf6ff65426b49c".parse().unwrap(),
            ),
            virtual_devices_path: Some(product_bundle_path.join("virtual_devices/manifest.json")),
            release_info: Some(ProductBundleReleaseInfo {
                name: "name".to_string(),
                version: "version".to_string(),
                sdk_version: "custom_sdk_version".to_string(),
                system_a: None,
                system_b: None,
                system_r: None,
            }),
        });
        assert_eq!(expected, product_bundle);

        // Fetch the VD by name.
        // Yes... vd_file_name is correct although unexpected.
        let virtual_device = product_bundle.get_device(&Some("my_virtual_device".into())).unwrap();
        assert_eq!("vd_file_name", virtual_device.name.as_str());

        // Fetch the VD as the default/recommended.
        let virtual_device = product_bundle.get_device(&None).unwrap();
        assert_eq!("vd_file_name", virtual_device.name.as_str());

        // Check the size report.
        let size_report_file = std::fs::File::open(size_report_path).unwrap();
        let size_report: serde_json::Value = serde_json::from_reader(size_report_file).unwrap();
        let size_report = size_report.as_object().unwrap();
        assert_eq!(size_report.get("name-zbi_a").unwrap(), 12);
        assert_eq!(size_report.get("name-zbi_a.budget").unwrap(), 60);
    }
}
