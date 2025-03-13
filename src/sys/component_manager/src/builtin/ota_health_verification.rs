// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::manager::ComponentManagerInstance;
use ::routing::error::RoutingError;
use anyhow::Context;
use cm_types::Name;
use fidl::endpoints::{create_proxy, DiscoverableProtocolMarker};
use fidl_fuchsia_update_verify as fupdate;
use futures::TryStreamExt;
use moniker::{ExtendedMoniker, Moniker};
use router_error::RouterError;
use sandbox::{Capability, Message, RouterResponse};
use std::sync::{Arc, Weak};

#[derive(Debug, Clone)]
pub struct OtaHealthVerification {
    health_checks: Vec<Moniker>,
    root: Weak<ComponentManagerInstance>,
}

impl OtaHealthVerification {
    pub fn new(health_checks: Vec<String>, root: Weak<ComponentManagerInstance>) -> Arc<Self> {
        Arc::new(Self {
            health_checks: health_checks
                .into_iter()
                .map(|s| s.as_str().try_into().unwrap())
                .collect(),
            root,
        })
    }

    pub async fn serve(
        self: Arc<Self>,
        mut stream: fupdate::HealthVerificationRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(request) = stream.try_next().await? {
            self.handle_request(request)
                .await
                .with_context(|| format!("OtaHealthVerification server failed"))?;
        }
        Ok(())
    }

    async fn handle_request(
        &self,
        request: fupdate::HealthVerificationRequest,
    ) -> Result<(), anyhow::Error> {
        match request {
            fupdate::HealthVerificationRequest::QueryHealthChecks { responder } => {
                let mut healthy_count = 0;
                for moniker in &self.health_checks {
                    let health_check_status = match open_protocol(moniker, self.root.clone()).await
                    {
                        Ok(health_check_status) => health_check_status,
                        Err(e) => {
                            log::error!(
                                "Error trying to connect to ComponentOtaHealthCheckProxy: {:?}",
                                e
                            );
                            continue;
                        }
                    };
                    let res = health_check_status.get_health_status().await;
                    match res {
                        Ok(fupdate::HealthStatus::Unhealthy) => {
                            log::error!("{:?} reported unhealthy status", moniker);
                        }
                        Ok(fupdate::HealthStatus::Healthy) => {
                            healthy_count += 1;
                        }
                        Err(_e) => {
                            return Ok(responder.send(zx::sys::ZX_ERR_INTERNAL)?);
                        }
                    }
                }

                if healthy_count != self.health_checks.len() {
                    return Ok(responder.send(zx::sys::ZX_ERR_BAD_STATE)?);
                }

                // All health checks responded with healthy!
                return Ok(responder.send(zx::sys::ZX_OK)?);
            }
        }
    }
}

async fn open_protocol(
    moniker: &Moniker,
    top_instance: Weak<ComponentManagerInstance>,
) -> Result<fupdate::ComponentOtaHealthCheckProxy, RouterError> {
    let instance = top_instance
        .upgrade()
        .ok_or(RouterError::Internal)?
        .root()
        .find(moniker)
        .await
        .ok_or_else(|| {
            RoutingError::expose_from_framework_not_found(
                &moniker,
                fupdate::ComponentOtaHealthCheckMarker::PROTOCOL_NAME,
            )
        })?;

    let instance_output = instance.lock_resolved_state().await?.sandbox.component_output.clone();
    let capability = instance_output
        .framework()
        .get(&Name::new(fupdate::ComponentOtaHealthCheckMarker::PROTOCOL_NAME).unwrap())
        .map_err(|_| RouterError::Internal)?
        .ok_or_else(|| {
            RoutingError::expose_from_self_not_found(
                &moniker,
                fupdate::ComponentOtaHealthCheckMarker::PROTOCOL_NAME,
            )
        })?;
    let router = match capability {
        Capability::ConnectorRouter(r) => r,
        other_type => {
            return Err(RoutingError::BedrockWrongCapabilityType {
                actual: format!("{:?}", other_type),
                expected: "ConnectorRouter".to_string(),
                moniker: ExtendedMoniker::from(moniker.clone()),
            }
            .into())
        }
    };
    let connector = match router.route(None, false).await? {
        RouterResponse::Capability(cap) => cap,
        RouterResponse::Unavailable => {
            return Err(RoutingError::RouteUnexpectedUnavailable {
                type_name: cm_rust::CapabilityTypeName::Protocol,
                moniker: ExtendedMoniker::from(moniker.clone()),
            }
            .into())
        }
        RouterResponse::Debug(_) => {
            log::error!("received unexpected debug data when routing for OTA health checks");
            return Err(RouterError::Internal);
        }
    };

    let (proxy, server_end) = create_proxy::<fupdate::ComponentOtaHealthCheckMarker>();
    connector.send(Message { channel: server_end.into_channel() }).map_err(|_| {
        RoutingError::BedrockFailedToSend {
            moniker: ExtendedMoniker::from(moniker.clone()),
            capability_id: fupdate::ComponentOtaHealthCheckMarker::PROTOCOL_NAME.to_string(),
        }
    })?;
    Ok(proxy)
}
