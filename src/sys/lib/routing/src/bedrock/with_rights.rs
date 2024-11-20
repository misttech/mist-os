// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::Metadata;
use crate::error::RoutingError;
use crate::rights::{Rights, RightsWalker};
use crate::walk_state::WalkStateUnit;
use async_trait::async_trait;
use fidl_fuchsia_component_sandbox as fsandbox;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{CapabilityBound, Request, Routable, Router, RouterResponse};

struct RightsRouter<T: CapabilityBound> {
    router: Router<T>,
    rights: Rights,
    moniker: ExtendedMoniker,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for RightsRouter<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, router_error::RouterError> {
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let RightsRouter { router, rights, moniker } = self;
        let request_rights: Rights =
            request.metadata.get_metadata().ok_or(fsandbox::RouterError::InvalidArgs)?;
        let request_rights = RightsWalker::new(request_rights, moniker.clone());
        let router_rights = RightsWalker::new(*rights, moniker.clone());
        // The rights of the request must be compatible with the
        // rights of this step of the route.
        match request_rights.validate_next(&router_rights) {
            Ok(()) => router.route(Some(request), debug).await,
            Err(e) => Err(RoutingError::from(e).into()),
        }
    }
}

pub trait WithRights {
    /// Returns a router that ensures the capability request does not request
    /// greater rights than provided at this stage of the route.
    fn with_rights(self, moniker: impl Into<ExtendedMoniker>, rights: Rights) -> Self;
}

impl<T: CapabilityBound> WithRights for Router<T> {
    fn with_rights(self, moniker: impl Into<ExtendedMoniker>, rights: Rights) -> Self {
        Router::<T>::new(RightsRouter { rights, router: self, moniker: moniker.into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_io as fio;
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Data, Dict, WeakInstanceToken};
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeComponentToken {}

    impl FakeComponentToken {
        fn new() -> WeakInstanceToken {
            WeakInstanceToken { inner: Arc::new(FakeComponentToken {}) }
        }
    }

    impl sandbox::WeakInstanceTokenAny for FakeComponentToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn rights_good() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy = base.with_rights(ExtendedMoniker::ComponentManager, fio::RW_STAR_DIR.into());
        let metadata = Dict::new();
        metadata.set_metadata(Into::<Rights>::into(fio::R_STAR_DIR));
        let capability = proxy
            .route(Some(Request { target: FakeComponentToken::new(), metadata }), false)
            .await
            .unwrap();
        let capability = match capability {
            RouterResponse::<Data>::Capability(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn rights_bad() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy = base.with_rights(ExtendedMoniker::ComponentManager, fio::R_STAR_DIR.into());
        let metadata = Dict::new();
        metadata.set_metadata(Into::<Rights>::into(fio::RW_STAR_DIR));
        let error = proxy
            .route(Some(Request { target: FakeComponentToken::new(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::RightsRoutingError(
                    crate::error::RightsRoutingError::Invalid { moniker: ExtendedMoniker::ComponentManager, requested, provided }
                ) if *requested == <fio::Operations as Into<Rights>>::into(fio::RW_STAR_DIR) && *provided == <fio::Operations as Into<Rights>>::into(fio::R_STAR_DIR)
            )
        );
    }
}
