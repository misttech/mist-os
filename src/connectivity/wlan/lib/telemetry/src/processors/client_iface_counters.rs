// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_wlan_stats as fidl_stats;
use fuchsia_async::TimeoutExt;
use futures::lock::Mutex;

use log::{error, warn};
use std::collections::HashMap;
use std::sync::Arc;
use windowed_stats::experimental::clock::Timed;
use windowed_stats::experimental::series::interpolation::LastSample;
use windowed_stats::experimental::series::statistic::LatchMax;
use windowed_stats::experimental::series::{SamplingProfile, TimeMatrix};
use windowed_stats::experimental::serve::{InspectedTimeMatrix, TimeMatrixClient};

// Include a timeout on stats calls so that if the driver deadlocks, telemtry doesn't get stuck.
const GET_IFACE_STATS_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);

#[derive(Debug)]
enum IfaceState {
    NotAvailable,
    Created { iface_id: u16, telemetry_proxy: Option<fidl_fuchsia_wlan_sme::TelemetryProxy> },
}

pub struct ClientIfaceCountersLogger {
    iface_state: Arc<Mutex<IfaceState>>,
    monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
    time_series_stats: IfaceCountersTimeSeries,
    driver_inspect_counter_configs: Arc<Mutex<HashMap<u16, String>>>,
}

impl ClientIfaceCountersLogger {
    pub fn new(
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        time_matrix_client: &TimeMatrixClient,
    ) -> Self {
        Self::new_helper(monitor_svc_proxy, IfaceCountersTimeSeries::new(time_matrix_client))
    }

    fn new_helper(
        monitor_svc_proxy: fidl_fuchsia_wlan_device_service::DeviceMonitorProxy,
        time_series_stats: IfaceCountersTimeSeries,
    ) -> Self {
        Self {
            iface_state: Arc::new(Mutex::new(IfaceState::NotAvailable)),
            monitor_svc_proxy,
            time_series_stats,
            driver_inspect_counter_configs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn handle_iface_created(&self, iface_id: u16) {
        let (proxy, server) = fidl::endpoints::create_proxy();
        let telemetry_proxy = match self.monitor_svc_proxy.get_sme_telemetry(iface_id, server).await
        {
            Ok(Ok(())) => {
                let inspect_counter_configs = match proxy.query_telemetry_support().await {
                    Ok(Ok(support)) => support.inspect_counter_configs,
                    Ok(Err(code)) => {
                        warn!("Failed to query telemetry support with status code {}. No driver-specific counters will be captured", code);
                        None
                    }
                    Err(e) => {
                        error!("Failed to query telemetry support with error {}. No driver-specific counters will be captured", e);
                        None
                    }
                };
                {
                    let mut driver_inspect_counter_configs =
                        self.driver_inspect_counter_configs.lock().await;
                    for inspect_counter_config in inspect_counter_configs.unwrap_or(vec![]) {
                        if let fidl_stats::InspectCounterConfig {
                            counter_id: Some(counter_id),
                            counter_name: Some(counter_name),
                            ..
                        } = inspect_counter_config
                        {
                            let _counter_name = driver_inspect_counter_configs
                                .entry(counter_id)
                                .or_insert_with(|| counter_name);
                        }
                    }
                }
                Some(proxy)
            }
            Ok(Err(e)) => {
                error!("Request for SME telemetry for iface {} completed with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
            Err(e) => {
                error!("Failed to request SME telemetry for iface {} with error {}. No telemetry will be captured.", iface_id, e);
                None
            }
        };
        *self.iface_state.lock().await = IfaceState::Created { iface_id, telemetry_proxy }
    }

    pub async fn handle_iface_destroyed(&self, iface_id: u16) {
        let destroyed = matches!(*self.iface_state.lock().await, IfaceState::Created { iface_id: existing_iface_id, .. } if iface_id == existing_iface_id);
        if destroyed {
            *self.iface_state.lock().await = IfaceState::NotAvailable;
        }
    }

    pub async fn handle_periodic_telemetry(&self, is_connected: bool) {
        match &*self.iface_state.lock().await {
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
                            if let Some(fidl_stats::ConnectionCounters {
                                connection_id: Some(_connection_id),
                                rx_unicast_total: Some(rx_unicast_total),
                                rx_unicast_drop: Some(rx_unicast_drop),
                                tx_total: Some(tx_total),
                                tx_drop: Some(tx_drop),
                                ..
                            }) = stats.connection_counters
                            {
                                self.time_series_stats.log_rx_unicast_total(rx_unicast_total);
                                self.time_series_stats.log_rx_unicast_drop(rx_unicast_drop);
                                self.time_series_stats.log_tx_total(tx_total);
                                self.time_series_stats.log_tx_drop(tx_drop);
                            }
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
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let rx_unicast_drop = client.inspect_time_matrix(
            "rx_unicast_drop",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let tx_total = client.inspect_time_matrix(
            "tx_total",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
            ),
        );
        let tx_drop = client.inspect_time_matrix(
            "tx_drop",
            TimeMatrix::<LatchMax<u64>, LastSample>::new(
                SamplingProfile::balanced(),
                LastSample::or(0),
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
    use wlan_common::assert_variant;

    const IFACE_ID: u16 = 66;

    #[fuchsia::test]
    fn test_handle_iface_created() {
        let mut test_helper = setup_test();
        let time_series = MockIfaceCountersTimeSeries::default();
        let logger = ClientIfaceCountersLogger::new_helper(
            test_helper.monitor_svc_proxy.clone(),
            time_series.build(),
        );

        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );

        let mocked_inspect_counter_configs = vec![fidl_stats::InspectCounterConfig {
            counter_id: Some(1),
            counter_name: Some("foo_counter".to_string()),
            ..Default::default()
        }];
        let telemetry_support = fidl_stats::TelemetrySupport {
            inspect_counter_configs: Some(mocked_inspect_counter_configs),
            ..Default::default()
        };
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );

        assert_variant!(logger.iface_state.try_lock().as_deref(), Some(IfaceState::Created { .. }));
        assert_eq!(
            *logger.driver_inspect_counter_configs.try_lock().unwrap(),
            HashMap::from([(1u16, "foo_counter".to_string())])
        );
    }

    #[fuchsia::test]
    fn test_handle_periodic_telemetry() {
        let mut test_helper = setup_test();
        let time_series = MockIfaceCountersTimeSeries::default();
        let logger = ClientIfaceCountersLogger::new_helper(
            test_helper.monitor_svc_proxy.clone(),
            time_series.build(),
        );

        // Transition to IfaceCreated state
        handle_iface_created(&mut test_helper, &logger);

        let is_connected = true;
        let mut test_fut = pin!(logger.handle_periodic_telemetry(is_connected));
        let counter_stats = fidl_stats::IfaceCounterStats {
            connection_counters: Some(fidl_stats::ConnectionCounters {
                connection_id: Some(1),
                rx_unicast_total: Some(100),
                rx_unicast_drop: Some(5),
                rx_multicast: Some(30),
                tx_total: Some(50),
                tx_drop: Some(2),
                ..Default::default()
            }),
            ..Default::default()
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
        handle_iface_created(&mut test_helper, &logger);

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

    fn handle_iface_created(test_helper: &mut TestHelper, logger: &ClientIfaceCountersLogger) {
        let mut handle_iface_created_fut = pin!(logger.handle_iface_created(IFACE_ID));
        assert_eq!(
            test_helper.run_and_handle_get_sme_telemetry(&mut handle_iface_created_fut),
            Poll::Pending
        );
        let telemetry_support = fidl_stats::TelemetrySupport::default();
        assert_eq!(
            test_helper.run_and_respond_query_telemetry_support(
                &mut handle_iface_created_fut,
                Ok(&telemetry_support)
            ),
            Poll::Ready(())
        );
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
