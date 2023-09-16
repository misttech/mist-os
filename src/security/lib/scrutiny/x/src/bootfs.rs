// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::api::Blob as _;
use super::blob::BlobOpenError;
use super::blob::BlobSet;
use super::blob::VerifiedMemoryBlob;
use super::data_source as ds;
use super::package::Error as PackageError;
use super::package::Package;
use fidl::unpersist;
use fidl::Error as FidlError;
use fidl_fuchsia_component_internal as component_internal;
use fuchsia_url::boot_url::BootUrl;
use routing::config::RuntimeConfig;
use scrutiny_utils::key_value::parse_key_value;
use std::collections::HashMap;
use std::io::Error as IoError;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::Utf8Error;
use thiserror::Error;

/// The path of the additional boot configuration in bootfs.
///
/// See https://fuchsia.dev/fuchsia-src/reference/kernel/kernel_cmdline
/// for some information about how these options are used by the
/// component manager, and `api::AdditionalBootConfiguration` for
/// Scrutiny's public interface.
const ADDITIONAL_BOOT_ARGS_PATH: &str = "config/additional_boot_args";

/// The path of the component manager configuration in bootfs.
///
/// See //sdk/fidl/fuchsia.component.internal/config.fidl for the FIDL
/// definition, and `api::ComponentManagerConfiguration` for Scrutiny's
/// public interface to the configuration.
const COMPONENT_MANAGER_CONFIG_PATH: &str = "config/component_manager";

/// The path of the package index file for packages stored in bootfs.
const BOOTFS_PACKAGE_INDEX_PATH: &str = "data/bootfs_packages";

#[derive(Clone)]
pub(crate) struct Bootfs(Rc<BootfsData>);

impl Bootfs {
    pub fn new<
        BlobsByPath: Clone + IntoIterator<Item = (Box<dyn api::Path>, VerifiedMemoryBlob)>,
    >(
        data_source: ds::DataSource,
        blobs_by_path: BlobsByPath,
    ) -> Self {
        let blobs_by_hash = blobs_by_path
            .clone()
            .into_iter()
            .map(|(_path, blob)| (blob.hash(), blob))
            .collect::<HashMap<_, _>>();
        let blobs_by_path = blobs_by_path.into_iter().collect::<HashMap<_, _>>();
        Self(Rc::new(BootfsData { data_source, blobs_by_path, blobs_by_hash }))
    }
}

impl api::Bootfs for Bootfs {
    fn content_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        Box::new(self.0.blobs_by_path.clone().into_iter().map(|(path, verified_memory_blob)| {
            let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
            (path, blob)
        }))
    }

    fn additional_boot_configuration(
        &self,
    ) -> Result<Box<dyn api::AdditionalBootConfiguration>, api::BootfsError> {
        let path: Box<dyn api::Path> = Box::new(Path::new(ADDITIONAL_BOOT_ARGS_PATH));
        match self.0.blobs_by_path.get(&path) {
            Some(blob) => Ok(Box::new(AdditionalBootConfiguration::new(blob)?)),
            None => Err(AdditionalBootConfigurationError::FileNotFound { path })?,
        }
    }

    fn component_manager_configuration(
        &self,
    ) -> Result<Box<dyn api::ComponentManagerConfiguration>, api::BootfsError> {
        let path: Box<dyn api::Path> = Box::new(Path::new(COMPONENT_MANAGER_CONFIG_PATH));
        match self.0.blobs_by_path.get(&path) {
            Some(blob) => Ok(Box::new(ComponentManagerConfiguration::new(blob)?)),
            None => Err(ComponentManagerConfigurationError::FileNotFound { path })?,
        }
    }

    fn packages(
        &self,
    ) -> Result<Box<dyn Iterator<Item = Box<dyn api::Package>>>, api::BootfsError> {
        let path: Box<dyn api::Path> = Box::new(Path::new(BOOTFS_PACKAGE_INDEX_PATH));
        let pkg_index_blob = match self.0.blobs_by_path.get(&path) {
            Some(blob) => Ok(blob),
            None => Err(BootfsPackageIndexError::IndexNotFound { path }),
        }?;

        let mut reader =
            pkg_index_blob.reader_seeker().map_err(|err| BootfsPackageIndexError::BlobRead(err))?;
        let mut pkg_index_blob_contents = Vec::<u8>::new();
        reader
            .read_to_end(&mut pkg_index_blob_contents)
            .map_err(|err| BootfsPackageIndexError::Io(err))?;

        let pkg_index_str = std::str::from_utf8(&pkg_index_blob_contents)
            .map_err(|err| BootfsPackageIndexError::ParseUtf8(err))?;
        let pkg_index = parse_key_value(pkg_index_str)
            .map_err(|err| BootfsPackageIndexError::ParseIndex(err))?;

        let packages = pkg_index
            .iter()
            .map(|(name_and_variant, merkle)| {
                let pkg_path: Box<dyn api::Path> =
                    Box::new([format!("blob/{}", merkle)].iter().collect::<PathBuf>());
                let meta_far = match self.0.blobs_by_path.get(&pkg_path) {
                    Some(blob) => Ok(Box::new(blob.clone()) as Box<dyn api::Blob>),
                    None => Err(BootfsPackageError::MetaFarNotFound { path: pkg_path }),
                }?;
                let package = Package::new(
                    Some(self.0.data_source.clone()),
                    api::PackageResolverUrl::Boot(
                        BootUrl::new_path(format!("/{}", name_and_variant))
                            .expect("fuchsia boot url from package name and variant"),
                    ),
                    meta_far,
                    Box::new(self.clone()),
                )
                .map_err(|err| BootfsPackageError::CreatePackage(err))?;
                Ok(Box::new(package) as Box<dyn api::Package>)
            })
            .collect::<Result<Vec<_>, api::BootfsError>>()?;
        Ok(Box::new(packages.into_iter()))
    }
}

impl BlobSet for Bootfs {
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        let blobs = self
            .0
            .blobs_by_hash
            .iter()
            .map(|(_path, blob)| Box::new(blob.clone()) as Box<dyn api::Blob>)
            .collect::<Vec<_>>();
        Box::new(blobs.into_iter())
    }

    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError> {
        self.0
            .blobs_by_hash
            .get(&hash)
            .ok_or_else(|| BlobOpenError::BlobNotFound { hash, directory: None })
            .map(|blob| Box::new(blob.clone()) as Box<dyn api::Blob>)
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new([Box::new(self.0.data_source.clone()) as Box<dyn api::DataSource>].into_iter())
    }
}

struct BootfsData {
    data_source: ds::DataSource,
    blobs_by_path: HashMap<Box<dyn api::Path>, VerifiedMemoryBlob>,
    blobs_by_hash: HashMap<Box<dyn api::Hash>, VerifiedMemoryBlob>,
}

#[derive(Debug, Error)]
pub enum BootfsPackageIndexError {
    #[error("no package index file found in bootfs at path: {path}")]
    IndexNotFound { path: Box<dyn api::Path> },
    #[error("failed to read blob: {0}")]
    BlobRead(#[from] api::BlobError),
    #[error("failed to perform io for blob: {0}")]
    Io(#[from] IoError),
    #[error("failed to parse blob as utf-8: {0}")]
    ParseUtf8(#[from] Utf8Error),
    #[error("failed to parse bootfs package index to key/value pairs: {0}")]
    ParseIndex(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum BootfsPackageError {
    #[error("no meta.far file found in bootfs at path: {path}")]
    MetaFarNotFound { path: Box<dyn api::Path> },
    #[error("failed to instantiate package: {0}")]
    CreatePackage(#[from] PackageError),
}

#[derive(Debug, Error)]
pub enum AdditionalBootConfigurationError {
    #[error("no configuration file found in bootfs at path: {path}")]
    FileNotFound { path: Box<dyn api::Path> },
    #[error("failed to read blob: {0}")]
    BlobRead(#[from] api::BlobError),
    #[error("failed to perform io for blob: {0}")]
    Io(#[from] IoError),
    #[error("failed to parse blob as utf-8: {0}")]
    ParseUtf8(#[from] Utf8Error),
    #[error("failed to parse additional boot configuration at line {line_num}: {message}")]
    ParseConfiguration { line_num: usize, message: String },
}

#[derive(Debug, Error)]
pub enum ComponentManagerConfigurationError {
    #[error("no configuration file found in bootfs at path: {path}")]
    FileNotFound { path: Box<dyn api::Path> },
    #[error("failed to read blob: {0}")]
    BlobRead(#[from] api::BlobError),
    #[error("failed to perform io for blob: {0}")]
    Io(#[from] IoError),
    #[error("failed to deserialize component manager configuration from persistent fidl: {0}")]
    ParseFidl(#[from] FidlError),
    #[error("failed to parse component manager configuration from deserialized fidl: {0}")]
    ParseConfiguration(#[from] anyhow::Error),
}

struct AdditionalBootConfiguration(HashMap<String, String>);

impl AdditionalBootConfiguration {
    // See https://fuchsia.dev/fuchsia-src/reference/kernel/kernel_cmdline for the expected format,
    // and //src/sys/component_manager/src/builtin/arguments.rs for the runtime parser. This impl is
    // mostly copied from the runtime parser, but makes the format validation strict.
    // TODO(fxbug.dev/133260): Consider making this shared code with component manager's parser.
    fn new(blob: &VerifiedMemoryBlob) -> Result<Self, AdditionalBootConfigurationError> {
        let mut data = HashMap::<String, String>::new();

        let mut reader = blob.reader_seeker()?;
        let mut blob_contents = Vec::<u8>::new();
        reader
            .read_to_end(&mut blob_contents)
            .map_err(|err| AdditionalBootConfigurationError::Io(err))?;

        let cfg_raw = std::str::from_utf8(&blob_contents)
            .map_err(|err| AdditionalBootConfigurationError::ParseUtf8(err))?;
        let lines = cfg_raw.trim_end_matches(char::from(0)).lines();
        for (line_num, line) in lines.enumerate() {
            let trimmed = line.trim_start().trim_end();

            if trimmed.starts_with("#") {
                // This is a comment.
                continue;
            }

            if trimmed.contains(char::is_whitespace) {
                // Leading and trailing whitespace have already been trimmed, so any other
                // internal whitespace makes this argument malformed.
                return Err(AdditionalBootConfigurationError::ParseConfiguration {
                    line_num,
                    message: "keys and values must not contain whitespace".to_string(),
                });
            }

            let split = trimmed.splitn(2, "=").collect::<Vec<&str>>();
            if split.len() == 0 {
                return Err(AdditionalBootConfigurationError::ParseConfiguration {
                    line_num,
                    message: "expected `key=value` or `key`".to_string(),
                });
            }

            if split[0].is_empty() {
                return Err(AdditionalBootConfigurationError::ParseConfiguration {
                    line_num,
                    message: "expected nonempty `key` string".to_string(),
                });
            }

            data.insert(
                split[0].to_string(),
                if split.len() == 1 { String::new() } else { split[1].to_string() },
            );
        }
        Ok(Self(data))
    }
}

impl api::AdditionalBootConfiguration for AdditionalBootConfiguration {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).as_ref().map(|&s| s.as_str())
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>> {
        Box::new(self.0.clone().into_iter())
    }
}

struct ComponentManagerConfiguration(Rc<RuntimeConfig>);

impl ComponentManagerConfiguration {
    fn new(blob: &VerifiedMemoryBlob) -> Result<Self, ComponentManagerConfigurationError> {
        let mut reader = blob.reader_seeker()?;
        let mut blob_contents = Vec::<u8>::new();
        reader
            .read_to_end(&mut blob_contents)
            .map_err(|err| ComponentManagerConfigurationError::Io(err))?;
        let config =
            RuntimeConfig::try_from(unpersist::<component_internal::Config>(&blob_contents)?)?;
        Ok(Self(Rc::new(config)))
    }
}

impl api::ComponentManagerConfiguration for ComponentManagerConfiguration {
    fn debug(&self) -> bool {
        self.0.debug
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::api::Bootfs as _;
    use super::super::blob::BlobSet as _;
    use super::super::blob::VerifiedMemoryBlob;
    use super::super::data_source as ds;
    use super::super::package::test::placeholder_package_far;
    use super::AdditionalBootConfigurationError;
    use super::Bootfs;
    use super::BootfsPackageIndexError;
    use super::ComponentManagerConfigurationError;
    use super::ADDITIONAL_BOOT_ARGS_PATH;
    use super::BOOTFS_PACKAGE_INDEX_PATH;
    use super::COMPONENT_MANAGER_CONFIG_PATH;
    use fidl::persist;
    use fidl_fuchsia_component_internal as component_internal;
    use std::collections::HashMap;
    use std::io::Read as _;

    #[fuchsia::test]
    fn bootfs_iter_by_paths() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path_1: Box<dyn api::Path> = Box::new("path_1");
        let path_2: Box<dyn api::Path> = Box::new("path_2");
        let blob_1 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_1".as_bytes().into(),
        )
        .expect("blob");
        let blob_2 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_2".as_bytes().into(),
        )
        .expect("blob");
        let blobs = [(path_1.clone(), blob_1.clone()), (path_2.clone(), blob_2.clone())];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let mut expected = blobs
            .into_iter()
            .map(|(path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (path, blob)
            })
            .collect::<HashMap<_, _>>();
        let actual = bootfs.content_blobs().collect::<Vec<(_, _)>>();
        for (path, blob) in actual {
            let expected_blob = expected.get(&path).expect("actual blob in expectation set");
            assert_eq!(expected_blob.hash().as_ref(), blob.hash().as_ref());
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected.remove(&path);
        }
        assert_eq!(0, expected.len());
    }

    #[fuchsia::test]
    fn bootfs_blob_set_api() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path_1: Box<dyn api::Path> = Box::new("path_1");
        let path_2: Box<dyn api::Path> = Box::new("path_2");
        let blob_1 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_1".as_bytes().into(),
        )
        .expect("blob");
        let blob_2 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_2".as_bytes().into(),
        )
        .expect("blob");
        let blobs = [(path_1.clone(), blob_1.clone()), (path_2.clone(), blob_2.clone())];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let mut expected = blobs
            .into_iter()
            .map(|(_path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (blob.hash(), blob)
            })
            .collect::<HashMap<_, _>>();
        let actual = bootfs.iter().collect::<Vec<_>>();
        for blob in actual {
            let hash = blob.hash();
            let expected_blob = expected.get(&hash).expect("actual blob in expectation set");
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected.remove(&hash);
        }
        assert_eq!(0, expected.len());
    }

    #[fuchsia::test]
    fn bootfs_different_iterators() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let blob_1_path_1: Box<dyn api::Path> = Box::new("blob_1_path_1");
        let blob_1_path_2: Box<dyn api::Path> = Box::new("blob_1_path_2");
        let blob_2_path: Box<dyn api::Path> = Box::new("blob_2_path");
        let blob_1 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_1".as_bytes().into(),
        )
        .expect("blob");
        let blob_2 = VerifiedMemoryBlob::new(
            [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
            "blob_2".as_bytes().into(),
        )
        .expect("blob");
        let blobs = [
            (blob_1_path_1.clone(), blob_1.clone()),
            (blob_1_path_2.clone(), blob_1.clone()),
            (blob_2_path.clone(), blob_2.clone()),
        ];
        let bootfs = Bootfs::new(data_source, blobs.clone());

        // Iterate-by-path should contain all 3 entries.
        let mut expected_by_path = blobs
            .clone()
            .into_iter()
            .map(|(path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (path, blob)
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(3, expected_by_path.len());
        let actual_by_path = bootfs.content_blobs().collect::<Vec<(_, _)>>();
        for (path, blob) in actual_by_path {
            let expected_blob =
                expected_by_path.get(&path).expect("actual blob in expectation set");
            assert_eq!(expected_blob.hash().as_ref(), blob.hash().as_ref());
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected_by_path.remove(&path);
        }
        assert_eq!(0, expected_by_path.len());

        // Iterate-by-hash should contain 2 entries; two identical blobs at different paths get
        // deduplicated.
        let mut expected_by_hash = blobs
            .into_iter()
            .map(|(_path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (blob.hash(), blob)
            })
            // Will add `blob_1` twice, but dedup by `blob.hash()`.
            .collect::<HashMap<_, _>>();
        assert_eq!(2, expected_by_hash.len());
        let actual = bootfs.iter().collect::<Vec<_>>();
        for blob in actual {
            println!("hash: {}", blob.hash());
            let hash = blob.hash();
            let expected_blob =
                expected_by_hash.get(&hash).expect("actual blob in expectation set");
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected_by_hash.remove(&hash);
        }
        assert_eq!(0, expected_by_hash.len());
    }

    #[fuchsia::test]
    fn bootfs_additional_boot_configuration_valid() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path: Box<dyn api::Path> = Box::new(ADDITIONAL_BOOT_ARGS_PATH);
        let additional_boot_config_str = "key_1=val_1\n# a comment\nkey_2=val_2\nkey_3";
        let expected = [("key_1", "val_1"), ("key_2", "val_2"), ("key_3", "")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>();
        let blobs = [(
            path,
            VerifiedMemoryBlob::new(
                [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                additional_boot_config_str.as_bytes().into(),
            )
            .expect("blob"),
        )];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let additional_boot_config =
            bootfs.additional_boot_configuration().expect("additional boot configuration");

        assert_eq!(additional_boot_config.get("key_1").unwrap(), "val_1");
        assert_eq!(additional_boot_config.get("key_2").unwrap(), "val_2");
        assert_eq!(additional_boot_config.get("key_3").unwrap(), "");

        let mut contents = additional_boot_config.iter().collect::<Vec<_>>();
        contents.sort();
        assert_eq!(expected, contents);
    }

    #[fuchsia::test]
    fn bootfs_additional_boot_configuration_invalid() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path: Box<dyn api::Path> = Box::new(ADDITIONAL_BOOT_ARGS_PATH);
        let additional_boot_config_str = "=invalid";
        let blobs = [(
            path,
            VerifiedMemoryBlob::new(
                [Box::new(data_source.clone()) as Box<dyn api::DataSource>].into_iter(),
                additional_boot_config_str.as_bytes().into(),
            )
            .expect("blob"),
        )];
        let bootfs = Bootfs::new(data_source, blobs.clone().into_iter());
        match bootfs.additional_boot_configuration() {
            Err(api::BootfsError::AdditionalBootConfiguration(AdditionalBootConfigurationError::ParseConfiguration{..})) => {},
            Err(err) => assert!(false, "Expected error with inner type AdditionalBootConfigurationError::ParseConfiguration, but got: {:?}", err),
            Ok(_) => assert!(false, "Expected error due to invalid configuration file, but initialization succeeded"),
        }
    }

    #[fuchsia::test]
    fn bootfs_additional_boot_configuration_not_found() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path: Box<dyn api::Path> = Box::new("some/other/path");
        let additional_boot_config_str = "key_1=val_1";
        let blobs = [(
            path,
            VerifiedMemoryBlob::new(
                [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                additional_boot_config_str.as_bytes().into(),
            )
            .expect("blob"),
        )];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        match bootfs.additional_boot_configuration() {
            Err(api::BootfsError::AdditionalBootConfiguration(AdditionalBootConfigurationError::FileNotFound{..})) => {},
            Err(err) => assert!(false, "Expected error with inner type AdditionalBootConfigurationError::FileNotFound, but got: {:?}", err),
            Ok(_) => assert!(false, "Expected error due to incorrect configuration file path, but initialization succeeded"),
        }
    }

    #[fuchsia::test]
    fn bootfs_component_manager_configuration_valid() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path: Box<dyn api::Path> = Box::new(COMPONENT_MANAGER_CONFIG_PATH);
        let mut component_manager_config_data = component_internal::Config::default();
        component_manager_config_data.debug = Some(true);
        let component_manager_config_bytes = persist(&component_manager_config_data)
            .expect("serialize component manager configuration fidl");
        let blobs = [(
            path,
            VerifiedMemoryBlob::new(
                [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                component_manager_config_bytes.into(),
            )
            .expect("blob"),
        )];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let component_manager_config =
            bootfs.component_manager_configuration().expect("component manager configuration");
        assert_eq!(component_manager_config.debug(), true);
    }

    #[fuchsia::test]
    fn bootfs_component_manager_configuration_not_found() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));
        let path: Box<dyn api::Path> = Box::new("some/other/path");
        let mut component_manager_config_data = component_internal::Config::default();
        component_manager_config_data.debug = Some(true);
        let component_manager_config_bytes = persist(&component_manager_config_data)
            .expect("serialize component manager configuration fidl");
        let blobs = [(
            path,
            VerifiedMemoryBlob::new(
                [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                component_manager_config_bytes.into(),
            )
            .expect("blob"),
        )];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        match bootfs.component_manager_configuration() {
            Err(api::BootfsError::ComponentManagerConfiguration(ComponentManagerConfigurationError::FileNotFound{..})) => {},
            Err(err) => assert!(false, "Expected error with inner type ComponentManagerConfigurationError::FileNotFound, but got: {:?}", err),
            Ok(_) => assert!(false, "Expected error due to incorrect configuration file path, but initialization succeeded"),
        }
    }

    #[fuchsia::test]
    fn bootfs_packages_single_package() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));

        let (meta_far_hash, far_bytes) = placeholder_package_far();
        let pkg_path: Box<dyn api::Path> = Box::new(format!("blob/{}", meta_far_hash));
        let pkg_index_str = format!("pkg_1={}", meta_far_hash);

        let blobs = [
            (
                Box::new(BOOTFS_PACKAGE_INDEX_PATH) as Box<dyn api::Path>,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    pkg_index_str.into_bytes(),
                )
                .expect("package index blob"),
            ),
            (
                pkg_path,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    far_bytes,
                )
                .expect("package blob"),
            ),
        ];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let packages = bootfs.packages().expect("bootfs packages").collect::<Vec<_>>();
        assert_eq!(packages.len(), 1);
        let package = &packages[0];
        assert_eq!(meta_far_hash.as_ref(), package.hash().as_ref());
    }

    // Two packages that have the same contents (and so have the same hash), but that have different names
    // in the bootfs package index, are considered to be distinct elements of the bootfs package iterator.
    #[fuchsia::test]
    fn bootfs_packages_duplicate_package_hash() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));

        let (meta_far_hash, far_bytes) = placeholder_package_far();
        let pkg_path: Box<dyn api::Path> = Box::new(format!("blob/{}", meta_far_hash));
        let pkg_index_str = format!("pkg_1={}\npkg_2={}", meta_far_hash, meta_far_hash);

        let blobs = [
            (
                Box::new(BOOTFS_PACKAGE_INDEX_PATH) as Box<dyn api::Path>,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    pkg_index_str.into_bytes(),
                )
                .expect("package index blob"),
            ),
            (
                pkg_path.clone(),
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    far_bytes.clone(),
                )
                .expect("package blob 1"),
            ),
            (
                pkg_path,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    far_bytes,
                )
                .expect("package blob 2"),
            ),
        ];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        let packages = bootfs.packages().expect("bootfs packages").collect::<Vec<_>>();
        assert_eq!(packages.len(), 2);
        let pkg_1 = &packages[0];
        let pkg_2 = &packages[1];
        assert_eq!(pkg_1.hash().as_ref(), meta_far_hash.as_ref());
        assert_eq!(pkg_2.hash().as_ref(), meta_far_hash.as_ref());
    }

    #[fuchsia::test]
    fn bootfs_package_index_not_found() {
        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Unknown,
            None,
            api::DataSourceVersion::Unknown,
        ));

        let (meta_far_hash, far_bytes) = placeholder_package_far();
        let pkg_path: Box<dyn api::Path> = Box::new(format!("blob/{}", meta_far_hash));
        let pkg_index_str = format!("pkg_1={}", meta_far_hash);

        let blobs = [
            (
                Box::new("some/other/path") as Box<dyn api::Path>,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    pkg_index_str.into_bytes(),
                )
                .expect("package index blob"),
            ),
            (
                pkg_path,
                VerifiedMemoryBlob::new(
                    [Box::new(data_source.clone()) as Box<dyn api::DataSource>],
                    far_bytes,
                )
                .expect("package blob"),
            ),
        ];
        let bootfs = Bootfs::new(data_source, blobs.clone());
        match bootfs.packages() {
            Err(api::BootfsError::PackageIndex(BootfsPackageIndexError::IndexNotFound{..})) => {},
            Err(err) => assert!(false, "Expected error with inner type BootfsPackageIndexError::IndexNotFound, but got: {:?}", err),
            Ok(_) => assert!(false, "Expected error due to incorrect package index file path, but initialization succeeded"),
        }
    }
}
