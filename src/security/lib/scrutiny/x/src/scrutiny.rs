// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::api::System as _;
use super::blob::BlobDirectoryError;
use super::blob::BlobSet;
use super::blob::CompositeBlobSet;
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
    #[error("failed to construct bootfs from system's zbi: {0}")]
    Zbi(#[from] api::ZbiError),
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

        let blob_set: Box<dyn BlobSet> = Box::new(CompositeBlobSet::new([
            product_bundle.blob_set()?,
            Box::new(system.zbi().bootfs()?.clone()),
        ]));
        Ok(Self(Rc::new(ScrutinyData { product_bundle, blob_set, system })))
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
    fn packages(
        &self,
    ) -> Box<dyn Iterator<Item = Result<Box<dyn api::Package>, api::ScrutinyPackagesError>>> {
        let scrutiny_data = self.0.clone();
        Box::new(scrutiny_data.system.update_package().packages().clone().into_iter().map(
            move |url| {
                let meta_far_blob: Box<dyn api::Blob> =
                    scrutiny_data.blob_set.blob(Box::new(Hash::from(url.hash())))?;
                let package: Box<dyn api::Package> = Box::new(Package::new(
                    Some(scrutiny_data.product_bundle.data_source().clone()),
                    api::PackageResolverUrl::Package(AbsolutePackageUrl::from(url).into()),
                    meta_far_blob,
                    scrutiny_data.blob_set.clone(),
                )?);
                Ok(package)
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
    blob_set: Box<dyn BlobSet>,
    system: System,
}
