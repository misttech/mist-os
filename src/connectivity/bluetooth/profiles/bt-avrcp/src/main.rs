// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "1024"]

use {
    anyhow::{format_err, Context, Error},
    fuchsia_bluetooth::profile::{psm_from_protocol, Psm},
    fuchsia_bluetooth::types::PeerId,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect as inspect,
    fuchsia_inspect_derive::Inspect,
    futures::{channel::mpsc, stream::StreamExt, FutureExt},
    profile_client::ProfileEvent,
    tracing::{error, info, warn},
};

mod browse_controller_service;
mod controller_service;
mod metrics;
mod packets;
mod peer;
mod peer_manager;
mod profile;
mod service;
mod types;

use crate::{
    metrics::{MetricsNode, METRICS_NODE_NAME},
    peer_manager::PeerManager,
    profile::{AvrcpService, AvrcpTargetFeatures},
};

fn record_avrcp_capabilities(
    metrics_proxy: Option<fidl_fuchsia_metrics::MetricEventLoggerProxy>,
    service: &AvrcpService,
    peer_id: PeerId,
) {
    let Some(metrics_proxy) = metrics_proxy else {
        return;
    };
    let mut event_codes_list = Vec::new();
    let remote_role = match service {
        AvrcpService::Controller { .. } => {
            bt_metrics::AvrcpRemotePeerCapabilitiesMetricDimensionRemoteRole::Controller
        }
        AvrcpService::Target { features, .. } => {
            let remote_role =
                bt_metrics::AvrcpRemotePeerCapabilitiesMetricDimensionRemoteRole::Target;
            if features.contains(AvrcpTargetFeatures::SUPPORTSCOVERART) {
                event_codes_list.push(vec![
                    remote_role as u32,
                    bt_metrics::AvrcpRemotePeerCapabilitiesMetricDimensionFeature::SupportsCoverArt
                        as u32,
                ]);
            }
            remote_role
        }
    };
    if service.supports_browsing() {
        event_codes_list.push(vec![
            remote_role as u32,
            bt_metrics::AvrcpRemotePeerCapabilitiesMetricDimensionFeature::SupportsBrowsing as u32,
        ]);
    }
    // Log the occurrences (in the background)
    fuchsia_async::Task::spawn(async move {
        for event_codes in event_codes_list {
            bt_metrics::log_on_failure(
                metrics_proxy
                    .log_occurrence(
                        bt_metrics::AVRCP_REMOTE_PEER_CAPABILITIES_METRIC_ID,
                        1,
                        &event_codes,
                    )
                    .await,
            );
        }
        info!("Finished recording AVRCP capabilities for {peer_id}");
    })
    .detach();
}

#[fuchsia::main(logging_tags = ["avrcp"])]
async fn main() -> Result<(), Error> {
    // Begin searching for AVRCP target/controller SDP records on newly connected remote peers
    // and register our AVRCP service with the `bredr.Profile` service.
    let (profile_proxy, mut profile_client) =
        profile::connect_and_advertise().context("Unable to connect to BrEdr Profile Service")?;

    // Create a channel that peer manager will receive requests for peer controllers from the FIDL
    // service runner.
    // TODO(fxbug.dev/44330) handle back pressure correctly and reduce mpsc::channel buffer sizes.
    let (client_sender, mut service_request_receiver) = mpsc::channel(512);

    let mut fs = ServiceFs::new();

    let inspect = inspect::Inspector::default();
    inspect_runtime::serve(&inspect, &mut fs)?;

    let mut peer_manager = PeerManager::new(profile_proxy);
    if let Err(e) = peer_manager.iattach(inspect.root(), "peers") {
        warn!("Failed to attach to inspect: {:?}", e);
    }

    // Set up cobalt 1.1 logger.
    let cobalt = bt_metrics::create_metrics_logger()
        .map_err(|e| warn!("Failed to create metrics: {e}"))
        .ok();

    let mut metrics_node = MetricsNode::default().with_cobalt_proxy(cobalt.clone());
    if let Err(e) = metrics_node.iattach(inspect.root(), METRICS_NODE_NAME) {
        warn!("Failed to attach to inspect metrics: {:?}", e);
    }
    peer_manager.set_metrics_node(metrics_node);

    let mut service_fut = service::run_services(fs, client_sender)
        .expect("Unable to start AVRCP FIDL service")
        .fuse();

    loop {
        futures::select! {
            request = profile_client.next() => {
                let request = match request {
                    None => return Err(format_err!("BR/EDR Profile unexpectedly closed")),
                    Some(Err(e)) => return Err(format_err!("Profile client error: {e:?}")),
                    Some(Ok(r)) => r,
                };
                match request {
                    ProfileEvent::PeerConnected { id, protocol, channel } => {
                        info!("Incoming connection request from {id} with protocol: {protocol:?}");
                        let protocol = protocol.iter().map(Into::into).collect();
                        match psm_from_protocol(&protocol) {
                            Some(Psm::AVCTP) => {
                                peer_manager.new_control_connection(&id, channel);
                            },
                            Some(Psm::AVCTP_BROWSE) => {
                                peer_manager.new_browse_connection(&id, channel);
                            },
                            _ => {
                                info!("Received connection over non-AVRCP protocol: {protocol:?}");
                            },
                        }
                    },
                    ProfileEvent::SearchResult { id, protocol, attributes } => {
                        let protocol = match protocol {
                            Some(p) => p,
                            None => {
                                info!("Received search result with no protocol, ignoring..");
                                continue;
                            }
                        };
                        match AvrcpService::from_search_result(protocol, attributes) {
                            Ok(service) => {
                                info!("Valid service found on {id}: {service:?}");
                                peer_manager.services_found(&id, vec![service]);
                                record_avrcp_capabilities(cobalt.clone(), &service, id.into());
                            }
                            Err(e) => {
                                warn!("Invalid service found: {e:?}");
                            }
                        }
                    },
                }
            }
            request = service_request_receiver.select_next_some() => {
                peer_manager.handle_service_request(request);
            },
            service_result = service_fut => {
                error!("Service task finished unexpectedly: {service_result:?}");
                break;
            },
            complete => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bt_metrics::respond_to_metrics_req_for_test;
    use fidl_fuchsia_metrics::*;
    use fuchsia_async::{Time, WaitState};

    use crate::profile::{AvrcpControllerFeatures, AvrcpProtocolVersion};

    #[fuchsia::test]
    fn record_target_peer_capabilities() {
        let mut exec = fuchsia_async::TestExecutor::new();
        let (proxy, mut receiver) =
            fidl::endpoints::create_proxy_and_stream::<MetricEventLoggerMarker>()
                .expect("failed to create MetricsEventLogger proxy");

        // Target with browsing and cover art features.
        let test_target = AvrcpService::Target {
            features: AvrcpTargetFeatures::from_bits_truncate(0b101000000),
            psm: Psm::AVCTP,
            protocol_version: AvrcpProtocolVersion(1, 6),
        };
        // Spins off a Task to record to metrics.
        record_avrcp_capabilities(Some(proxy), &test_target, PeerId(1));

        let mut cobalt_recv_fut = receiver.next();
        let log_request = exec.run_singlethreaded(&mut cobalt_recv_fut);

        // First log request for cover art.
        let got = respond_to_metrics_req_for_test(log_request.unwrap().expect("should be ok"));
        assert_eq!(bt_metrics::AVRCP_REMOTE_PEER_CAPABILITIES_METRIC_ID, got.metric_id);
        assert_eq!(vec![0, 1], got.event_codes);
        assert_eq!(MetricEventPayload::Count(1), got.payload);

        let mut cobalt_recv_fut = receiver.next();
        let log_request = exec.run_singlethreaded(&mut cobalt_recv_fut);

        // Second log request for browsing.
        let got = respond_to_metrics_req_for_test(log_request.unwrap().expect("should be ok"));
        assert_eq!(bt_metrics::AVRCP_REMOTE_PEER_CAPABILITIES_METRIC_ID, got.metric_id);
        assert_eq!(vec![0, 0], got.event_codes);
        assert_eq!(MetricEventPayload::Count(1), got.payload);

        // Running the background task should empty out the executor of things to do.
        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        assert_eq!(WaitState::Waiting(Time::INFINITE), exec.is_waiting());
    }

    #[fuchsia::test]
    fn record_controller_peer_capabilities() {
        let mut exec = fuchsia_async::TestExecutor::new();
        let (proxy, mut receiver) =
            fidl::endpoints::create_proxy_and_stream::<MetricEventLoggerMarker>()
                .expect("failed to create MetricsEventLogger proxy");

        // Target with browsing and cover art features.
        let test_controller = AvrcpService::Controller {
            features: AvrcpControllerFeatures::from_bits_truncate(0b1000000),
            psm: Psm::AVCTP,
            protocol_version: AvrcpProtocolVersion(1, 6),
        };
        // Spins off a Task to record to metrics.
        record_avrcp_capabilities(Some(proxy), &test_controller, PeerId(1));

        let mut cobalt_recv_fut = receiver.next();
        let log_request = exec.run_singlethreaded(&mut cobalt_recv_fut);

        // Log request for browsing.
        let got = respond_to_metrics_req_for_test(log_request.unwrap().expect("should be ok"));
        assert_eq!(bt_metrics::AVRCP_REMOTE_PEER_CAPABILITIES_METRIC_ID, got.metric_id);
        assert_eq!(vec![1, 0], got.event_codes);
        assert_eq!(MetricEventPayload::Count(1), got.payload);

        // Running the background task should empty out the executor of things to do.
        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        assert_eq!(WaitState::Waiting(Time::INFINITE), exec.is_waiting());
    }
}
