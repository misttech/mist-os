// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{format_err, Context as _, Error};
use fuchsia_inspect::Node as InspectNode;
use fuchsia_inspect_contrib::auto_persist;
use futures::channel::mpsc;
use futures::{future, select, Future, StreamExt, TryFutureExt};
use std::boxed::Box;
use tracing::error;
use windowed_stats::experimental::serve::serve_time_matrix_inspection;
use wlan_common::bss::BssDescription;
use {
    fidl_fuchsia_diagnostics_persist, fidl_fuchsia_metrics,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fuchsia_async as fasync, fuchsia_component,
    fuchsia_zircon as zx, wlan_legacy_metrics_registry as metrics,
};

mod processors;
pub(crate) mod util;
pub use crate::processors::connect_disconnect::DisconnectInfo;
pub use crate::processors::toggle_events::ClientConnectionsToggleEvent;
pub use util::sender::TelemetrySender;
#[cfg(test)]
mod testing;

// Service name to persist Inspect data across boots
const PERSISTENCE_SERVICE_PATH: &str = "/svc/fuchsia.diagnostics.persist.DataPersistence-wlan";

#[derive(Debug)]
pub enum TelemetryEvent {
    /// Report a connection result.
    ConnectResult { result: fidl_ieee80211::StatusCode, bss: Box<BssDescription> },
    /// Report a disconnection.
    Disconnect { info: DisconnectInfo },
    /// Report a client connections enabled or disabled event.
    ClientConnectionsToggle { event: ClientConnectionsToggleEvent },
}

/// Attempts to connect to the Cobalt service.
pub async fn setup_cobalt_proxy(
) -> Result<fidl_fuchsia_metrics::MetricEventLoggerProxy, anyhow::Error> {
    let cobalt_1dot1_svc = fuchsia_component::client::connect_to_protocol::<
        fidl_fuchsia_metrics::MetricEventLoggerFactoryMarker,
    >()
    .context("failed to connect to metrics service")?;

    let (cobalt_1dot1_proxy, cobalt_1dot1_server) =
        fidl::endpoints::create_proxy::<fidl_fuchsia_metrics::MetricEventLoggerMarker>()
            .context("failed to create MetricEventLoggerMarker endponts")?;

    let project_spec = fidl_fuchsia_metrics::ProjectSpec {
        customer_id: Some(metrics::CUSTOMER_ID),
        project_id: Some(metrics::PROJECT_ID),
        ..Default::default()
    };

    match cobalt_1dot1_svc.create_metric_event_logger(&project_spec, cobalt_1dot1_server).await {
        Ok(_) => Ok(cobalt_1dot1_proxy),
        Err(err) => Err(format_err!("failed to create metrics event logger: {:?}", err)),
    }
}

/// Attempts to create a disconnected FIDL channel with types matching the Cobalt service. This
/// allows for a fallback with a uniform code path in case of a failure to connect to Cobalt.
pub fn setup_disconnected_cobalt_proxy(
) -> Result<fidl_fuchsia_metrics::MetricEventLoggerProxy, anyhow::Error> {
    // Create a disconnected proxy
    fidl::endpoints::create_proxy::<fidl_fuchsia_metrics::MetricEventLoggerMarker>()
        .context("failed to create MetricEventLoggerMarker endpoints")
        .map(|(proxy, _)| proxy)
}

pub fn setup_persistence_req_sender(
) -> Result<(auto_persist::PersistenceReqSender, impl Future<Output = ()>), anyhow::Error> {
    fuchsia_component::client::connect_to_protocol_at_path::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
    >(PERSISTENCE_SERVICE_PATH)
    .map(|persistence_proxy| auto_persist::create_persistence_req_sender(persistence_proxy))
}

/// Creates a disconnected channel with the same types as the persistence service. This allows for
/// a fallback with a uniform code path in case of a failure to connect to the persistence service.
pub fn setup_disconnected_persistence_req_sender() -> auto_persist::PersistenceReqSender {
    let (sender, _receiver) = mpsc::channel::<String>(1);
    // Note: because we drop the receiver here, be careful about log spam when sending
    // tags through the `sender` below. This is automatically handled by `auto_persist::AutoPersist`
    // because it only logs the first time sending fails, so just use that wrapper type instead of
    // writing directly to this channel.
    sender
}

/// How often to refresh time series stats. Also how often to request packet counters.
const TELEMETRY_QUERY_INTERVAL: zx::Duration = zx::Duration::from_seconds(10);

pub fn serve_telemetry(
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    inspect_node: InspectNode,
    inspect_path: &str,
    persistence_req_sender: auto_persist::PersistenceReqSender,
) -> (TelemetrySender, impl Future<Output = Result<(), Error>>) {
    let (sender, mut receiver) =
        mpsc::channel::<TelemetryEvent>(util::sender::TELEMETRY_EVENT_BUFFER_SIZE);
    let sender = TelemetrySender::new(sender);

    // Inspect nodes to hold time series and metadata for other nodes
    const METADATA_NODE_NAME: &'static str = "metadata";
    let inspect_metadata_node = inspect_node.create_child(METADATA_NODE_NAME);
    let inspect_time_series_node = inspect_node.create_child("time_series");

    let (time_matrix_client, time_series_fut) =
        serve_time_matrix_inspection(inspect_time_series_node);

    // Create and initialize modules
    let connect_disconnect = processors::connect_disconnect::ConnectDisconnectLogger::new(
        cobalt_1dot1_proxy.clone(),
        &inspect_node,
        &inspect_metadata_node,
        &format!("{inspect_path}/{METADATA_NODE_NAME}"),
        persistence_req_sender,
        &time_matrix_client,
    );

    let mut toggle_logger =
        processors::toggle_events::ToggleLogger::new(cobalt_1dot1_proxy, &inspect_node);

    let fut = async move {
        // Prevent the inspect nodes from being dropped while the loop is running.
        let _inspect_node = inspect_node;
        let _inspect_metadata_node = inspect_metadata_node;

        let mut telemetry_interval = fasync::Interval::new(TELEMETRY_QUERY_INTERVAL);
        loop {
            select! {
                event = receiver.next() => {
                    let Some(event) = event else {
                        error!("Telemetry event stream unexpectedly terminated.");
                        return Err(format_err!("Telemetry event stream unexpectedly terminated."));
                    };
                    use TelemetryEvent::*;
                    match event {
                        ConnectResult { result, bss } => {
                            connect_disconnect.log_connect_attempt(result, &bss).await;
                        }
                        Disconnect { info } => {
                            connect_disconnect.log_disconnect(&info).await;
                        }
                        ClientConnectionsToggle { event } => {
                            toggle_logger.log_toggle_event(event).await;
                        }
                    }
                }
                _ = telemetry_interval.next() => {
                    connect_disconnect.handle_periodic_telemetry();
                }
            }
        }
    };
    let fut = future::try_join(fut, time_series_fut).map_ok(|((), ())| ());
    (sender, fut)
}
