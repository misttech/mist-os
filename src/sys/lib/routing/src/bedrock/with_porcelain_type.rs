// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::METADATA_KEY_TYPE;
use crate::error::RoutingError;
use async_trait::async_trait;
use cm_rust::CapabilityTypeName;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{Capability, CapabilityBound, Data, Request, Routable, Router, RouterResponse};

pub fn is_supported(porcelain_type: &CapabilityTypeName) -> bool {
    matches!(porcelain_type, CapabilityTypeName::Protocol | CapabilityTypeName::Config)
}

pub trait WithPorcelainType {
    /// Returns a router that ensures the capability request has a porcelain
    /// type that is the same as the type of the capability returned by the
    /// router.
    fn with_porcelain_type(
        self,
        porcelain_type: CapabilityTypeName,
        moniker: impl Into<ExtendedMoniker>,
    ) -> Self;
}

#[derive(Debug, Clone)]
struct RouterWithPorcelainType<T: CapabilityBound> {
    router: Router<T>,
    porcelain_type: CapabilityTypeName,
    moniker: ExtendedMoniker,
}

#[async_trait]
impl<T: CapabilityBound> Routable<T> for RouterWithPorcelainType<T> {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<T>, RouterError> {
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let RouterWithPorcelainType { router, porcelain_type, moniker } = self;
        let Capability::Data(Data::String(capability_type)) = request
            .metadata
            .get(&cm_types::Name::new(METADATA_KEY_TYPE).unwrap())
            .map_err(|()| RoutingError::BedrockNotCloneable { moniker: moniker.clone() })?
            .unwrap_or_else(|| {
                panic!("missing capability type {porcelain_type} for request: {request:?}")
            })
        else {
            return Err(RoutingError::BedrockNotPresentInDictionary {
                moniker: moniker.clone(),
                name: String::from("type"),
            }
            .into());
        };
        let porcelain_type = porcelain_type.to_string();
        if capability_type == porcelain_type {
            router.route(Some(request), debug).await
        } else {
            Err(RoutingError::BedrockWrongCapabilityType {
                moniker: moniker.clone(),
                actual: capability_type,
                expected: porcelain_type,
            }
            .into())
        }
    }
}

impl<T: CapabilityBound> WithPorcelainType for Router<T> {
    fn with_porcelain_type(
        self,
        porcelain_type: CapabilityTypeName,
        moniker: impl Into<ExtendedMoniker>,
    ) -> Self {
        if !is_supported(&porcelain_type) {
            return self;
        }

        Router::<T>::new(RouterWithPorcelainType::<T> {
            router: self,
            porcelain_type,
            moniker: moniker.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::request_metadata::{protocol_metadata, Metadata, METADATA_KEY_TYPE};
    use assert_matches::assert_matches;
    use cm_rust::Availability;
    use cm_types::Name;
    use moniker::Moniker;
    use router_error::{DowncastErrorForTest, RouterError};
    use sandbox::{Capability, Data, Dict, WeakInstanceToken};
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeInstanceToken {}

    impl FakeInstanceToken {
        fn new() -> WeakInstanceToken {
            WeakInstanceToken { inner: Arc::new(FakeInstanceToken {}) }
        }
    }

    impl sandbox::WeakInstanceTokenAny for FakeInstanceToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn porcelain_type_good() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy = base.with_porcelain_type(CapabilityTypeName::Protocol, Moniker::root());
        let metadata = protocol_metadata(Availability::Optional);
        let capability = proxy
            .route(Some(Request { target: FakeInstanceToken::new(), metadata }), false)
            .await
            .unwrap();
        let capability = match capability {
            RouterResponse::<Data>::Capability(d) => d,
            c => panic!("Bad enum {:#?}", c),
        };
        assert_eq!(capability, Data::String("hello".to_string()));
    }

    #[fuchsia::test]
    async fn porcelain_type_bad() {
        let source = Data::String("hello".to_string());
        let base = Router::<Data>::new_ok(source);
        let proxy = base.with_porcelain_type(CapabilityTypeName::Protocol, Moniker::root());
        let metadata = Dict::new();
        metadata
            .insert(
                Name::new(METADATA_KEY_TYPE).unwrap(),
                Capability::Data(Data::String(String::from("directory"))),
            )
            .unwrap();
        metadata.set_metadata(Availability::Optional);
        let error = proxy
            .route(Some(Request { target: FakeInstanceToken::new(), metadata }), false)
            .await
            .unwrap_err();
        assert_matches!(
            error,
            RouterError::NotFound(err)
            if matches!(
                err.downcast_for_test::<RoutingError>(),
                RoutingError::BedrockWrongCapabilityType { actual: _, expected: _, moniker: _},
            )
        );
    }
}
