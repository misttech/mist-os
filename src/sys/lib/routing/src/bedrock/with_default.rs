// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use cm_types::Availability;
use derivative::Derivative;
use router_error::RouterError;
use sandbox::{
    CapabilityBound, Dict, Request, Routable, Router, RouterResponse, WeakInstanceToken,
};
use std::sync::Arc;

pub trait WithDefault {
    /// Returns a router that provides a default when a `None` request is passed in, constructed
    /// from the metadata returned by `metadata_fn`.
    fn with_default(
        self: Self,
        metadata_fn: Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>,
        availability: Availability,
        target: WeakInstanceToken,
    ) -> Self;
}

#[derive(Derivative)]
#[derivative(Debug)]
struct RouterWithDefault<T: CapabilityBound> {
    router: Router<T>,
    #[derivative(Debug = "ignore")]
    metadata_fn: Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>,
    availability: Availability,
    target: WeakInstanceToken,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for RouterWithDefault<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let request = if let Some(request) = request {
            request
        } else {
            let metadata = (self.metadata_fn)(self.availability);
            Request { target: self.target.clone(), metadata }
        };
        self.router.route(Some(request), debug).await
    }
}

impl<T: CapabilityBound> WithDefault for Router<T> {
    fn with_default(
        self,
        metadata_fn: Arc<dyn Fn(Availability) -> Dict + Send + Sync + 'static>,
        availability: Availability,
        target: WeakInstanceToken,
    ) -> Self {
        Self::new(RouterWithDefault { router: self, metadata_fn, target, availability })
    }
}
