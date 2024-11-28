// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async::TimeoutExt;
use fuchsia_sync::Mutex;

use std::sync::Arc;
use tracing::{error, warn};
use windowed_stats::experimental::clock::Timed;
use windowed_stats::experimental::series::interpolation::LastAggregation;
use windowed_stats::experimental::series::statistic::LatchMax;
use windowed_stats::experimental::series::{SamplingProfile, TimeMatrix};
use windowed_stats::experimental::serve::{InspectedTimeMatrix, TimeMatrixClient};

// Include a timeout on stats calls so that if the driver deadlocks, telemtry doesn't get stuck.
const GET_IFACE_STATS_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);

enum IfaceState {
    NotAvailable,
    Created { iface_id: u16, telemetry_proxy: Option<fidl_fuchsia_wlan_sme::TelemetryProxy> },
}

pub struct ClientIfaceCountersLogger {
    iface_state: Arc<Mutex<IfaceState>>,
    monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    time_series_stats: IfaceCountersTimeSeries,
}

impl ClientIfaceCountersLogger {
    pub fn new(
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        time_matrix_client: &TimeMatrixClient,
    ) -> Self {
        Self::new_helper(monitor_svc_proxy, IfaceCountersTimeSeries::new(&time_matrix_client))
    }

    fn new_helper(
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        time_series_stats: IfaceCountersTimeSeries,
    ) -> Self {
        Self {
            iface_state: Arc::new(Mutex::new(IfaceState::NotAvailable)),
            monitor_svc_proxy,
            time_series_stats,
        }
    }

    pub async fn handle_iface_created(&self, iface_id: u16) {
        let (proxy, server) = fidl::endpoints::create_proxy();
        let telemetry_proxy = match self.monitor_svc_proxy.get_sme_telemetry(iface_id, server).await
        {
            Ok(Ok(())) => Some(proxy),
            Ok(Err(e)) => {
                error!("Request for SME telemetry for iface {} completed with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
            Err(e) => {
                error!("Failed to request SME telemetry for iface {} with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
        };
        *self.iface_state.lock() = IfaceState::Created { iface_id, telemetry_proxy }
    }

    pub async fn handle_iface_destroyed(&self, iface_id: u16) {
        let destroyed = match *self.iface_state.lock() {
            IfaceState::Created { iface_id: existing_iface_id, .. }
                if iface_id == existing_iface_id =>
            {
                true
            }
            _ => false,
        };
        if destroyed {
            *self.iface_state.lock() = IfaceState::NotAvailable;
        }
    }

    pub async fn handle_periodic_telemetry(&self, is_connected: bool) {
        match &*self.iface_state.lock() {
            IfaceState::NotAvailable => (),
            IfaceState::Created { telemetry_proxy, .. } => {
                if let Some(telemetry_proxy) = &telemetry_proxy {
                    match telemetry_proxy
                        .get_counter_stats()
                        .on_timeout(GET_IFACE_STATS_TIMEOUT, || {
                            warn!("Timed out waiting for counter stats");
                            Ok(Err(zx::Status::TIMED_OUT.into_raw()))
                        })
                        .await
                    {
                        Ok(Ok(stats)) => {
                            self.time_series_stats.log_rx_unicast_total(stats.rx_unicast_total);
                            self.time_series_stats.log_rx_unicast_drop(stats.rx_unicast_drop);
                            self.time_series_stats.log_tx_total(stats.tx_total);
                            self.time_series_stats.log_tx_drop(stats.tx_drop);
                        }
                        error => {
                            // It's normal for this call to fail while the device is not connected,
                            // so suppress the warning if that's the case.
                            if is_connected {
                                warn!("Failed to get interface stats: {:?}", error);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct IfaceCountersTimeSeries {
    rx_unicast_total: InspectedTimeMatrix<u64>,
    rx_unicast_drop: InspectedTimeMatrix<u64>,
    tx_total: InspectedTimeMatrix<u64>,
    tx_drop: InspectedTimeMatrix<u64>,
}

impl IfaceCountersTimeSeries {
    pub fn new(client: &TimeMatrixClient) -> Self {
        let rx_unicast_total = client.inspect_time_matrix(
            "rx_unicast_total",
            TimeMatrix::<LatchMax<u64>, LastAggregation>::new(
                SamplingProfile::balanced(),
                LastAggregation::or(0),
            ),
        );
        let rx_unicast_drop = client.inspect_time_matrix(
            "rx_unicast_drop",
            TimeMatrix::<LatchMax<u64>, LastAggregation>::new(
                SamplingProfile::balanced(),
                LastAggregation::or(0),
            ),
        );
        let tx_total = client.inspect_time_matrix(
            "tx_total",
            TimeMatrix::<LatchMax<u64>, LastAggregation>::new(
                SamplingProfile::balanced(),
                LastAggregation::or(0),
            ),
        );
        let tx_drop = client.inspect_time_matrix(
            "tx_drop",
            TimeMatrix::<LatchMax<u64>, LastAggregation>::new(
                SamplingProfile::balanced(),
                LastAggregation::or(0),
            ),
        );
        Self { rx_unicast_total, rx_unicast_drop, tx_total, tx_drop }
    }

    fn log_rx_unicast_total(&self, data: u64) {
        self.rx_unicast_total.fold_or_log_error(Timed::now(data));
    }

    fn log_rx_unicast_drop(&self, data: u64) {
        self.rx_unicast_drop.fold_or_log_error(Timed::now(data));
    }

    fn log_tx_total(&self, data: u64) {
        self.tx_total.fold_or_log_error(Timed::now(data));
    }

    fn log_tx_drop(&self, data: u64) {
        self.tx_drop.fold_or_log_error(Timed::now(data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use futures::TryStreamExt;
    use std::pin::pin;
    use std::task::Poll;
    use windowed_stats::experimental::testing::{MockTimeMatrix, TimeMatrixCall};

    const IFACE_ID: u16 = 66;

    #[fuchsia::test]
    fn test_handle_periodic_telemetry() {
        let mut test_helper = setup_test();
        let time_series = MockIfaceCountersTimeSeries::default();
        let logger = ClientIfaceCountersLogger::new_helper(
            test_helper.monitor_svc_proxy.clone(),
            time_series.build(),
        );

        // Transition to IfaceCreated state
        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Ready(())
        );

        let is_connected = true;
        let mut test_fut = pin!(logger.handle_periodic_telemetry(is_connected));
        let counter_stats = fidl_fuchsia_wlan_stats::IfaceCounterStats {
            rx_unicast_total: 100,
            rx_unicast_drop: 5,
            rx_multicast: 30,
            tx_total: 50,
            tx_drop: 2,
        };
        assert_eq!(
            test_helper.run_and_respond_iface_counter_stats_req(&mut test_fut, Ok(&counter_stats)),
            Poll::Ready(())
        );

        assert_eq!(
            &time_series.rx_unicast_total.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(100))]
        );
        assert_eq!(
            &time_series.rx_unicast_drop.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(5))]
        );
        assert_eq!(
            &time_series.tx_total.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(50))]
        );
        assert_eq!(&time_series.tx_drop.drain_calls()[..], &[TimeMatrixCall::Fold(Timed::now(2))]);
    }

    #[fuchsia::test]
    fn test_handle_iface_destroyed() {
        let mut test_helper = setup_test();
        let time_series = MockIfaceCountersTimeSeries::default();
        let logger = ClientIfaceCountersLogger::new_helper(
            test_helper.monitor_svc_proxy.clone(),
            time_series.build(),
        );

        // Transition to IfaceCreated state
        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Ready(())
        );

        let mut handle_iface_destroyed_fut = pin!(logger.handle_iface_destroyed(IFACE_ID));
        assert_eq!(
            test_helper.exec.run_until_stalled(&mut handle_iface_destroyed_fut),
            Poll::Ready(())
        );

        let is_connected = true;
        let mut test_fut = pin!(logger.handle_periodic_telemetry(is_connected));
        assert_eq!(test_helper.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));
        let telemetry_svc_stream = test_helper.telemetry_svc_stream.as_mut().unwrap();
        let mut telemetry_svc_req_fut = pin!(telemetry_svc_stream.try_next());
        // Verify that no telemetry request is made now that the iface is destroyed
        match test_helper.exec.run_until_stalled(&mut telemetry_svc_req_fut) {
            Poll::Ready(Ok(None)) => (),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[derive(Debug, Default)]
    struct MockIfaceCountersTimeSeries {
        rx_unicast_total: MockTimeMatrix<u64>,
        rx_unicast_drop: MockTimeMatrix<u64>,
        tx_total: MockTimeMatrix<u64>,
        tx_drop: MockTimeMatrix<u64>,
    }

    impl MockIfaceCountersTimeSeries {
        fn build(&self) -> IfaceCountersTimeSeries {
            IfaceCountersTimeSeries {
                rx_unicast_total: self.rx_unicast_total.build_ref("rx_unicast_total"),
                rx_unicast_drop: self.rx_unicast_drop.build_ref("rx_unicast_drop"),
                tx_total: self.tx_total.build_ref("tx_total"),
                tx_drop: self.tx_drop.build_ref("tx_drop"),
            }
        }
    }
}
