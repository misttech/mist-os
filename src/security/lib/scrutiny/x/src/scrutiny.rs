// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::api::Bootfs as _;
use super::api::BootfsError;
use super::api::System as _;
use super::blob::BlobDirectoryError;
use super::blob::BlobOpenError;
use super::blob::BlobSet;
use super::blob::CompositeBlobSet;
use super::hash::Hash;
use super::package::Error as PackageError;
use super::package::Package;
use super::product_bundle as pb;
use super::system::Error as SystemError;
use super::system::System;
use fuchsia_url::AbsolutePackageUrl;
use std::rc::Rc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to open product bundle: {0}")]
    ProductBundle(#[from] pb::Error),
    #[error("failed to open product bundle blob directory: {0}")]
    BlobDirectory(#[from] BlobDirectoryError),
    #[error("failed to construct system from product bundle: {0}")]
    System(#[from] SystemError),
    #[error("failed to construct bootfs from system's zbi: {0}")]
    Zbi(#[from] api::ZbiError),
    #[error("failed to gather packages: {0}")]
    Package(#[from] ProductBundlePackageError),
    #[error("failed to gather packages from bootfs: {0}")]
    BootfsPackages(#[from] BootfsError),
}

#[derive(Debug, Error)]
pub enum ProductBundlePackageError {
    #[error("failed to locate package blob: {0}")]
    Locate(#[from] BlobOpenError),
    #[error("failed to open package: {0}")]
    Open(#[from] PackageError),
}

pub(crate) struct Scrutiny(Rc<ScrutinyData>);

impl Scrutiny {
    /// Constructs a [`Scrutiny`] instance backed by build artifacts referenced in a single product
    /// bundle.
    pub fn new(
        product_bundle_path: Box<dyn api::Path>,
        variant: api::SystemVariant,
    ) -> Result<Self, Error> {
        let product_bundle = pb::ProductBundle::new(product_bundle_path)?;

        let system: System = System::new(product_bundle.clone(), variant)?;

        let product_bundle_data_source = product_bundle.data_source();
        let product_bundle_blob_set = product_bundle.blob_set()?;
        let bootfs = system.zbi().bootfs()?;
        let blob_set: Box<dyn BlobSet> = Box::new(CompositeBlobSet::new([
            product_bundle_blob_set.clone(),
            Box::new(bootfs.clone()),
        ]));

        let packages = system
            .update_package()
            .packages()
            .clone()
            .into_iter()
            .map(move |url| -> Result<_, ProductBundlePackageError> {
                let meta_far_blob: Box<dyn api::Blob> =
                    product_bundle_blob_set.blob(Box::new(Hash::from(url.hash())))?;
                let package: Box<dyn api::Package> = Box::new(Package::new(
                    Some(product_bundle_data_source.clone()),
                    api::PackageResolverUrl::Package(AbsolutePackageUrl::from(url).into()),
                    meta_far_blob,
                    product_bundle_blob_set.clone(),
                )?);
                Ok(package)
            })
            .chain(bootfs.packages()?.map(Ok))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self(Rc::new(ScrutinyData { product_bundle, blob_set, packages, system })))
    }
}

impl api::Scrutiny for Scrutiny {
    fn system(&self) -> Box<dyn api::System> {
        Box::new(self.0.system.clone()) as Box<dyn api::System>
    }

    fn component_manager(&self) -> Box<dyn api::ComponentManager> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API")
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        let data_source: Box<dyn api::DataSource> =
            Box::new(self.0.product_bundle.data_source().clone());
        Box::new([data_source].into_iter())
    }

    fn blobs(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        self.0.blob_set.iter()
    }

    #[tracing::instrument(level = "trace", skip_all)]
    fn packages(&self) -> Box<dyn Iterator<Item = Box<dyn api::Package>>> {
        Box::new(self.0.packages.clone().into_iter())
    }

    fn package_resolvers(&self) -> Box<dyn Iterator<Item = Box<dyn api::PackageResolver>>> {
        todo!("TODO(fxbug.dev/111249): Integrate with production package resolver API")
    }

    fn components(&self) -> Box<dyn Iterator<Item = Box<dyn api::Component>>> {
        todo!("TODO(fxbug.dev/111243): Integrate with production component API")
    }

    fn component_resolvers(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentResolver>>> {
        todo!("TODO(fxbug.dev/111250): Integrate with production component resolver API")
    }

    fn component_capabilities(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::ComponentCapability>>> {
        todo!("TODO(fxbug.dev/111244): Integrate with production component capability API")
    }

    fn component_instances(&self) -> Box<dyn Iterator<Item = Box<dyn api::ComponentInstance>>> {
        todo!("TODO(fxbug.dev/111245): Integrate with production component instance API")
    }

    fn component_instance_capabilities(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::ComponentInstanceCapability>>> {
        todo!("TODO(fxbug.dev/111246): Integrate with production component instance capability API")
    }
}

struct ScrutinyData {
    product_bundle: pb::ProductBundle,
    blob_set: Box<dyn BlobSet>,
    packages: Vec<Box<dyn api::Package>>,
    system: System,
}
