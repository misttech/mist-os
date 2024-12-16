// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for constructing product bundles, which are distributable containers for a product's
//! images and packages, and can be used to emulate, flash, or update a product.

use anyhow::{bail, ensure, Context, Result};
use assembly_manifest::{AssemblyManifest, BlobfsContents, Image, PackagesMetadata};
use assembly_partitions_config::{PartitionImageMapper, PartitionsConfig, Slot as PartitionSlot};
use assembly_tool::{SdkToolProvider, ToolProvider};
use assembly_update_package::{Slot, UpdatePackageBuilder};
use assembly_update_packages_manifest::UpdatePackagesManifest;
use camino::{Utf8Path, Utf8PathBuf};
use epoch::EpochFile;
use ffx_config::sdk::{in_tree_sdk_version, SdkVersion};
use ffx_config::EnvironmentContext;
use ffx_fastboot::manifest::FlashManifestVersion;
use ffx_product_create_args::CreateCommand;
use fho::{return_bug, FfxMain, FfxTool, SimpleWriter};
use fuchsia_pkg::PackageManifest;
use fuchsia_repo::repo_builder::RepoBuilder;
use fuchsia_repo::repo_keys::RepoKeys;
use fuchsia_repo::repository::FileSystemRepository;
use sdk_metadata::{
    ProductBundle, ProductBundleV2, Repository, VirtualDevice, VirtualDeviceManifest,
};
use std::fs::{self, File};
use tempfile::TempDir;
use walkdir::WalkDir;

/// Default delivery blob type to use for products.
const DEFAULT_DELIVERY_BLOB_TYPE: u32 = 1;

#[derive(FfxTool)]
pub struct ProductCreateTool {
    #[command]
    pub cmd: CreateCommand,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(ProductCreateTool);

/// Create a product bundle.
#[async_trait::async_trait(?Send)]
impl FfxMain for ProductCreateTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let sdk = self.ctx.get_sdk().await.context("getting sdk env context")?;
        let sdk_version = match sdk.get_version() {
            SdkVersion::Version(version) => version.to_string(),
            SdkVersion::InTree => in_tree_sdk_version(),
            SdkVersion::Unknown => return_bug!("Unable to determine SDK version"),
        };
        let tools = SdkToolProvider::try_new()?;
        pb_create_with_sdk_version(self.cmd, &sdk_version, Box::new(tools))
            .await
            .map_err(Into::into)
    }
}

/// Create a product bundle using the provided sdk.
pub async fn pb_create_with_sdk_version(
    cmd: CreateCommand,
    sdk_version: &str,
    tools: Box<dyn ToolProvider>,
) -> Result<()> {
    // We build an update package if `update_version_file` or `update_epoch` is provided.
    // If we decide to build an update package, we need to ensure that both of them
    // are provided.
    let update_details =
        if cmd.update_package_version_file.is_some() || cmd.update_package_epoch.is_some() {
            if cmd.tuf_keys.is_none() {
                anyhow::bail!("TUF keys must be provided to build an update package");
            }
            let version = cmd.update_package_version_file.ok_or_else(|| {
                anyhow::anyhow!("A version file must be provided to build an update package")
            })?;
            let epoch = cmd.update_package_epoch.ok_or_else(|| {
                anyhow::anyhow!("A epoch must be provided to build an update package")
            })?;
            Some((version, epoch))
        } else {
            None
        };

    // Make sure `out_dir` is created and empty.
    if cmd.out_dir.exists() {
        if cmd.out_dir == "" || cmd.out_dir == "/" {
            anyhow::bail!("Avoiding deletion of an unsafe out directory: {}", &cmd.out_dir);
        }
        std::fs::remove_dir_all(&cmd.out_dir).context("Deleting the out_dir")?;
    }
    std::fs::create_dir_all(&cmd.out_dir).context("Creating the out_dir")?;

    let partitions = load_partitions_config(&cmd.partitions, &cmd.out_dir.join("partitions"))?;
    let (system_a, packages_a) =
        load_assembly_manifest(&cmd.system_a, &cmd.out_dir.join("system_a"))?;
    let (system_b, _packages_b) =
        load_assembly_manifest(&cmd.system_b, &cmd.out_dir.join("system_b"))?;
    let (system_r, packages_r) =
        load_assembly_manifest(&cmd.system_r, &cmd.out_dir.join("system_r"))?;

    // We must assert that the board_name for the system of images matches the hardware_revision in
    // the partitions config, otherwise OTAs may not work.
    let ensure_system_board = |system: &AssemblyManifest| -> Result<()> {
        if partitions.hardware_revision != "" {
            ensure!(
                &system.board_name == &partitions.hardware_revision,
                format!(
                    "The system board_name ({}) must match the partitions.hardware_revision ({})",
                    &system.board_name, &partitions.hardware_revision
                )
            );
        }
        Ok(())
    };

    // Generate a size report for the images after mapping them to partitions.
    if let Some(size_report) = cmd.gerrit_size_report {
        let mut mapper = PartitionImageMapper::new(partitions.clone());
        if let Some(system) = &system_a {
            mapper.map_images_to_slot(&system.images, PartitionSlot::A)?;
            ensure_system_board(system)?;
        }
        if let Some(system) = &system_b {
            mapper.map_images_to_slot(&system.images, PartitionSlot::B)?;
            ensure_system_board(system)?;
        }
        if let Some(system) = &system_r {
            mapper.map_images_to_slot(&system.images, PartitionSlot::R)?;
            ensure_system_board(system)?;
        }
        mapper
            .generate_gerrit_size_report(&size_report, &cmd.product_name)
            .context("Generating image size report")?;
    }

    // Generate the update packages if necessary.
    let (_gen_dir, update_package_hash, update_packages) =
        if let Some((version, epoch)) = update_details {
            let epoch: EpochFile = EpochFile::Version1 { epoch };
            let gen_dir = TempDir::new().context("creating temporary directory")?;
            let mut builder = UpdatePackageBuilder::new(
                partitions.clone(),
                partitions.hardware_revision.clone(),
                version,
                epoch,
                Utf8Path::from_path(gen_dir.path())
                    .context("checking if temporary directory is UTF-8")?,
            );
            let mut all_packages = UpdatePackagesManifest::default();
            for (_path, package) in &packages_a {
                all_packages.add_by_manifest(&package)?;
            }
            builder.add_packages(all_packages);
            if let Some(manifest) = &system_a {
                builder.add_slot_images(Slot::Primary(manifest.clone()));
            }
            if let Some(manifest) = &system_r {
                builder.add_slot_images(Slot::Recovery(manifest.clone()));
            }
            let update_package = builder.build(tools)?;
            (Some(gen_dir), Some(update_package.merkle), update_package.package_manifests)
        } else {
            (None, None, vec![])
        };

    // We always create a blobs directory even if there is no repository, because tools that read
    // the product bundle inadvertently creates the blobs directory, which dirties the product
    // bundle, causing hermeticity errors.
    let repo_path = &cmd.out_dir;
    let blobs_path = repo_path.join("blobs");
    std::fs::create_dir(&blobs_path).context("Creating blobs directory")?;

    // When RBE is enabled, Bazel will skip empty directory. This will ensure
    // blobs directory still appear in the output dir.
    let ensure_file_path = blobs_path.join(".ensure-one-file");
    std::fs::File::create(&ensure_file_path).context("Creating ensure file")?;

    let repositories = if let Some(tuf_keys) = &cmd.tuf_keys {
        let main_metadata_path = repo_path.join("repository");
        let recovery_metadata_path = repo_path.join("recovery_repository");
        let keys_path = repo_path.join("keys");
        let delivery_blob_type = cmd.delivery_blob_type.unwrap_or(DEFAULT_DELIVERY_BLOB_TYPE);

        let repo_keys =
            RepoKeys::from_dir(tuf_keys.as_std_path()).context("Gathering repo keys")?;

        // Main slot.
        let repo = FileSystemRepository::builder(
            main_metadata_path.to_path_buf(),
            blobs_path.to_path_buf(),
        )
        .delivery_blob_type(delivery_blob_type.try_into()?)
        .build();
        RepoBuilder::create(&repo, &repo_keys)
            .add_package_manifests(packages_a.into_iter())
            .await?
            .add_package_manifests(update_packages.into_iter().map(|manifest| (None, manifest)))
            .await?
            .commit()
            .await
            .context("Building the repo")?;

        // Recovery slot.
        // We currently need this for scrutiny to find the recovery blobs.
        let recovery_repo = FileSystemRepository::builder(
            recovery_metadata_path.to_path_buf(),
            blobs_path.to_path_buf(),
        )
        .delivery_blob_type(delivery_blob_type.try_into()?)
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
        vec![Repository {
            name: "fuchsia.com".into(),
            metadata_path: main_metadata_path,
            blobs_path: blobs_path,
            delivery_blob_type,
            root_private_key_path: copy_file(tuf_keys.join("root.json"), &keys_path).ok(),
            targets_private_key_path: copy_file(tuf_keys.join("targets.json"), &keys_path).ok(),
            snapshot_private_key_path: copy_file(tuf_keys.join("snapshot.json"), &keys_path).ok(),
            timestamp_private_key_path: copy_file(tuf_keys.join("timestamp.json"), &keys_path).ok(),
        }]
    } else {
        vec![]
    };

    // Add the virtual devices, and determine the path to the manifest.
    let virtual_devices_path = if cmd.virtual_device.is_empty() {
        None
    } else {
        // Prepare a manifest for the virtual devices.
        let mut manifest = VirtualDeviceManifest::default();

        // Create the virtual_devices directory.
        let vd_dir = cmd.out_dir.join("virtual_devices");
        std::fs::create_dir_all(&vd_dir).context("Creating the virtual_devices directory.")?;

        for path in cmd.virtual_device {
            let device = VirtualDevice::try_load_from(&path)
                .with_context(|| format!("Parsing file as virtual device: '{}'", path))?;

            // Write the virtual device to the directory.
            let device_file_name =
                path.file_name().unwrap_or_else(|| panic!("Path has no file name: '{}'", path));
            let device_file_name = Utf8PathBuf::from(device_file_name);
            let path_in_pb = vd_dir.join(&device_file_name);
            device
                .write(&path_in_pb)
                .with_context(|| format!("Writing virtual device: {}", path_in_pb))?;

            // Add the virtual device to the manifest.
            let name = device.name().to_string();
            manifest.device_paths.insert(name, device_file_name);
        }

        // Write the manifest into the directory.
        manifest.recommended = cmd.recommended_device;
        let manifest_path = vd_dir.join("manifest.json");
        let manifest_file = File::create(&manifest_path)
            .with_context(|| format!("Couldn't create manifest file '{}'", manifest_path))?;
        serde_json::to_writer(manifest_file, &manifest)
            .context("Couldn't serialize manifest to disk.")?;

        Some(manifest_path)
    };

    let product_name = cmd.product_name.to_owned();
    let product_version = cmd.product_version.to_owned();

    let product_bundle = ProductBundleV2 {
        product_name,
        product_version,
        partitions,
        sdk_version: sdk_version.to_string(),
        system_a: system_a.map(|s| s.images),
        system_b: system_b.map(|s| s.images),
        system_r: system_r.map(|s| s.images),
        repositories,
        update_package_hash,
        virtual_devices_path,
    };
    let product_bundle = ProductBundle::V2(product_bundle);
    product_bundle.write(&cmd.out_dir).context("writing product bundle")?;

    if cmd.with_deprecated_flash_manifest {
        let manifest_path = cmd.out_dir.join("flash.json");
        let flash_manifest_file = File::create(&manifest_path)
            .with_context(|| format!("Couldn't create flash.json '{}'", manifest_path))?;
        FlashManifestVersion::from_product_bundle(&product_bundle)?.write(flash_manifest_file)?
    }

    Ok(())
}

/// Open and parse a PartitionsConfig from a path, copying the images into `out_dir`.
fn load_partitions_config(
    path: impl AsRef<Utf8Path>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<PartitionsConfig> {
    let path = path.as_ref().parent().context("Determine base path")?;
    let out_dir = out_dir.as_ref();

    std::fs::create_dir_all(&out_dir).context("Creating the out_dir")?;

    // This is invalid, causing a cycle since WalkDir would infinitely iterate
    // as we keep adding subdirectories/subfiles.
    if out_dir.canonicalize_utf8()?.starts_with(path.canonicalize_utf8()?) {
        bail!("out_dir {:?} cannot be nested in partitions_config base path {:?}", out_dir, path);
    }

    for entry in WalkDir::new(path).follow_links(true) {
        let entry = entry?;
        let entry_path = entry.path();

        let relative_path = entry_path.strip_prefix(path)?;
        let rebased_path = out_dir.join_os(relative_path);

        if entry.file_type().is_dir() {
            fs::create_dir_all(rebased_path).context("Create directory")?;
        } else if entry.file_type().is_file() {
            fs::copy(entry_path, rebased_path).context("Copy file")?;
        }
    }

    PartitionsConfig::try_load_from(out_dir.join("partitions_config.json"))
        .context("Loading partitions config")
}

/// Open and parse an AssemblyManifest from a path, copying the images into `out_dir`.
/// Returns None if the given path is None.
fn load_assembly_manifest(
    path: &Option<Utf8PathBuf>,
    out_dir: impl AsRef<Utf8Path>,
) -> Result<(Option<AssemblyManifest>, Vec<(Option<Utf8PathBuf>, PackageManifest)>)> {
    let out_dir = out_dir.as_ref();

    if let Some(path) = path {
        // Make sure `out_dir` is created.
        std::fs::create_dir_all(&out_dir).context("Creating the out_dir")?;

        let manifest = AssemblyManifest::try_load_from(path)
            .with_context(|| format!("Loading assembly manifest: {}", path))?;

        // Filter out the base package, and the blobfs contents.
        let mut images = Vec::new();
        let mut packages = Vec::new();
        let mut extract_packages = |packages_metadata| -> Result<()> {
            let PackagesMetadata { base, cache } = packages_metadata;
            let all_packages = [base.0, cache.0].concat();
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
        for image in manifest.images.into_iter() {
            match image {
                Image::BasePackage(..) => {}
                Image::Fxfs { path, contents } => {
                    extract_packages(contents.packages)?;
                    images.push(Image::Fxfs { path, contents: BlobfsContents::default() });
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

                // We don't need to extract packages from `FxfsSparse`, since it exists only if
                // `Fxfs` also exists (and always contains the same set of packages).
                Image::FxfsSparse { .. }
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

        Ok((
            Some(AssemblyManifest { images: new_images, board_name: manifest.board_name }),
            packages,
        ))
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

#[cfg(test)]
mod test {
    use super::*;
    use assembly_tool::testing::{blobfs_side_effect, FakeToolProvider};
    use fuchsia_repo::test_utils;
    use sdk_metadata::VirtualDeviceV1;
    use std::io::Write;

    const VIRTUAL_DEVICE_VALID: &str =
        include_str!("../../../../../../../build/sdk/meta/test_data/virtual_device.json");

    #[test]
    fn test_copy_file() {
        let temp1 = TempDir::new().unwrap();
        let tempdir1 = Utf8Path::from_path(temp1.path()).unwrap();
        let temp2 = TempDir::new().unwrap();
        let tempdir2 = Utf8Path::from_path(temp2.path()).unwrap();

        let source_path = tempdir1.join("source.txt");
        let mut source_file = File::create(&source_path).unwrap();
        write!(source_file, "contents").unwrap();
        let destination = copy_file(&source_path, tempdir2).unwrap();
        assert!(destination.exists());
    }

    #[test]
    fn test_load_partitions_config() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");
        fs::create_dir(&pb_dir).unwrap();

        let config_dir = tempdir.join("config");
        fs::create_dir(&config_dir).unwrap();
        let config_path = config_dir.join("partitions_config.json");
        let config_file = File::create(&config_path).unwrap();
        serde_json::to_writer(&config_file, &PartitionsConfig::default()).unwrap();

        let error_dir = tempdir.join("error");
        fs::create_dir(&error_dir).unwrap();
        let error_path = error_dir.join("partitions_config.json");
        let mut error_file = File::create(&error_path).unwrap();
        error_file.write_all("error".as_bytes()).unwrap();

        let nested_error_path = pb_dir.join("partitions_config.json");
        let nested_error_file = File::create(&nested_error_path).unwrap();
        serde_json::to_writer(&nested_error_file, &PartitionsConfig::default()).unwrap();

        let parsed = load_partitions_config(&config_path, &pb_dir);
        assert!(parsed.is_ok());

        let error = load_partitions_config(&error_path, &pb_dir);
        assert!(error.is_err());

        let nested_error = load_partitions_config(&nested_error_path, &pb_dir);
        assert!(nested_error.is_err());
    }

    #[test]
    fn test_load_assembly_manifest() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let manifest_path = tempdir.join("manifest.json");
        AssemblyManifest { images: Default::default(), board_name: "my_board".into() }
            .write(&manifest_path)
            .unwrap();

        let error_path = tempdir.join("error.json");
        let mut error_file = File::create(&error_path).unwrap();
        error_file.write_all("error".as_bytes()).unwrap();

        let (parsed, packages) = load_assembly_manifest(&Some(manifest_path), &pb_dir).unwrap();
        assert!(parsed.is_some());
        assert_eq!(packages, Vec::new());

        let error = load_assembly_manifest(&Some(error_path), &pb_dir);
        assert!(error.is_err());

        let (none, _) = load_assembly_manifest(&None, &pb_dir).unwrap();
        assert!(none.is_none());
    }

    #[fuchsia::test]
    async fn test_pb_create_minimal() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: None,
                system_b: None,
                system_r: None,
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: None,
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_path = tempdir.join("system.json");
        AssemblyManifest { images: Default::default(), board_name: "my_board".into() }
            .write(&system_path)
            .unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: Some(system_path.clone()),
                system_b: None,
                system_r: Some(system_path.clone()),
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: Some(vec![]),
                system_b: None,
                system_r: Some(vec![]),
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: None,
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r_with_multiple_zbi() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_path = tempdir.join("system.json");
        let mut manifest =
            AssemblyManifest { images: Default::default(), board_name: "my_board".into() };
        manifest.images = vec![
            Image::ZBI { path: Utf8PathBuf::from("path1"), signed: false },
            Image::ZBI { path: Utf8PathBuf::from("path2"), signed: true },
        ];
        manifest.write(&system_path).unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        assert!(pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: Some(system_path.clone()),
                system_b: None,
                system_r: Some(system_path.clone()),
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .is_err());
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r_and_repository() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_path = tempdir.join("system.json");
        AssemblyManifest { images: Default::default(), board_name: "my_board".into() }
            .write(&system_path)
            .unwrap();

        let tuf_keys = tempdir.join("keys");
        test_utils::make_repo_keys_dir(&tuf_keys);

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: Some(system_path.clone()),
                system_b: None,
                system_r: Some(system_path.clone()),
                tuf_keys: Some(tuf_keys),
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: Some(1),
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: Some(vec![]),
                system_b: None,
                system_r: Some(vec![]),
                repositories: vec![Repository {
                    name: "fuchsia.com".into(),
                    metadata_path: pb_dir.join("repository"),
                    blobs_path: pb_dir.join("blobs"),
                    delivery_blob_type: 1,
                    root_private_key_path: Some(pb_dir.join("keys/root.json")),
                    targets_private_key_path: Some(pb_dir.join("keys/targets.json")),
                    snapshot_private_key_path: Some(pb_dir.join("keys/snapshot.json")),
                    timestamp_private_key_path: Some(pb_dir.join("keys/timestamp.json")),
                }],
                update_package_hash: None,
                virtual_devices_path: None,
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_with_update() {
        let tmp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(tmp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let version_path = tempdir.join("version.txt");
        std::fs::write(&version_path, "").unwrap();

        let tuf_keys = tempdir.join("keys");
        test_utils::make_repo_keys_dir(&tuf_keys);

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: Some(tuf_keys),
                update_package_version_file: Some(version_path),
                update_package_epoch: Some(1),
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        // NB: do not assert on the package hash because this test is not hermetic; platform
        // changes such as API level bumps may change the package hash and such changes are
        // immaterial to the code under test here.
        assert_matches::assert_matches!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: _,
                product_version: _,
                partitions,
                sdk_version: _,
                system_a: None,
                system_b: None,
                system_r: None,
                repositories,
                update_package_hash: Some(_),
                virtual_devices_path: None,
            }) if partitions == Default::default() && repositories == &[Repository {
                name: "fuchsia.com".into(),
                metadata_path: pb_dir.join("repository"),
                blobs_path: pb_dir.join("blobs"),
                delivery_blob_type: DEFAULT_DELIVERY_BLOB_TYPE,
                root_private_key_path: Some(pb_dir.join("keys/root.json")),
                targets_private_key_path: Some(pb_dir.join("keys/targets.json")),
                snapshot_private_key_path: Some(pb_dir.join("keys/snapshot.json")),
                timestamp_private_key_path: Some(pb_dir.join("keys/timestamp.json")),
            }]
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_with_virtual_devices() -> Result<()> {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path)?;
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default())?;

        let vd_path1 = tempdir.join("device_1.json");
        let vd_path2 = tempdir.join("device_2.json");
        let template_path = tempdir.join("device_1.json.template");
        let mut vd_file1 = File::create(&vd_path1)?;
        let mut vd_file2 = File::create(&vd_path2)?;
        File::create(&template_path)?;
        vd_file1.write_all(VIRTUAL_DEVICE_VALID.as_bytes())?;
        vd_file2.write_all(VIRTUAL_DEVICE_VALID.as_bytes())?;

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: partitions_path,
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![vd_path1, vd_path2],
                recommended_device: Some("device_2".to_string()),
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: true,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: None,
                system_b: None,
                system_r: None,
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: Some(pb_dir.join("virtual_devices/manifest.json")),
            })
        );

        let internal_pb = match pb {
            ProductBundle::V2(pb) => pb,
        };

        let path = internal_pb.get_virtual_devices_path();
        let manifest =
            VirtualDeviceManifest::from_path(&path).context("Manifest file from_path")?;
        let default = manifest.default_device();
        assert!(matches!(default, Ok(Some(VirtualDevice::V1(_)))), "{:?}", default);

        let devices = manifest.device_names();
        assert_eq!(devices.len(), 2);
        assert!(devices.contains(&"device_1".to_string()));
        assert!(devices.contains(&"device_2".to_string()));

        let device1 = manifest.device("device_1");
        assert!(device1.is_ok(), "{:?}", device1.unwrap_err());
        assert!(matches!(device1, Ok(VirtualDevice::V1(VirtualDeviceV1 { .. }))));

        let device2 = manifest.device("device_2");
        assert!(device2.is_ok(), "{:?}", device2.unwrap_err());
        assert!(matches!(device2, Ok(VirtualDevice::V1(VirtualDeviceV1 { .. }))));

        Ok(())
    }
}
