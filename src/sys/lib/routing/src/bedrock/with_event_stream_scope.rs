// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::Metadata;
use async_trait::async_trait;
use cm_rust::{EventScope, NativeIntoFidl, OfferDecl};
use fidl_fuchsia_component_internal as finternal;
use moniker::Moniker;
use router_error::RouterError;
use sandbox::{Connector, Data, Dict, DirEntry, Request, Routable, Router, RouterResponse};

struct EventStreamScopeRouter {
    router: Router<Dict>,
    scope_moniker: Moniker,
    scope: Vec<EventScope>,
}

#[async_trait]
impl Routable<Dict> for EventStreamScopeRouter {
    async fn route(
        &self,
        request: Option<Request>,
        debug: bool,
    ) -> Result<RouterResponse<Dict>, RouterError> {
        let request = request.ok_or_else(|| RouterError::InvalidArgs)?;
        let mut route_metadata: finternal::EventStreamRouteMetadata =
            request.metadata.get_metadata().expect("missing event stream route request metadata");
        // If the scope is already set then it's a smaller scope (because we can't expose these),
        // so only set our scope if the request doesn't have one yet.
        if route_metadata.scope.is_none() {
            route_metadata.scope_moniker = Some(self.scope_moniker.to_string());
            route_metadata.scope = Some(self.scope.clone().native_into_fidl());
            request.metadata.set_metadata(route_metadata);
        }
        self.router.route(Some(request), debug).await
    }
}

pub trait WithEventStreamScope {
    /// This will be a no-op if `offer` is not `OfferDecl::EventStream` or if the offer does not
    /// set a scope.
    fn with_event_stream_scope(self, scope_moniker: Moniker, offer: OfferDecl) -> Self;
}

impl WithEventStreamScope for Router<Connector> {
    fn with_event_stream_scope(self, _scope_moniker: Moniker, _offer: OfferDecl) -> Self {
        self
    }
}

impl WithEventStreamScope for Router<Data> {
    fn with_event_stream_scope(self, _scope_moniker: Moniker, _offer: OfferDecl) -> Self {
        self
    }
}

impl WithEventStreamScope for Router<Dict> {
    fn with_event_stream_scope(self, scope_moniker: Moniker, offer: OfferDecl) -> Self {
        let OfferDecl::EventStream(offer_event_stream_decl) = offer else {
            return self;
        };
        let Some(scope) = offer_event_stream_decl.scope else {
            return self;
        };
        Router::new(EventStreamScopeRouter { router: self, scope_moniker, scope })
    }
}

impl WithEventStreamScope for Router<DirEntry> {
    fn with_event_stream_scope(self, _scope_moniker: Moniker, _offer: OfferDecl) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::request_metadata::event_stream_metadata;
    use fidl_fuchsia_component_decl as fdecl;
    use futures::FutureExt;
    use std::sync::Arc;

    fn responding_router() -> Router<Dict> {
        Router::new(|request: Option<Request>, _debug: bool| {
            async move { Ok(RouterResponse::Capability(request.unwrap().metadata)) }.boxed()
        })
    }

    fn get_offer_decl_with_scope(scope: Option<Vec<cm_rust::EventScope>>) -> OfferDecl {
        OfferDecl::EventStream(cm_rust::OfferEventStreamDecl {
            source: cm_rust::OfferSource::Parent,
            scope,
            source_name: "started".parse().unwrap(),
            target: cm_rust::OfferTarget::Collection("hippo".parse().unwrap()),
            target_name: "started".parse().unwrap(),
            availability: cm_rust::Availability::Required,
        })
    }

    #[derive(Debug)]
    struct FakeInstanceToken {}
    impl sandbox::WeakInstanceTokenAny for FakeInstanceToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    impl From<FakeInstanceToken> for sandbox::WeakInstanceToken {
        fn from(f: FakeInstanceToken) -> Self {
            Self { inner: Arc::new(f) }
        }
    }

    #[fuchsia::test]
    async fn scope_not_yet_set() {
        let metadata = event_stream_metadata(
            cm_rust::Availability::Required,
            finternal::EventStreamRouteMetadata {
                scope_moniker: None,
                scope: None,
                ..Default::default()
            },
        );
        let scope = Some(vec![cm_rust::EventScope::Child(cm_rust::ChildRef {
            name: "bar".parse().unwrap(),
            collection: Some("baz".parse().unwrap()),
        })]);
        let offer = get_offer_decl_with_scope(scope.clone());
        let router = responding_router().with_event_stream_scope("foo".parse().unwrap(), offer);
        let request = Request { metadata, target: FakeInstanceToken {}.into() };
        let result = match router.route(Some(request), false).await {
            Ok(RouterResponse::Capability(dictionary)) => dictionary,
            other_response => panic!("unexpected response: {:?}", other_response),
        };
        let route_metadata: finternal::EventStreamRouteMetadata = result.get_metadata().unwrap();

        assert_eq!(route_metadata.scope_moniker, Some("foo".to_string()));
        assert_eq!(route_metadata.scope, scope.map(|s| s.native_into_fidl()));
    }

    #[fuchsia::test]
    async fn scope_already_set() {
        let metadata = event_stream_metadata(
            cm_rust::Availability::Required,
            finternal::EventStreamRouteMetadata {
                scope_moniker: Some("bar/baz".to_string()),
                scope: Some(vec![fdecl::Ref::Collection(fdecl::CollectionRef {
                    name: "hippos".to_string(),
                })]),
                ..Default::default()
            },
        );
        let scope = Some(vec![cm_rust::EventScope::Child(cm_rust::ChildRef {
            name: "bar".parse().unwrap(),
            collection: Some("baz".parse().unwrap()),
        })]);
        let offer = get_offer_decl_with_scope(scope.clone());
        let router = responding_router().with_event_stream_scope("foo".parse().unwrap(), offer);
        let request = Request { metadata, target: FakeInstanceToken {}.into() };
        let result = match router.route(Some(request), false).await {
            Ok(RouterResponse::Capability(dictionary)) => dictionary,
            other_response => panic!("unexpected response: {:?}", other_response),
        };
        let route_metadata: finternal::EventStreamRouteMetadata = result.get_metadata().unwrap();

        assert_eq!(route_metadata.scope_moniker, Some("bar/baz".to_string()));
        assert_eq!(
            route_metadata.scope,
            Some(vec![fdecl::Ref::Collection(fdecl::CollectionRef { name: "hippos".to_string() })])
        );
    }
}
