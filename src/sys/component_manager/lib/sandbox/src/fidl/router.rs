// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{Capability, Request, Router};
use fidl::handle::{AsHandleRef, EventPair};
use fidl_fuchsia_component_sandbox as fsandbox;
use futures::TryStreamExt;
use zx::Koid;

impl From<Request> for fsandbox::RouteRequest {
    fn from(request: Request) -> Self {
        let (token, server) = EventPair::create();
        request.target.register(token.get_koid().unwrap(), server);
        fsandbox::RouteRequest {
            requesting: Some(fsandbox::InstanceToken { token }),
            metadata: Some(request.metadata.into()),
            ..Default::default()
        }
    }
}

// TODO(b/314343346): Complete or remove the Router implementation of sandbox::Capability
impl crate::RemotableCapability for Router {}

impl From<Router> for fsandbox::Capability {
    fn from(router: Router) -> Self {
        let (client_end, sender_stream) =
            fidl::endpoints::create_request_stream::<fsandbox::RouterMarker>().unwrap();
        router.serve_and_register(sender_stream, client_end.get_koid().unwrap());
        fsandbox::Capability::Router(client_end)
    }
}

impl Router {
    async fn serve_router(
        self,
        mut stream: fsandbox::RouterRequestStream,
    ) -> Result<(), fidl::Error> {
        async fn do_route(
            router: &Router,
            payload: fsandbox::RouteRequest,
        ) -> Result<fsandbox::Capability, fsandbox::RouterError> {
            let cap = match (payload.requesting, payload.metadata) {
                (Some(token), Some(metadata)) => {
                    let capability =
                        crate::fidl::registry::get(token.token.as_handle_ref().get_koid().unwrap());
                    let component = match capability {
                        Some(crate::Capability::Instance(c)) => c,
                        Some(_) => return Err(fsandbox::RouterError::InvalidArgs),
                        None => return Err(fsandbox::RouterError::InvalidArgs),
                    };
                    let Capability::Dictionary(metadata) =
                        Capability::try_from(fsandbox::Capability::Dictionary(metadata)).unwrap()
                    else {
                        return Err(fsandbox::RouterError::InvalidArgs);
                    };
                    let request = Request { target: component, metadata };
                    router.route(Some(request), false).await?
                }
                (None, None) => router.route(None, false).await?,
                _ => {
                    return Err(fsandbox::RouterError::InvalidArgs);
                }
            };
            Ok(cap.into())
        }

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::RouterRequest::Route { payload, responder } => {
                    responder.send(do_route(&self, payload).await)?;
                }
                fsandbox::RouterRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!("Received unknown Router request with ordinal {ordinal}");
                }
            }
        }
        Ok(())
    }

    /// Serves the `fuchsia.sandbox.Router` protocol and moves ourself into the registry.
    pub fn serve_and_register(self, stream: fsandbox::RouterRequestStream, koid: Koid) {
        let router = self.clone();

        // Move this capability into the registry.
        crate::fidl::registry::insert(self.into(), koid, async move {
            router.serve_router(stream).await.expect("failed to serve Router");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Connector, Data, Dict, DictKey, WeakInstanceToken};
    use assert_matches::assert_matches;
    use cm_types::Availability;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FakeInstanceToken {}

    impl FakeInstanceToken {
        fn new() -> WeakInstanceToken {
            WeakInstanceToken { inner: Arc::new(FakeInstanceToken {}) }
        }
    }

    impl crate::WeakInstanceTokenAny for FakeInstanceToken {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[fuchsia::test]
    async fn serve_router() {
        let component = FakeInstanceToken::new();
        let (component_client, server) = EventPair::create();
        let koid = server.basic_info().unwrap().related_koid;
        component.register(koid, server);

        let (_, sender) = Connector::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        let metadata = Dict::new();
        let key = DictKey::new("availability").expect("dict key creation failed unexpectedly");
        metadata
            .insert(key, Capability::Data(Data::String(Availability::Required.to_string())))
            .unwrap();
        let capability = client
            .route(fsandbox::RouteRequest {
                requesting: Some(fsandbox::InstanceToken { token: component_client }),
                metadata: Some(metadata.into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        assert_matches!(capability, fsandbox::Capability::Connector(_));
    }

    #[fuchsia::test]
    async fn serve_router_bad_arguments() {
        let (_, sender) = Connector::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        let metadata = Dict::new();
        metadata
            .insert(
                DictKey::new("availability").unwrap(),
                Capability::Data(Data::String(Availability::Required.to_string())),
            )
            .unwrap();
        // Check with no component token.
        let capability = client
            .route(fsandbox::RouteRequest {
                requesting: None,
                metadata: Some(metadata.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));

        let component = FakeInstanceToken::new();
        let (component_client, server) = EventPair::create();
        let koid = server.basic_info().unwrap().related_koid;
        component.register(koid, server);

        // Check with no availability.
        let capability = client
            .route(fsandbox::RouteRequest {
                requesting: Some(fsandbox::InstanceToken { token: component_client }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));
    }

    #[fuchsia::test]
    async fn serve_router_bad_token() {
        let (_, sender) = Connector::new();
        let router = Router::new_ok(sender);
        let (client, stream) =
            fidl::endpoints::create_proxy_and_stream::<fsandbox::RouterMarker>().unwrap();
        let _stream = fuchsia_async::Task::spawn(router.serve_router(stream));

        // Create the client but don't register it.
        let (component_client, _server) = EventPair::create();

        let metadata = Dict::new();
        metadata
            .insert(
                DictKey::new("availability").unwrap(),
                Capability::Data(Data::String(String::from("required"))),
            )
            .unwrap();
        let capability = client
            .route(fsandbox::RouteRequest {
                requesting: Some(fsandbox::InstanceToken { token: component_client }),
                metadata: Some(metadata.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_matches!(capability, Err(fsandbox::RouterError::InvalidArgs));
    }
}
