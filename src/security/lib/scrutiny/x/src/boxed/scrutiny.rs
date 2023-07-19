// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::product_bundle as pb;
use std::rc::Rc;

pub struct Scrutiny(Rc<ScrutinyData>);

impl Scrutiny {
    /// Constructs a [`Scrutiny`] instance backed by build artifacts referenced in a single product
    /// bundle.
    pub fn new(product_bundle_path: Box<dyn api::Path>) -> Result<Self, pb::Error> {
        Ok(Self(Rc::new(ScrutinyData {
            product_bundle: pb::ProductBundle::new(product_bundle_path)?,
        })))
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

    fn packages(&self) -> Box<dyn Iterator<Item = Box<dyn api::Package>>> {
        todo!("TODO(fxbug.dev/111242): Integrate with production package API")
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
}
