// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::blob::BlobDirectoryError;
use super::blob::BlobSet;
use super::hash::Hash;
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
}

pub struct Scrutiny(Rc<ScrutinyData>);

impl Scrutiny {
    /// Constructs a [`Scrutiny`] instance backed by build artifacts referenced in a single product
    /// bundle.
    pub fn new(product_bundle_path: Box<dyn api::Path>) -> Result<Self, Error> {
        let product_bundle = pb::ProductBundle::new(product_bundle_path)?;
        let product_bundle_blob_set = product_bundle.blob_set()?;
        let system: Box<dyn api::System> = Box::new(System::new(product_bundle.clone())?);
        Ok(Self(Rc::new(ScrutinyData { product_bundle, product_bundle_blob_set, system })))
    }
}

impl api::Scrutiny for Scrutiny {
    fn system(&self) -> Box<dyn api::System> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API")
    }

    fn component_manager(&self) -> Box<dyn api::ComponentManager> {
        todo!("TODO(fxbug.dev/111251): Integrate with production system API")
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        let data_source: Box<dyn api::DataSource> =
            Box::new(self.0.product_bundle.data_source().clone());
        Box::new([data_source].into_iter())
    }

    fn blobs(
        &self,
    ) -> Result<Box<dyn Iterator<Item = Box<dyn api::Blob>>>, api::ScrutinyBlobsError> {
        Ok(self.0.product_bundle.blob_set()?.iter())
    }

    #[tracing::instrument(level = "trace", skip_all)]
    fn packages(
        &self,
    ) -> Box<dyn Iterator<Item = Result<Box<dyn api::Package>, api::ScrutinyPackagesError>>> {
        let scrutiny_data = self.0.clone();
        Box::new(scrutiny_data.system.update_package().packages().clone().into_iter().map(
            move |url| match url {
                AbsolutePackageUrl::Unpinned(unpinned) => {
                    Err(api::ScrutinyPackagesError::Ambiguous(unpinned.clone()))
                }
                AbsolutePackageUrl::Pinned(pinned) => {
                    let meta_far_blob: Box<dyn api::Blob> =
                        scrutiny_data
                            .product_bundle_blob_set
                            .blob(Box::new(Hash::from(pinned.hash())))?;
                    let package: Box<dyn api::Package> = Box::new(Package::new(
                        Some(scrutiny_data.product_bundle.data_source().clone()),
                        api::PackageResolverUrl::Url,
                        meta_far_blob,
                        scrutiny_data.product_bundle_blob_set.clone(),
                    )?);
                    Ok(package)
                }
            },
        ))
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
    product_bundle_blob_set: Box<dyn BlobSet>,
    system: Box<dyn api::System>,
}
