// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::blob::Blob;
use crate::blob::BlobDirectoryBlobSetBuilderError;
use crate::blob::BlobSet;
use crate::data_source::DataSource;
use crate::package::ScrutinyPackage;
use crate::product_bundle::DataSource as ProductBundleSource;
use crate::product_bundle::ProductBundle;
use crate::product_bundle::ProductBundleRepositoryBlobSet;
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
        let product_bundle_blobs_set = product_bundle.repository().blobs().blob_set()?;
        Ok(Scrutiny { product_bundle_blobs_set, product_bundle })
    }
}

/// Production implementation of the [`crate::api::Scrutiny`] API.
pub struct Scrutiny {
    product_bundle_blobs_set: ProductBundleRepositoryBlobSet,
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
    type Blob = Blob;
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
    use crate::hash::Hash;
    use crate::product_bundle::test::utf8_path;
    use crate::product_bundle::test::v2_sdk_a_product_bundle;
    use crate::product_bundle::test::v2_sdk_abr_product_bundle;
    use crate::product_bundle::test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH;
    use crate::product_bundle::test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME;
    use crate::product_bundle::test::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH;
    use crate::product_bundle::test::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME;
    use crate::product_bundle::test::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH;
    use crate::product_bundle::test::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME;
    use crate::product_bundle::ProductBundle;
    use crate::product_bundle::SystemSlot;
    use std::fs;
    use tempfile::TempDir;

    #[fuchsia::test]
    fn test_blob_directory_blob_set_builder_error() {
        // Create directory for product bundle, complete with repository blob directory.
        let temp_dir = TempDir::new().unwrap();
        // Note: Do not create blobs directory, which will trigger expected error.

        // Write product bundle manifest.
        v2_sdk_a_product_bundle(temp_dir.path()).write(utf8_path(temp_dir.path())).unwrap();

        // Construct product bundle.
        let product_bundle = ProductBundle::builder(
            temp_dir.path(),
            SystemSlot::A,
            V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME,
        )
        .build()
        .expect("product bundle from well-formed manifest");

        // Ensure blobs directory is missing.
        fs::remove_dir_all(&temp_dir.path().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH))
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
        let blobs_path_buf = temp_dir.path().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        fs::create_dir_all(&blobs_path_buf).unwrap();

        // Write product bundle manifest.
        v2_sdk_abr_product_bundle(temp_dir.path()).write(utf8_path(temp_dir.path())).unwrap();

        let a_blob_contents = "a";
        let a_blob_hash = Hash::from_contents(a_blob_contents.as_bytes());
        let a_blob_hash_string = format!("{}", a_blob_hash);
        fs::create_dir_all(temp_dir.path().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH))
            .expect("create a blob directory");
        fs::write(
            temp_dir
                .path()
                .join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH)
                .join(&a_blob_hash_string),
            a_blob_contents.as_bytes(),
        )
        .expect("write a blob");

        let b_blob_contents = "b";
        let b_blob_hash = Hash::from_contents(b_blob_contents.as_bytes());
        let b_blob_hash_string = format!("{}", b_blob_hash);
        fs::create_dir_all(temp_dir.path().join(V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH))
            .expect("create b blob directory");
        fs::write(
            temp_dir
                .path()
                .join(V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH)
                .join(&b_blob_hash_string),
            b_blob_contents.as_bytes(),
        )
        .expect("write b blob");

        let r_blob_contents = "r";
        let r_blob_hash = Hash::from_contents(r_blob_contents.as_bytes());
        let r_blob_hash_string = format!("{}", r_blob_hash);
        fs::create_dir_all(temp_dir.path().join(V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH))
            .expect("create r blob directory");
        fs::write(
            temp_dir
                .path()
                .join(V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH)
                .join(&r_blob_hash_string),
            r_blob_contents.as_bytes(),
        )
        .expect("write r blob");

        // Test against a slot.
        let a_product_bundle = ProductBundle::builder(
            temp_dir.path(),
            SystemSlot::A,
            V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME,
        )
        .build()
        .expect("product bundle from well-formed manifest and directories");
        let a_scrutiny = Scrutiny::builder(a_product_bundle)
            .build()
            .expect("scrutiny from well-formed product bundle");
        let a_blobs: Vec<_> = a_scrutiny.blobs().collect();
        assert_eq!(1, a_blobs.len());
        let mut a_blob_read_contents = vec![];
        a_blobs[0]
            .reader_seeker()
            .expect("a blob reader")
            .read_to_end(&mut a_blob_read_contents)
            .expect("a blob read");
        assert_eq!(a_blob_contents.as_bytes(), a_blob_read_contents.as_slice());

        // Test against b slot.
        let b_product_bundle = ProductBundle::builder(
            temp_dir.path(),
            SystemSlot::B,
            V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME,
        )
        .build()
        .expect("product bundle from well-formed manifest and directories");
        let b_scrutiny = Scrutiny::builder(b_product_bundle)
            .build()
            .expect("scrutiny from well-formed product bundle");
        let b_blobs: Vec<_> = b_scrutiny.blobs().collect();
        assert_eq!(1, b_blobs.len());
        let mut b_blob_read_contents = vec![];
        b_blobs[0]
            .reader_seeker()
            .expect("b blob reader")
            .read_to_end(&mut b_blob_read_contents)
            .expect("b blob read");
        assert_eq!(b_blob_contents.as_bytes(), b_blob_read_contents.as_slice());

        // Test against r slot.
        let r_product_bundle = ProductBundle::builder(
            temp_dir.path(),
            SystemSlot::R,
            V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME,
        )
        .build()
        .expect("product bundle from well-formed manifest and directories");
        let r_scrutiny = Scrutiny::builder(r_product_bundle)
            .build()
            .expect("scrutiny from well-formed product bundle");
        let r_blobs: Vec<_> = r_scrutiny.blobs().collect();
        assert_eq!(1, r_blobs.len());
        let mut r_blob_read_contents = vec![];
        r_blobs[0]
            .reader_seeker()
            .expect("r blob reader")
            .read_to_end(&mut r_blob_read_contents)
            .expect("r blob read");
        assert_eq!(r_blob_contents.as_bytes(), r_blob_read_contents.as_slice());
    }
}
