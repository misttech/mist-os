// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.cti

use std::sync::Arc;

use ::routing::RouteRequest;
use anyhow::Context;
use async_trait::async_trait;
use cm_types::Name;
use cm_util::TaskGroup;
use errors::CapabilityProviderError;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component as fcomponent;
use futures::TryStreamExt;
use lazy_static::lazy_static;
use moniker::{ExtendedMoniker, Moniker};
use routing::capability_source::InternalCapability;
use routing::error::RoutingError;
use routing::policy::PolicyError;
use tracing::warn;
use vfs::directory::entry::OpenRequest;

use crate::capability::{CapabilityProvider, FrameworkCapability, InternalCapabilityProvider};
use crate::model::component::WeakComponentInstance;
use crate::model::routing::report_routing_failure;
use crate::model::token::{InstanceRegistry, InstanceToken};

lazy_static! {
    static ref INTROSPECTOR_SERVICE: Name = "fuchsia.component.Introspector".parse().unwrap();
    static ref DEBUG_REQUEST: RouteRequest = RouteRequest::UseProtocol(cm_rust::UseProtocolDecl {
        source: cm_rust::UseSource::Framework,
        source_name: INTROSPECTOR_SERVICE.clone(),
        source_dictionary: Default::default(),
        target_path: cm_types::Path::new("/null").unwrap(),
        dependency_type: cm_rust::DependencyType::Strong,
        availability: Default::default(),
    });
}

struct IntrospectorCapability {
    scope_moniker: Moniker,
    instance_registry: Arc<InstanceRegistry>,
}

impl IntrospectorCapability {
    pub fn new(scope_moniker: Moniker, instance_registry: Arc<InstanceRegistry>) -> Self {
        Self { scope_moniker, instance_registry }
    }

    pub async fn serve(
        &self,
        mut stream: fcomponent::IntrospectorRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(request) = stream.try_next().await? {
            let method_name = request.method_name();
            self.handle_request(request)
                .await
                .with_context(|| format!("Error handling Introspector method {method_name}"))?;
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        request: fcomponent::IntrospectorRequest,
    ) -> Result<(), fidl::Error> {
        match request {
            fcomponent::IntrospectorRequest::GetMoniker { component_instance, responder } => {
                let token = InstanceToken::from(component_instance);
                let Some(Ok(moniker)) = self
                    .instance_registry
                    .get(&token)
                    .map(|moniker| moniker.strip_prefix(&self.scope_moniker))
                else {
                    return responder.send(Err(fcomponent::Error::InstanceNotFound));
                };
                return responder.send(Ok(&moniker.to_string()));
            }
            fcomponent::IntrospectorRequest::_UnknownMethod {
                ordinal,
                control_handle: _,
                method_type,
                ..
            } => {
                warn!(%ordinal, "Unknown {method_type:?} Introspector method");
                Ok(())
            }
        }
    }
}

#[async_trait]
impl InternalCapabilityProvider for IntrospectorCapability {
    async fn open_protocol(self: Box<Self>, server_end: zx::Channel) {
        let server_end = ServerEnd::<fcomponent::IntrospectorMarker>::new(server_end);
        let serve_result = self.serve(server_end.into_stream().unwrap()).await;
        if let Err(error) = serve_result {
            warn!(%error, "Error serving Introspector");
        }
    }
}

pub struct IntrospectorFrameworkCapability {
    pub instance_registry: Arc<InstanceRegistry>,
}

impl FrameworkCapability for IntrospectorFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&INTROSPECTOR_SERVICE)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        lazy_static! {
            static ref MEMORY_MONITOR: Moniker =
                Moniker::parse_str("/core/memory_monitor2").unwrap();
            /// Moniker for integration tests.
            static ref RECEIVER: Moniker =
                Moniker::parse_str("/receiver").unwrap();
            static ref TEST_REALMS: Moniker =
                Moniker::parse_str("/core/testing").unwrap();
            static ref STARNIX_TESTS: Name = "starnix-tests".parse().unwrap();
        };
        // TODO(https://fxbug.dev/318904493): Temporary workaround to prevent other components from
        // using `Introspector` while improvements to framework capability allowlists are under way.
        //
        // In production, the capability is minted at `/`, then offered to `/core/memory_monitor`.
        //
        // In the `introspector-integration-test`, the capability is minted at some test specific
        // realm, then exposed from `/`.
        //
        // In starnix tests, the capability is minted at some realm inside
        // `/core/testing/starnix-tests`, then used by some realm inside
        // `/core/testing/starnix-tests`.
        //
        // All other cases are disallowed.
        let is_starnix_test_realm = |moniker: &Moniker| match moniker.strip_prefix(&*TEST_REALMS) {
            Ok(mut rest) => match rest.next() {
                Some(realm) => realm.collection() == Some(&*STARNIX_TESTS),
                None => false,
            },
            Err(_) => false,
        };
        if target.moniker != *MEMORY_MONITOR
            && target.moniker != *RECEIVER
            && !target.moniker.is_root()
            && !(is_starnix_test_realm(&target.moniker) && is_starnix_test_realm(&scope.moniker))
        {
            return Box::new(AccessDeniedCapabilityProvider {
                target,
                source_moniker: scope.moniker,
            });
        }
        Box::new(IntrospectorCapability::new(scope.moniker, self.instance_registry.clone()))
    }
}

// TODO(https://fxbug.dev/318904493): Remove this.
struct AccessDeniedCapabilityProvider {
    target: WeakComponentInstance,
    source_moniker: Moniker,
}

#[async_trait]
impl CapabilityProvider for AccessDeniedCapabilityProvider {
    async fn open(
        self: Box<Self>,
        _task_group: TaskGroup,
        _open_request: OpenRequest<'_>,
    ) -> Result<(), CapabilityProviderError> {
        let err = RoutingError::from(PolicyError::CapabilityUseDisallowed {
            cap: INTROSPECTOR_SERVICE.to_string(),
            source_moniker: ExtendedMoniker::ComponentInstance(self.source_moniker),
            target_moniker: self.target.moniker.clone(),
        });
        if let Ok(target) = self.target.upgrade() {
            report_routing_failure(&*DEBUG_REQUEST, DEBUG_REQUEST.availability(), &target, &err)
                .await;
        }
        Err(err.into())
    }
}
