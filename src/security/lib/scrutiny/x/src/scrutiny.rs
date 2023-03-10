// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::blob::Blob;
use crate::blob::BlobDirectoryBlobSetBuilderError;
use crate::blob::BlobDirectoryError;
use crate::blob::BlobSet;
use crate::blob::CompositeBlobSet;
use crate::data_source::DataSource;
use crate::package::ScrutinyPackage;
use crate::product_bundle::DataSource as ProductBundleSource;
use crate::product_bundle::ProductBundle;
use crate::product_bundle::ProductBundleRepositoryBlob;
use std::fmt;
use thiserror::Error;

/// Errors that can be encountered building a [`Scrutiny`] via a [`ScrutinyBuilder`].
#[derive(Debug, Error)]
pub enum ScrutinyBuilderError {
    #[error("failed to construct blob set for scrutiny interface: {0:?}")]
    BlobDirectoryBlobSetBuilderError(#[from] BlobDirectoryBlobSetBuilderError),
}

/// A builder pattern for constructing well-formed instances of [`Scrutiny`].
pub struct ScrutinyBuilder {
    product_bundle: ProductBundle,
}

impl ScrutinyBuilder {
    /// Constructs a new builder for building a [`Scrutiny`] instance.
    pub fn new(product_bundle: ProductBundle) -> Self {
        Self { product_bundle }
    }

    /// Builds a [`Scrutiny`] based on data in builder. This builder relies on the
    /// `ProductBundleRepositoryBlobs::blob_set()` API to construct a `BlobSet` from a product
    /// bundle repository blobs directory.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn build(self) -> Result<Scrutiny, ScrutinyBuilderError> {
        let product_bundle = self.product_bundle.clone();
        let product_bundle_blobs_set = product_bundle.blob_set()?;
        Ok(Scrutiny { product_bundle_blobs_set, product_bundle })
    }
}

/// Production implementation of the [`crate::api::Scrutiny`] API.
pub struct Scrutiny {
    product_bundle_blobs_set: CompositeBlobSet<ProductBundleRepositoryBlob, BlobDirectoryError>,
    product_bundle: ProductBundle,
}

impl Scrutiny {
    /// Constructs a builder for building a well-formed [`Scrutiny`] instance.
    pub fn builder(product_bundle: ProductBundle) -> ScrutinyBuilder {
        ScrutinyBuilder::new(product_bundle)
    }
}

impl fmt::Debug for Scrutiny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.write_str("Scrutiny(")?;
        self.product_bundle.fmt(f)?;
        f.write_str(")")
    }
}

impl api::Scrutiny for Scrutiny {
    type Blob = Blob<ProductBundleRepositoryBlob>;
    type Package = ScrutinyPackage;
    type DataSource = DataSource;

    // TODO: Use production implementations when available.
    type PackageResolver = crate::todo::PackageResolver;
    type Component = crate::todo::Component;
    type ComponentResolver = crate::todo::ComponentResolver;
    type ComponentCapability = crate::todo::ComponentCapability;
    type ComponentInstance = crate::todo::ComponentInstance;
    type ComponentInstanceCapability = crate::todo::ComponentInstanceCapability;
    type System = crate::todo::System;
    type ComponentManager = crate::todo::ComponentManager;

    fn system(&self) -> Self::System {
        todo!("TODO(fxbug.dev/111251): Integrate Scrutiny with production System API")
    }

    fn component_manager(&self) -> Self::ComponentManager {
        todo!("TODO(fxbug.dev/111251): Integrate Scrutiny with production System API")
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        let product_bundle_source: ProductBundleSource = self.product_bundle.clone().into();
        Box::new([product_bundle_source.into()].into_iter())
    }

    fn blobs(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
        Box::new(self.product_bundle_blobs_set.iter().map(Blob::from))
    }

    fn packages(&self) -> Box<dyn Iterator<Item = Self::Package>> {
        todo!("TODO(fxbug.dev/111242): Integrate `Scrutiny` with production Package API that can locate relevant packages")
    }

    fn package_resolvers(&self) -> Box<dyn Iterator<Item = Self::PackageResolver>> {
        todo!("TODO(fxbug.dev/111249): Integrate `Scrutiny` with production package resolver API")
    }

    fn components(&self) -> Box<dyn Iterator<Item = Self::Component>> {
        todo!("TODO(fxbug.dev/111243): Integrte `Scrutiny` with production component API")
    }

    fn component_resolvers(&self) -> Box<dyn Iterator<Item = Self::ComponentResolver>> {
        todo!("TODO(fxbug.dev/111250): Integrate `Scrutiny` with production component resolver API")
    }

    fn component_capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
        todo!(
            "TODO(fxbug.dev/111244): Integrate `Scrutiny` with production component capability API"
        )
    }

    fn component_instances(&self) -> Box<dyn Iterator<Item = Self::ComponentInstance>> {
        todo!("TODO(fxbug.dev/111245): Integrate `Scrutiny` with production component instance API")
    }

    fn component_instance_capabilities(
        &self,
    ) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
        todo!("TODO(fxbug.dev/111246): Integrate `Scrutiny` with production component instance capability API")
    }
}

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::blob::fake::Blob;
    use crate::component::fake::Component;
    use crate::component_capability::fake::ComponentCapability;
    use crate::component_instance::fake::ComponentInstance;
    use crate::component_instance_capability::fake::ComponentInstanceCapability;
    use crate::component_manager::fake::ComponentManager;
    use crate::component_resolver::fake::ComponentResolver;
    use crate::data_source::fake::DataSource;
    use crate::hash::fake::Hash;
    use crate::package::fake::Package;
    use crate::package_resolver::fake::PackageResolver;
    use crate::system::fake::System;
    use std::iter;

    #[derive(Default)]
    struct Scrutiny;

    impl api::Scrutiny for Scrutiny {
        type Blob = Blob<Hash>;
        type Package = Package;
        type PackageResolver = PackageResolver;
        type Component = Component;
        type ComponentResolver = ComponentResolver;
        type ComponentCapability = ComponentCapability;
        type ComponentInstance = ComponentInstance;
        type ComponentInstanceCapability = ComponentInstanceCapability;
        type System = System;
        type ComponentManager = ComponentManager;
        type DataSource = DataSource;

        fn system(&self) -> Self::System {
            System::default()
        }

        fn component_manager(&self) -> Self::ComponentManager {
            ComponentManager::default()
        }

        fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
            Box::new(iter::empty())
        }

        fn blobs(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
            Box::new(iter::empty())
        }

        fn packages(&self) -> Box<dyn Iterator<Item = Self::Package>> {
            Box::new(iter::empty())
        }

        fn package_resolvers(&self) -> Box<dyn Iterator<Item = Self::PackageResolver>> {
            Box::new(iter::empty())
        }

        fn components(&self) -> Box<dyn Iterator<Item = Self::Component>> {
            Box::new(iter::empty())
        }

        fn component_resolvers(&self) -> Box<dyn Iterator<Item = Self::ComponentResolver>> {
            Box::new(iter::empty())
        }

        fn component_capabilities(&self) -> Box<dyn Iterator<Item = Self::ComponentCapability>> {
            Box::new(iter::empty())
        }

        fn component_instances(&self) -> Box<dyn Iterator<Item = Self::ComponentInstance>> {
            Box::new(iter::empty())
        }

        fn component_instance_capabilities(
            &self,
        ) -> Box<dyn Iterator<Item = Self::ComponentInstanceCapability>> {
            Box::new(iter::empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Scrutiny;
    use super::ScrutinyBuilderError;
    use crate::api::Blob as _;
    use crate::api::Scrutiny as _;
    use crate::blob;
    use crate::data_source::BlobSource;
    use crate::hash::Hash;
    use crate::product_bundle::test as pbt;
    use crate::product_bundle::ProductBundle;
    use crate::product_bundle::ProductBundleRepositoryBlob;
    use crate::product_bundle::ProductBundleRepositoryBlobs;
    use crate::product_bundle::SystemSlot;
    use maplit::hashmap;
    use maplit::hashset;
    use sdk_metadata::Repository;
    use std::borrow::Borrow;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::fs;
    use std::io::Read as _;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_blob<Directory: AsRef<Path>, Contents: Borrow<str>>(
        directory: Directory,
        contents: Contents,
    ) -> Hash {
        let hash = Hash::from_contents(contents.borrow().as_bytes());
        let filename = format!("{}", hash);
        fs::write(directory.as_ref().join(filename), contents.borrow().as_bytes())
            .expect("write blob");
        hash
    }

    #[fuchsia::test]
    fn test_blob_directory_blob_set_builder_error() {
        // Create directory for product bundle, complete with repository blob directory.
        let temp_dir = TempDir::new().unwrap();
        // Note: Do not create blobs directory, which will trigger expected error.

        // Write product bundle manifest.
        pbt::v2_sdk_a_product_bundle(temp_dir.path())
            .write(pbt::utf8_path(temp_dir.path()))
            .unwrap();

        // Construct product bundle.
        let product_bundle = ProductBundle::builder(temp_dir.path(), SystemSlot::A)
            .build()
            .expect("product bundle from well-formed manifest");

        // Ensure blobs directory is missing.
        fs::remove_dir_all(
            &temp_dir.path().join(pbt::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        )
        .expect("remove blobs directory");

        // Expect error creating blob set.
        match Scrutiny::builder(product_bundle)
            .build()
            .expect_err("blob directory blob set builder error")
        {
            ScrutinyBuilderError::BlobDirectoryBlobSetBuilderError(_) => {}
        }
    }

    #[fuchsia::test]
    fn test_ok() {
        // Create directory for product bundle, complete with repository blob directory.
        let temp_dir = TempDir::new().unwrap();

        // Write product bundle manifest.
        pbt::v2_sdk_abr_product_bundle(temp_dir.path())
            .write(pbt::utf8_path(temp_dir.path()))
            .unwrap();

        let a_blobs_directory =
            temp_dir.path().join(pbt::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        let a_metadata_directory =
            temp_dir.path().join(pbt::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH);
        fs::create_dir_all(&a_blobs_directory).expect("create a blobs directory");
        let a_blob_contents = "a";
        let a_blob_hash = write_blob(&a_blobs_directory, a_blob_contents);
        let ab_blob_contents = "ab";
        let ab_blob_hash = write_blob(&a_blobs_directory, ab_blob_contents);
        let ar_blob_contents = "ar";
        let ar_blob_hash = write_blob(&a_blobs_directory, ar_blob_contents);

        let b_blobs_directory =
            temp_dir.path().join(pbt::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        let b_metadata_directory =
            temp_dir.path().join(pbt::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH);
        fs::create_dir_all(&b_blobs_directory).expect("create b blobs directory");
        let b_blob_contents = "b";
        let b_blob_hash = write_blob(&b_blobs_directory, b_blob_contents);
        write_blob(&b_blobs_directory, ab_blob_contents);
        let br_blob_contents = "br";
        let br_blob_hash = write_blob(&b_blobs_directory, br_blob_contents);

        let r_blobs_directory =
            temp_dir.path().join(pbt::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        let r_metadata_directory =
            temp_dir.path().join(pbt::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH);
        fs::create_dir_all(&r_blobs_directory).expect("create r blobs directory");
        let r_blob_contents = "r";
        let r_blob_hash = write_blob(&r_blobs_directory, r_blob_contents);
        write_blob(&r_blobs_directory, ar_blob_contents);
        write_blob(&r_blobs_directory, br_blob_contents);

        let blobs_map = hashmap! {
            a_blob_hash.clone() => a_blob_contents.as_bytes().to_vec(),
            ab_blob_hash.clone() => ab_blob_contents.as_bytes().to_vec(),
            ar_blob_hash.clone() => ar_blob_contents.as_bytes().to_vec(),
            b_blob_hash.clone() => b_blob_contents.as_bytes().to_vec(),
            br_blob_hash.clone() => br_blob_contents.as_bytes().to_vec(),
            r_blob_hash.clone() => r_blob_contents.as_bytes().to_vec(),
        };

        let a_repository = Repository {
            name: pbt::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
            metadata_path: a_metadata_directory.try_into().unwrap(),
            blobs_path: a_blobs_directory.try_into().unwrap(),
        };
        let b_repository = Repository {
            name: pbt::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
            metadata_path: b_metadata_directory.try_into().unwrap(),
            blobs_path: b_blobs_directory.try_into().unwrap(),
        };
        let r_repository = Repository {
            name: pbt::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
            metadata_path: r_metadata_directory.try_into().unwrap(),
            blobs_path: r_blobs_directory.try_into().unwrap(),
        };

        let data_source = |product_bundle: &ProductBundle,
                           repository: &Repository|
         -> ProductBundleRepositoryBlobs {
            ProductBundleRepositoryBlobs::new_for_test(product_bundle.clone(), repository.clone())
        };

        let blob_sources = |product_bundle: &ProductBundle| -> HashMap<Hash, HashSet<ProductBundleRepositoryBlobs>> {
            hashmap! {
                a_blob_hash.clone() => hashset!{data_source(product_bundle, &a_repository)},
                ab_blob_hash.clone() => hashset!{data_source(product_bundle, &a_repository), data_source(product_bundle, &b_repository)},
                ar_blob_hash.clone() => hashset!{data_source(product_bundle, &a_repository), data_source(product_bundle, &r_repository)},
                b_blob_hash.clone() => hashset!{data_source(product_bundle, &b_repository)},
                br_blob_hash.clone() => hashset!{data_source(product_bundle, &b_repository), data_source(product_bundle, &r_repository)},
                r_blob_hash.clone() => hashset!{data_source(product_bundle, &r_repository)},
            }
        };

        let verify_blobs =
            |product_bundle: &ProductBundle,
             blobs: Vec<blob::Blob<ProductBundleRepositoryBlob>>| {
                assert_eq!(blobs_map.len(), blobs.len());

                let blob_sources_map = blob_sources(product_bundle);

                for actual_blob in blobs.into_iter() {
                    let actual_hash = actual_blob.hash();
                    let mut actual_contents = vec![];
                    actual_blob
                        .reader_seeker()
                        .expect("blob reader")
                        .read_to_end(&mut actual_contents)
                        .expect("blob read");
                    let expected_blob_contents =
                        blobs_map.get(&actual_hash).expect("expected blob for actual blob");
                    assert_eq!(expected_blob_contents, &actual_contents);
                    let expected_blob_sources = blob_sources_map
                        .get(&actual_hash)
                        .expect("expected blob sources for actual blob")
                        .clone()
                        .into_iter()
                        .map(BlobSource::from)
                        .collect::<HashSet<BlobSource>>();
                    let actual_blob_sources: HashSet<BlobSource> =
                        actual_blob.data_sources().collect();
                    assert_eq!(expected_blob_sources, actual_blob_sources);
                }
            };

        // Test against a slot.
        let a_product_bundle = ProductBundle::builder(temp_dir.path(), SystemSlot::A)
            .build()
            .expect("product bundle from well-formed manifest and directories");
        let a_scrutiny = Scrutiny::builder(a_product_bundle.clone())
            .build()
            .expect("scrutiny from well-formed product bundle");
        verify_blobs(&a_product_bundle, a_scrutiny.blobs().collect());

        // Test against b slot.
        let b_product_bundle = ProductBundle::builder(temp_dir.path(), SystemSlot::B)
            .build()
            .expect("product bundle from well-formed manifest and directories");
        let b_scrutiny = Scrutiny::builder(b_product_bundle.clone())
            .build()
            .expect("scrutiny from well-formed product bundle");
        verify_blobs(&b_product_bundle, b_scrutiny.blobs().collect());

        // Test against r slot.
        let r_product_bundle = ProductBundle::builder(temp_dir.path(), SystemSlot::R)
            .build()
            .expect("product bundle from well-formed manifest and directories");
        let r_scrutiny = Scrutiny::builder(r_product_bundle.clone())
            .build()
            .expect("scrutiny from well-formed product bundle");
        verify_blobs(&r_product_bundle, r_scrutiny.blobs().collect());
    }
}
