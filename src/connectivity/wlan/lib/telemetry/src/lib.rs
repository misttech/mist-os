// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(339221340): remove these allows once the skeleton has a few uses
#![allow(unused)]

use {
    fidl_fuchsia_metrics, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_inspect::Node as InspectNode,
    fuchsia_inspect_contrib::auto_persist,
    futures::{channel::mpsc, Future, StreamExt},
    std::boxed::Box,
    tracing::error,
    wlan_common::bss::BssDescription,
};

mod processors;
pub(crate) mod util;
pub use util::sender::TelemetrySender;
#[cfg(test)]
mod testing;

#[cfg_attr(test, derive(Debug))]
pub enum TelemetryEvent {
    /// Report a connection result.
    ConnectResult { result: fidl_sme::ConnectResult, bss: Box<BssDescription> },
    /// Report a disconnection.
    Disconnect,
}

pub fn serve_telemetry(
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    inspect_node: InspectNode,
    persistence_req_sender: auto_persist::PersistenceReqSender,
) -> (TelemetrySender, impl Future<Output = ()>) {
    let (sender, mut receiver) =
        mpsc::channel::<TelemetryEvent>(util::sender::TELEMETRY_EVENT_BUFFER_SIZE);
    let sender = TelemetrySender::new(sender);

    // Create and initialize modules
    let connect_disconnect = processors::connect_disconnect::ConnectDisconnectLogger::new(
        cobalt_1dot1_proxy,
        inspect_node,
        persistence_req_sender,
    );

    let fut = async move {
        loop {
            let Some(event) = receiver.next().await else {
                error!("Telemetry event stream unexpectedly terminated.");
                break;
            };

            use TelemetryEvent::*;
            match event {
                ConnectResult { result, bss } => {
                    connect_disconnect.log_connect_attempt(result, &bss).await;
                }
                Disconnect => {
                    connect_disconnect.log_disconnect().await;
                }
            }
        }
    };
    (sender, fut)
}

#[cfg(test)]
mod tests {}
